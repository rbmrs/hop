//! Which global hotkey switches to which port, from the monitor's config.

use crate::caps;
use crate::config::{MonitorConfig, PortConfig};

/// A global hotkey and the VCP 0x60 code it switches to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// As written in the config, e.g. "Ctrl+Alt+Cmd+Equal".
    pub hotkey: String,
    pub code: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bindings {
    pub bindings: Vec<Binding>,
    /// One message per hotkey that was skipped because another port uses it.
    pub conflicts: Vec<String>,
}

/// Bindings in config order. When two ports share a hotkey (in any case or
/// key order), the first keeps it.
pub fn bindings(config: Option<&MonitorConfig>) -> Bindings {
    let mut out = Bindings::default();
    let mut owners: Vec<(String, &PortConfig)> = Vec::new();
    for port in config.map_or(&[][..], |m| &m.ports[..]) {
        let Some(hotkey) = &port.hotkey else { continue };
        let key = normalize(hotkey);
        if let Some((_, owner)) = owners.iter().find(|(k, _)| *k == key) {
            out.conflicts.push(format!(
                "{hotkey} on {} is already used by {}",
                port_title(port),
                port_title(owner)
            ));
            continue;
        }
        owners.push((key, port));
        out.bindings.push(Binding {
            hotkey: hotkey.clone(),
            code: port.code,
        });
    }
    out
}

/// The port's label, or its detected name.
pub(crate) fn port_title(port: &PortConfig) -> String {
    port.label
        .clone()
        .unwrap_or_else(|| caps::port_name(port.code))
}

/// Whether two hotkey strings mean the same keys.
pub fn same_hotkey(a: &str, b: &str) -> bool {
    normalize(a) == normalize(b)
}

/// Same hotkey regardless of case, key order, and modifier aliases
/// (the names the global-hotkey parser accepts).
fn normalize(hotkey: &str) -> String {
    let mut keys: Vec<String> = hotkey
        .split('+')
        .map(|k| {
            let k = k.trim().to_lowercase();
            match k.as_str() {
                "control" => "ctrl".into(),
                "option" => "alt".into(),
                "command" | "super" => "cmd".into(),
                // Key codes ("KeyA", "Digit0") name the same keys as "A", "0".
                _ => match k.strip_prefix("key").or_else(|| k.strip_prefix("digit")) {
                    Some(rest) if rest.len() == 1 => rest.to_string(),
                    _ => k,
                },
            }
        })
        .collect();
    keys.sort();
    keys.join("+")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn monitor_config(toml: &str) -> MonitorConfig {
        Config::parse(toml).unwrap().monitors.remove(0)
    }

    #[test]
    fn default_config_binds_both_hotkeys() {
        let config = Config::defaults_for(&crate::monitor::Display {
            name: "DELL U3223QE".into(),
            serial: None,
        });
        let b = bindings(config.monitors.first());
        assert_eq!(
            b.bindings,
            vec![
                Binding {
                    hotkey: "Ctrl+Alt+Cmd+Equal".into(),
                    code: 27
                },
                Binding {
                    hotkey: "Ctrl+Alt+Cmd+Minus".into(),
                    code: 17
                },
            ]
        );
        assert!(b.conflicts.is_empty());
    }

    #[test]
    fn ports_without_a_hotkey_are_skipped() {
        let m = monitor_config(
            "[[monitor]]\nmodel = \"M\"\n[[monitor.port]]\ncode = 15\n[[monitor.port]]\ncode = 17\nhotkey = \"Ctrl+Alt+Cmd+Minus\"\n",
        );
        assert_eq!(
            bindings(Some(&m)).bindings,
            vec![Binding {
                hotkey: "Ctrl+Alt+Cmd+Minus".into(),
                code: 17
            }]
        );
    }

    #[test]
    fn a_hotkey_on_two_ports_keeps_the_first_and_reports_the_second() {
        let m = monitor_config(
            "[[monitor]]\nmodel = \"M\"\n[[monitor.port]]\ncode = 27\nlabel = \"MacBook\"\nhotkey = \"Ctrl+Alt+Cmd+Equal\"\n[[monitor.port]]\ncode = 17\nlabel = \"Linux\"\nhotkey = \"cmd+ctrl+alt+equal\"\n",
        );
        let b = bindings(Some(&m));
        assert_eq!(
            b.bindings,
            vec![Binding {
                hotkey: "Ctrl+Alt+Cmd+Equal".into(),
                code: 27
            }]
        );
        assert_eq!(
            b.conflicts,
            vec!["cmd+ctrl+alt+equal on Linux is already used by MacBook".to_string()]
        );
    }

    #[test]
    fn no_config_means_no_bindings() {
        assert_eq!(bindings(None), Bindings::default());
    }

    #[test]
    fn modifier_aliases_count_as_the_same_hotkey() {
        let m = monitor_config(
            "[[monitor]]\nmodel = \"M\"\n[[monitor.port]]\ncode = 27\nhotkey = \"Ctrl+Alt+Cmd+Equal\"\n[[monitor.port]]\ncode = 17\nhotkey = \"Control+Option+Command+Equal\"\n",
        );
        assert_eq!(bindings(Some(&m)).conflicts.len(), 1);
    }

    #[test]
    fn key_code_names_match_their_short_names() {
        assert!(same_hotkey("Ctrl+KeyA", "ctrl+a"));
        assert!(same_hotkey("Cmd+Digit0", "Command+0"));
        assert!(!same_hotkey("Ctrl+KeyA", "Ctrl+B"));
    }
}
