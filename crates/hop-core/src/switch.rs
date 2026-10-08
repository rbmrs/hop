//! `hop switch`: resolve a port query, send the switch, confirm the readback.

use std::fmt;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::caps;
use crate::config::Config;
use crate::monitor::{DdcBackend, Display, Error, Port, ports_for};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    Unknown { query: String, valid: Vec<String> },
    Ambiguous(Vec<String>),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown { query, valid } => {
                write!(
                    f,
                    "unknown port \"{query}\"; valid ports: {}",
                    valid.join(", ")
                )
            }
            Self::Ambiguous(names) => write!(f, "ambiguous port; matches {}", names.join(", ")),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Turns a user query into a VCP 0x60 code. Accepts a raw code ("17",
/// "0x11"), a label or detected name in any case with or without spaces and
/// dashes ("linux", "usb-c"), or a name without its number when only one
/// port matches ("HDMI"). Labels win over detected names.
pub fn resolve_port(ports: &[Port], query: &str) -> Result<u8, ResolveError> {
    if let Some(code) = caps::parse_code(query) {
        return Ok(code);
    }
    if let Some(code) = label_match(ports, query) {
        return Ok(code);
    }
    let q = normalize(query);
    if let Some(port) = ports.iter().find(|p| normalize(&p.name) == q) {
        return Ok(port.code);
    }
    let prefix_of = |name: &str| {
        normalize(name)
            .strip_prefix(&q)
            .is_some_and(|rest| !q.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
    };
    let matches: Vec<&Port> = ports
        .iter()
        .filter(|p| prefix_of(&p.name) || p.label.as_deref().is_some_and(prefix_of))
        .collect();
    match matches.as_slice() {
        [port] => Ok(port.code),
        [] => Err(ResolveError::Unknown {
            query: query.to_string(),
            valid: ports.iter().map(Port::title).collect(),
        }),
        many => Err(ResolveError::Ambiguous(
            many.iter().map(|p| p.title()).collect(),
        )),
    }
}

fn label_match(ports: &[Port], query: &str) -> Option<u8> {
    let q = normalize(query);
    ports
        .iter()
        .find(|p| p.label.as_deref().is_some_and(|l| normalize(l) == q))
        .map(|p| p.code)
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Pause between readback attempts, so a backend that fails fast does not
/// flood the DDC bus.
const POLL_DELAY: Duration = Duration::from_millis(100);

/// What the readback after a switch showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readback {
    Confirmed,
    /// The monitor still reports another input.
    Other(u8),
    /// The monitor never answered in time; this is the last error.
    Unknown(Error),
}

#[derive(Debug, Clone)]
pub struct Switched {
    pub display: Display,
    pub code: u8,
    pub readback: Readback,
}

#[derive(Debug, Clone)]
pub enum SwitchError {
    NoDisplay,
    /// Listing the displays failed.
    Detect(Error),
    Port(ResolveError),
    Ddc {
        display: String,
        error: Error,
    },
}

impl fmt::Display for SwitchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDisplay => write!(f, "no DDC/CI monitor found"),
            Self::Detect(e) => write!(f, "could not list monitors: {e}"),
            Self::Port(e) => write!(f, "{e}"),
            Self::Ddc { display, error } => write!(f, "could not switch {display}: {error}"),
        }
    }
}

impl std::error::Error for SwitchError {}

/// Switches the first monitor to the port named by `query`, then reads the
/// input back for up to `confirm_for`. The monitor sends null replies for
/// about 3 s after a switch, so callers should allow at least that long.
pub fn switch(
    backend: &dyn DdcBackend,
    config: &Config,
    query: &str,
    confirm_for: Duration,
) -> Result<Switched, SwitchError> {
    let ddc_error = |display: &Display| {
        let name = display.name.clone();
        move |error| SwitchError::Ddc {
            display: name,
            error,
        }
    };
    let display = backend
        .list_displays()
        .map_err(SwitchError::Detect)?
        .into_iter()
        .next()
        .ok_or(SwitchError::NoDisplay)?;
    let settings = config.monitor(&display);
    // A raw code or a label needs no capabilities read, which takes about 1 s.
    let quick = caps::parse_code(query).or_else(|| label_match(&ports_for(&[], settings), query));
    let code = match quick {
        Some(code) => code,
        None => {
            let caps = backend
                .capabilities(&display)
                .map_err(ddc_error(&display))?;
            let ports = ports_for(&caps::input_ports(&caps), settings);
            resolve_port(&ports, query).map_err(SwitchError::Port)?
        }
    };
    backend
        .set_input(&display, code)
        .map_err(ddc_error(&display))?;
    let readback = confirm(backend, &display, code, confirm_for);
    Ok(Switched {
        display,
        code,
        readback,
    })
}

