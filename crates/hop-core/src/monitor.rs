//! Displays, the DDC backend trait, and the monitor listing behind `hop list`.

use std::fmt;

use crate::caps::{self, InputPort};
use crate::config::{Config, MonitorConfig};
use crate::ddc::ReplyError;

/// A monitor reachable over DDC/CI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    /// Product name from the EDID, e.g. "DELL U3223QE".
    pub name: String,
    /// Serial from the EDID, when the monitor reports one.
    pub serial: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The OS-level I2C transfer failed.
    Transport(String),
    /// Every retry got an invalid reply; this is the last one.
    Reply(ReplyError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(msg) => write!(f, "{msg}"),
            Self::Reply(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Platform access to DDC/CI monitors.
pub trait DdcBackend {
    fn list_displays(&self) -> Result<Vec<Display>, Error>;
    fn capabilities(&self, display: &Display) -> Result<String, Error>;
    /// Current VCP 0x60 input code.
    fn get_input(&self, display: &Display) -> Result<u8, Error>;
    fn set_input(&self, display: &Display, code: u8) -> Result<(), Error>;
}

/// An input port: what the monitor reports, plus the user's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    /// VCP 0x60 value.
    pub code: u8,
    /// Detected name, e.g. "HDMI 1".
    pub name: String,
    pub label: Option<String>,
    pub hidden: bool,
}

impl Port {
    /// "Linux" with a label, else "HDMI 1".
    pub fn label_or_name(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.name)
    }

    /// "Linux (HDMI 1)" with a label, else "HDMI 1".
    pub fn title(&self) -> String {
        match &self.label {
            Some(label) => format!("{label} ({})", self.name),
            None => self.name.clone(),
        }
    }
}

/// Merges detected ports with the monitor's settings. Configured ports the
/// capabilities string omits are kept, after the detected ones.
pub fn ports_for(detected: &[InputPort], config: Option<&MonitorConfig>) -> Vec<Port> {
    let settings = config.map_or(&[][..], |m| &m.ports[..]);
    let setting = |code| settings.iter().find(|p| p.code == code);
    let mut ports: Vec<Port> = detected
        .iter()
        .map(|d| Port {
            code: d.code,
            name: d.name.clone(),
            label: setting(d.code).and_then(|s| s.label.clone()),
            hidden: setting(d.code).is_some_and(|s| s.hidden),
        })
        .collect();
    for s in settings {
        if !ports.iter().any(|p| p.code == s.code) {
            ports.push(Port {
                code: s.code,
                name: caps::port_name(s.code),
                label: s.label.clone(),
                hidden: s.hidden,
            });
        }
    }
    ports
}

/// A display with its ports and the active input.
#[derive(Debug, Clone)]
pub struct Monitor {
    pub display: Display,
    pub ports: Vec<Port>,
    pub active: Result<u8, Error>,
}

/// Reads every display's ports and active input, applying the config.
pub fn list_monitors(backend: &dyn DdcBackend, config: &Config) -> Result<Vec<Monitor>, Error> {
    backend
        .list_displays()?
        .into_iter()
        .map(|display| {
            let mut detected = caps::input_ports(&backend.capabilities(&display)?);
            let active = backend.get_input(&display);
            if let Ok(code) = active
                && !detected.iter().any(|p| p.code == code)
            {
                detected.push(InputPort {
                    code,
                    name: caps::port_name(code),
                });
            }
            let ports = ports_for(&detected, config.monitor(&display));
            Ok(Monitor {
                display,
                ports,
                active,
            })
        })
        .collect()
}

impl Monitor {
    pub fn is_active(&self, code: u8) -> bool {
        self.active.as_ref() == Ok(&code)
    }

    /// Ports that are not hidden. A hidden port still shows while it is
    /// active, so the active input is always visible.
    pub fn visible_ports(&self) -> impl Iterator<Item = &Port> {
        self.ports
            .iter()
            .filter(|p| !p.hidden || self.is_active(p.code))
    }
}

