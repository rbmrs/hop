//! Displays, the DDC backend trait, and the monitor listing behind `hop list`.

use std::fmt;

use crate::caps::{self, InputPort};
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

/// A display with its detected input ports and the active input.
#[derive(Debug, Clone)]
pub struct Monitor {
    pub display: Display,
    pub ports: Vec<InputPort>,
    pub active: Result<u8, Error>,
}

/// Reads every display's ports and active input.
pub fn list_monitors(backend: &dyn DdcBackend) -> Result<Vec<Monitor>, Error> {
    backend
        .list_displays()?
        .into_iter()
        .map(|display| {
            let mut ports = caps::input_ports(&backend.capabilities(&display)?);
            let active = backend.get_input(&display);
            if let Ok(code) = active
                && !ports.iter().any(|p| p.code == code)
            {
                ports.push(InputPort {
                    code,
                    name: caps::port_name(code),
                });
            }
            Ok(Monitor {
                display,
                ports,
                active,
            })
        })
        .collect()
}

impl fmt::Display for Monitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.display.serial {
            Some(serial) => writeln!(f, "{} ({serial})", self.display.name)?,
            None => writeln!(f, "{}", self.display.name)?,
        }
        let width = self.ports.iter().map(|p| p.name.len()).max().unwrap_or(0) + 2;
        for port in &self.ports {
            let mark = if self.active.as_ref() == Ok(&port.code) {
                '*'
            } else {
                ' '
            };
            writeln!(f, "  {mark} {:<width$}{}", port.name, port.code)?;
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
        list_monitors(backend)
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
        assert!(list_monitors(&backend).unwrap().is_empty());
    }
}