fn confirm(backend: &dyn DdcBackend, display: &Display, code: u8, wait: Duration) -> Readback {
    let deadline = Instant::now() + wait;
    loop {
        // Judge only the latest read: an early read can still show the old
        // input before the monitor starts to switch.
        let last = backend.get_input(display);
        if last.as_ref() == Ok(&code) {
            return Readback::Confirmed;
        }
        if Instant::now() >= deadline {
            return match last {
                Ok(got) => Readback::Other(got),
                Err(e) => Readback::Unknown(e),
            };
        }
        sleep(POLL_DELAY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::Port;

    fn port(code: u8, name: &str, label: Option<&str>) -> Port {
        Port {
            code,
            name: name.into(),
            label: label.map(Into::into),
            hidden: false,
            detected: true,
        }
    }

    fn dell_ports() -> Vec<Port> {
        vec![
            port(27, "USB-C", None),
            port(15, "DP 1", None),
            port(17, "HDMI 1", None),
        ]
    }

    fn labelled_ports() -> Vec<Port> {
        vec![
            port(27, "USB-C", Some("MacBook")),
            port(15, "DP 1", None),
            port(17, "HDMI 1", Some("Linux")),
        ]
    }

    #[test]
    fn resolves_labels_in_any_case() {
        assert_eq!(resolve_port(&labelled_ports(), "Linux"), Ok(17));
        assert_eq!(resolve_port(&labelled_ports(), "macbook"), Ok(27));
    }

    #[test]
    fn labelled_ports_still_resolve_by_detected_name() {
        assert_eq!(resolve_port(&labelled_ports(), "hdmi"), Ok(17));
    }

    #[test]
    fn a_label_wins_over_another_ports_detected_name() {
        let ports = vec![port(27, "USB-C", Some("HDMI 1")), port(17, "HDMI 1", None)];
        assert_eq!(resolve_port(&ports, "HDMI 1"), Ok(27));
    }

    #[test]
    fn unknown_name_lists_labels_with_detected_names() {
        let err = resolve_port(&labelled_ports(), "vga").unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown port \"vga\"; valid ports: MacBook (USB-C), DP 1, Linux (HDMI 1)"
        );
    }

    #[test]
    fn resolves_full_names_ignoring_case_spaces_and_dashes() {
        let ports = dell_ports();
        assert_eq!(resolve_port(&ports, "HDMI 1"), Ok(17));
        assert_eq!(resolve_port(&ports, "hdmi1"), Ok(17));
        assert_eq!(resolve_port(&ports, "usb-c"), Ok(27));
        assert_eq!(resolve_port(&ports, "USBC"), Ok(27));
    }

    #[test]
    fn resolves_a_name_without_its_number_when_only_one_port_matches() {
        assert_eq!(resolve_port(&dell_ports(), "HDMI"), Ok(17));
        assert_eq!(resolve_port(&dell_ports(), "dp"), Ok(15));
    }

    #[test]
    fn resolves_raw_decimal_and_hex_codes() {
        assert_eq!(resolve_port(&dell_ports(), "17"), Ok(17));
        assert_eq!(resolve_port(&dell_ports(), "0x11"), Ok(17));
        // Raw codes are allowed even when the capabilities string omits them.
        assert_eq!(resolve_port(&dell_ports(), "18"), Ok(18));
    }

    #[test]
    fn ambiguous_name_lists_the_candidates() {
        let mut ports = dell_ports();
        ports.push(port(18, "HDMI 2", None));
        assert_eq!(
            resolve_port(&ports, "hdmi"),
            Err(ResolveError::Ambiguous(vec![
                "HDMI 1".into(),
                "HDMI 2".into()
            ]))
        );
    }

    #[test]
    fn unknown_name_lists_the_valid_ports() {
        let err = resolve_port(&dell_ports(), "vga").unwrap_err();
        assert_eq!(
            err.to_string(),
            "unknown port \"vga\"; valid ports: USB-C, DP 1, HDMI 1"
        );
    }

    mod flow {
        use super::super::*;
        use crate::config::Config;
        use crate::ddc::ReplyError;
        use crate::monitor::{DdcBackend, Display, Error};
        use std::cell::RefCell;
        use std::collections::VecDeque;
        use std::time::Duration;

        const CAPS: &str = "(vcp(60(1B 0F 11)))";
        const NULL: Error = Error::Reply(ReplyError::Null);

        struct FakeBackend {
            displays: Vec<Display>,
            set_result: Result<(), Error>,
            readbacks: RefCell<VecDeque<Result<u8, Error>>>,
            sent: RefCell<Vec<u8>>,
            caps_reads: RefCell<usize>,
        }

        impl FakeBackend {
            fn new(readbacks: Vec<Result<u8, Error>>) -> Self {
                Self {
                    displays: vec![Display {
                        name: "DELL U3223QE".into(),
                        serial: None,
                    }],
                    set_result: Ok(()),
                    readbacks: RefCell::new(readbacks.into()),
                    sent: RefCell::new(Vec::new()),
                    caps_reads: RefCell::new(0),
                }
            }
        }

        impl DdcBackend for FakeBackend {
            fn list_displays(&self) -> Result<Vec<Display>, Error> {
                Ok(self.displays.clone())
            }
            fn capabilities(&self, _: &Display) -> Result<String, Error> {
                *self.caps_reads.borrow_mut() += 1;
                Ok(CAPS.into())
            }
            /// Pops readbacks in order and repeats the last one forever.
            fn get_input(&self, _: &Display) -> Result<u8, Error> {
                let mut queue = self.readbacks.borrow_mut();
                if queue.len() > 1 {
                    queue.pop_front().unwrap()
                } else {
                    queue.front().cloned().unwrap_or(Err(NULL))
                }
            }
            fn set_input(&self, _: &Display, code: u8) -> Result<(), Error> {
                self.sent.borrow_mut().push(code);
                self.set_result.clone()
            }
        }

        const WAIT: Duration = Duration::from_millis(50);

        #[test]
        fn sends_the_resolved_code_and_confirms_through_null_replies() {
            let backend = FakeBackend::new(vec![Err(NULL), Err(NULL), Err(NULL), Ok(17)]);
            // Long enough for several polls; returns as soon as the input confirms.
            let done =
                switch(&backend, &Config::default(), "HDMI", Duration::from_secs(5)).unwrap();
            assert_eq!(*backend.sent.borrow(), vec![17]);
            assert_eq!(done.code, 17);
            assert_eq!(done.readback, Readback::Confirmed);
        }

        #[test]
        fn unknown_port_sends_nothing() {
            let backend = FakeBackend::new(vec![]);
            let err = switch(&backend, &Config::default(), "vga", WAIT).unwrap_err();
            assert!(matches!(
                err,
                SwitchError::Port(ResolveError::Unknown { .. })
            ));
            assert!(backend.sent.borrow().is_empty());
        }

        #[test]
        fn failed_send_is_an_error() {
            let mut backend = FakeBackend::new(vec![]);
            backend.set_result = Err(Error::Transport("I2C write failed".into()));
            let err = switch(&backend, &Config::default(), "17", WAIT).unwrap_err();
            assert_eq!(
                err.to_string(),
                "could not switch DELL U3223QE: I2C write failed"
            );
        }

        #[test]
        fn readback_of_another_input_is_reported() {
            let backend = FakeBackend::new(vec![Ok(27)]);
            assert_eq!(
                switch(&backend, &Config::default(), "17", WAIT)
                    .unwrap()
                    .readback,
                Readback::Other(27)
            );
        }

        #[test]
        fn old_input_read_before_the_switch_starts_is_not_a_failure() {
            let backend = FakeBackend::new(vec![Ok(27), Err(NULL)]);
            assert_eq!(
                switch(&backend, &Config::default(), "17", WAIT)
                    .unwrap()
                    .readback,
                Readback::Unknown(NULL)
            );
        }

        #[test]
        fn readback_that_never_answers_is_unconfirmed_not_an_error() {
            let backend = FakeBackend::new(vec![]);
            assert_eq!(
                switch(&backend, &Config::default(), "17", WAIT)
                    .unwrap()
                    .readback,
                Readback::Unknown(NULL)
            );
        }

        #[test]
        fn no_display_is_an_error() {
            let mut backend = FakeBackend::new(vec![]);
            backend.displays.clear();
            assert!(matches!(
                switch(&backend, &Config::default(), "17", WAIT),
                Err(SwitchError::NoDisplay)
            ));
        }

        #[test]
        fn switches_by_label_without_reading_capabilities() {
            let backend = FakeBackend::new(vec![Ok(17)]);
            let config = Config::defaults_for(&backend.displays[0]);
            let done = switch(&backend, &config, "Linux", WAIT).unwrap();
            assert_eq!(*backend.sent.borrow(), vec![17]);
            assert_eq!(done.readback, Readback::Confirmed);
            assert_eq!(*backend.caps_reads.borrow(), 0);
        }
    }
}