impl fmt::Display for Monitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.display.serial {
            Some(serial) => writeln!(f, "{} ({serial})", self.display.name)?,
            None => writeln!(f, "{}", self.display.name)?,
        }
        let visible: Vec<&Port> = self.visible_ports().collect();
        let width = visible.iter().map(|p| p.title().len()).max().unwrap_or(0) + 2;
        for port in visible {
            let mark = if self.is_active(port.code) { '*' } else { ' ' };
            writeln!(f, "  {mark} {:<width$}{}", port.title(), port.code)?;
        }
        if let Err(e) = &self.active {
            writeln!(f, "  active input unknown: {e}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddc::ReplyError;

    const CAPS: &str =
        "(prot(monitor)type(LCD)model(U3223QE)vcp(02 10 60(1B 0F 11 ) D6(01 04 05))mccs_ver(2.1))";

    struct FakeBackend {
        displays: Vec<Display>,
        input: Result<u8, Error>,
    }

    impl DdcBackend for FakeBackend {
        fn list_displays(&self) -> Result<Vec<Display>, Error> {
            Ok(self.displays.clone())
        }
        fn capabilities(&self, _: &Display) -> Result<String, Error> {
            Ok(CAPS.to_string())
        }
        fn get_input(&self, _: &Display) -> Result<u8, Error> {
            self.input.clone()
        }
        fn set_input(&self, _: &Display, _: u8) -> Result<(), Error> {
            unreachable!("listing never switches")
        }
    }

    fn dell() -> Display {
        Display {
            name: "DELL U3223QE".into(),
            serial: Some("9CY9834".into()),
        }
    }

    fn render(backend: &FakeBackend) -> String {
        render_with(backend, &Config::default())
    }

    fn render_with(backend: &FakeBackend, config: &Config) -> String {
        list_monitors(backend, config)
            .unwrap()
            .iter()
            .map(|m| m.to_string())
            .collect()
    }

    #[test]
    fn lists_ports_and_marks_the_active_one() {
        let backend = FakeBackend {
            displays: vec![dell()],
            input: Ok(27),
        };
        assert_eq!(
            render(&backend),
            "DELL U3223QE (9CY9834)\n  * USB-C   27\n    DP 1    15\n    HDMI 1  17\n"
        );
    }

    #[test]
    fn unreadable_active_input_still_lists_ports() {
        let backend = FakeBackend {
            displays: vec![dell()],
            input: Err(Error::Reply(ReplyError::Null)),
        };
        assert_eq!(
            render(&backend),
            "DELL U3223QE (9CY9834)\n    USB-C   27\n    DP 1    15\n    HDMI 1  17\n  active input unknown: display sent a null reply\n"
        );
    }

    #[test]
    fn active_input_missing_from_capabilities_is_still_shown() {
        let backend = FakeBackend {
            displays: vec![dell()],
            input: Ok(0x12),
        };
        assert_eq!(
            render(&backend),
            "DELL U3223QE (9CY9834)\n    USB-C   27\n    DP 1    15\n    HDMI 1  17\n  * HDMI 2  18\n"
        );
    }

    #[test]
    fn no_displays_gives_an_empty_list() {
        let backend = FakeBackend {
            displays: vec![],
            input: Ok(27),
        };
        assert!(
            list_monitors(&backend, &Config::default())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn labels_are_shown_with_the_detected_name() {
        let backend = FakeBackend {
            displays: vec![dell()],
            input: Ok(27),
        };
        assert_eq!(
            render_with(&backend, &Config::defaults_for(&dell())),
            "DELL U3223QE (9CY9834)\n  * MacBook (USB-C)  27\n    DP 1             15\n    Linux (HDMI 1)   17\n"
        );
    }

    #[test]
    fn hidden_ports_are_left_out() {
        let backend = FakeBackend {
            displays: vec![dell()],
            input: Ok(27),
        };
        let config = Config::parse(
            "[[monitor]]\nmodel = \"DELL U3223QE\"\n[[monitor.port]]\ncode = 15\nhidden = true\n",
        )
        .unwrap();
        assert_eq!(
            render_with(&backend, &config),
            "DELL U3223QE (9CY9834)\n  * USB-C   27\n    HDMI 1  17\n"
        );
    }

    #[test]
    fn configured_port_missing_from_capabilities_is_listed() {
        let backend = FakeBackend {
            displays: vec![dell()],
            input: Ok(27),
        };
        let config = Config::parse(
            "[[monitor]]\nmodel = \"DELL U3223QE\"\n[[monitor.port]]\ncode = 16\nlabel = \"Spare\"\n",
        )
        .unwrap();
        assert!(render_with(&backend, &config).ends_with("    Spare (DP 2)  16\n"));
    }

    #[test]
    fn a_hidden_port_is_still_shown_while_it_is_active() {
        let backend = FakeBackend {
            displays: vec![dell()],
            input: Ok(15),
        };
        let config = Config::parse(
            "[[monitor]]\nmodel = \"DELL U3223QE\"\n[[monitor.port]]\ncode = 15\nhidden = true\n",
        )
        .unwrap();
        assert!(render_with(&backend, &config).contains("  * DP 1    15\n"));
    }
}
