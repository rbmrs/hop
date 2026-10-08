//! The tray menu model: which ports to show, under which titles, and which
//! one is checked. Pure, so the UI layer only draws it.

use crate::monitor::Monitor;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuEntry {
    /// VCP 0x60 value to switch to on click.
    pub code: u8,
    /// The label, or the detected name when there is no label.
    pub title: String,
    pub checked: bool,
}

pub fn port_entries(monitor: &Monitor) -> Vec<MenuEntry> {
    monitor
        .visible_ports()
        .map(|p| MenuEntry {
            code: p.code,
            title: p.label_or_name().to_string(),
            checked: monitor.is_active(p.code),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ddc::ReplyError;
    use crate::monitor::{Display, Error, Monitor, Port};

    fn port(code: u8, name: &str, label: Option<&str>, hidden: bool) -> Port {
        Port {
            code,
            name: name.into(),
            label: label.map(Into::into),
            hidden,
        }
    }

    fn monitor(active: Result<u8, Error>, ports: Vec<Port>) -> Monitor {
        Monitor {
            display: Display {
                name: "DELL U3223QE".into(),
                serial: None,
            },
            ports,
            active,
        }
    }

    fn entries(m: &Monitor) -> Vec<(u8, String, bool)> {
        port_entries(m)
            .into_iter()
            .map(|e| (e.code, e.title, e.checked))
            .collect()
    }

    #[test]
    fn shows_labels_and_checks_the_active_port() {
        let m = monitor(
            Ok(17),
            vec![
                port(27, "USB-C", Some("MacBook"), false),
                port(15, "DP 1", None, false),
                port(17, "HDMI 1", Some("Linux"), false),
            ],
        );
        assert_eq!(
            entries(&m),
            vec![
                (27, "MacBook".into(), false),
                (15, "DP 1".into(), false),
                (17, "Linux".into(), true),
            ]
        );
    }

    #[test]
    fn hidden_ports_are_left_out_unless_active() {
        let ports = vec![port(27, "USB-C", None, false), port(15, "DP 1", None, true)];
        assert_eq!(
            entries(&monitor(Ok(27), ports.clone())),
            vec![(27, "USB-C".into(), true)]
        );
        assert_eq!(entries(&monitor(Ok(15), ports)).len(), 2);
    }

    #[test]
    fn unknown_active_input_checks_nothing() {
        let m = monitor(
            Err(Error::Reply(ReplyError::Null)),
            vec![port(27, "USB-C", None, false)],
        );
        assert_eq!(entries(&m), vec![(27, "USB-C".into(), false)]);
    }
}
