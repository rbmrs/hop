//! The config file: per-monitor port labels, hidden ports and hotkeys.
//! One TOML schema on every OS, so the file can be copied between machines.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::monitor::Display;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(rename = "monitor", default)]
    pub monitors: Vec<MonitorConfig>,
}

/// Settings for one monitor, matched by model and serial (not a volatile
/// OS display ID).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorConfig {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    #[serde(rename = "port", default)]
    pub ports: Vec<PortConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortConfig {
    /// VCP 0x60 value.
    pub code: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkey: Option<String>,
}

#[derive(Debug)]
pub struct ConfigError {
    pub path: PathBuf,
    pub kind: ConfigErrorKind,
}

#[derive(Debug)]
pub enum ConfigErrorKind {
    Io(io::Error),
    Parse(toml::de::Error),
    Serialize(toml::ser::Error),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.path.display();
        match &self.kind {
            ConfigErrorKind::Io(e) => write!(f, "config {path}: {e}"),
            ConfigErrorKind::Parse(e) => write!(f, "config {path} is invalid: {e}"),
            ConfigErrorKind::Serialize(e) => write!(f, "config {path} could not be written: {e}"),
        }
    }
}

impl ConfigError {
    fn new(path: &Path, kind: ConfigErrorKind) -> Self {
        Self {
            path: path.to_path_buf(),
            kind,
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// First-run defaults: USB-C → "MacBook" (⌃⌥⌘=), HDMI 1 → "Linux" (⌃⌥⌘-).
    pub fn defaults_for(display: &Display) -> Self {
        let port = |code, label: &str, hotkey: &str| PortConfig {
            code,
            label: Some(label.into()),
            hidden: false,
            hotkey: Some(hotkey.into()),
        };
        Self {
            monitors: vec![MonitorConfig {
                model: display.name.clone(),
                serial: display.serial.clone(),
                ports: vec![
                    port(0x1B, "MacBook", "Ctrl+Alt+Cmd+Equal"),
                    port(0x11, "Linux", "Ctrl+Alt+Cmd+Minus"),
                ],
            }],
        }
    }

    /// The settings for a display: an exact serial match wins, then an entry
    /// for the same model with no serial, then any entry for the same model
    /// (another OS may report the serial in another form, or not at all).
    pub fn monitor(&self, display: &Display) -> Option<&MonitorConfig> {
        let same_model: Vec<&MonitorConfig> = self
            .monitors
            .iter()
            .filter(|m| m.model == display.name)
            .collect();
        let serial_match = same_model
            .iter()
            .find(|m| m.serial.is_some() && m.serial == display.serial);
        serial_match
            .or_else(|| same_model.iter().find(|m| m.serial.is_none()))
            .or(same_model.first())
            .copied()
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self)
            .map_err(|e| ConfigError::new(path, ConfigErrorKind::Serialize(e)))?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| ConfigError::new(path, ConfigErrorKind::Io(e)))?;
        }
        fs::write(path, text).map_err(|e| ConfigError::new(path, ConfigErrorKind::Io(e)))
    }
}

/// `$HOP_CONFIG` if set, else `hop/config.toml` in the platform config dir
/// (`~/Library/Application Support` on macOS, `$XDG_CONFIG_HOME` on Linux).
pub fn default_path() -> PathBuf {
    if let Some(path) = std::env::var_os("HOP_CONFIG") {
        return PathBuf::from(path);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hop")
        .join("config.toml")
}

/// Loads the config. On first run, writes defaults for the first detected
/// display; with no display detected, returns an empty config and writes
/// nothing.
pub fn load_or_create(path: &Path, displays: &[Display]) -> Result<Config, ConfigError> {
    match fs::read_to_string(path) {
        Ok(text) => {
            Config::parse(&text).map_err(|e| ConfigError::new(path, ConfigErrorKind::Parse(e)))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let Some(display) = displays.first() else {
                return Ok(Config::default());
            };
            let config = Config::defaults_for(display);
            config.save(path)?;
            Ok(config)
        }
        Err(e) => Err(ConfigError::new(path, ConfigErrorKind::Io(e))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitor::Display;

    fn dell() -> Display {
        Display {
            name: "DELL U3223QE".into(),
            serial: Some("9CY9834".into()),
        }
    }

    #[test]
    fn first_run_writes_the_default_labels_and_hotkeys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hop").join("config.toml");

        let config = load_or_create(&path, &[dell()]).unwrap();

        assert!(path.exists());
        let monitor = config.monitor(&dell()).unwrap();
        let ports: Vec<(u8, Option<&str>, Option<&str>)> = monitor
            .ports
            .iter()
            .map(|p| (p.code, p.label.as_deref(), p.hotkey.as_deref()))
            .collect();
        assert_eq!(
            ports,
            vec![
                (27, Some("MacBook"), Some("Ctrl+Alt+Cmd+Equal")),
                (17, Some("Linux"), Some("Ctrl+Alt+Cmd+Minus")),
            ]
        );
    }

    #[test]
    fn existing_file_is_loaded_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[[monitor]]\nmodel = \"DELL U3223QE\"\n\n[[monitor.port]]\ncode = 15\nlabel = \"Desk PC\"\nhidden = true\n",
        )
        .unwrap();

        let config = load_or_create(&path, &[dell()]).unwrap();

        let port = &config.monitor(&dell()).unwrap().ports[0];
        assert_eq!(
            (port.code, port.label.as_deref(), port.hidden),
            (15, Some("Desk PC"), true)
        );
    }

    #[test]
    fn save_then_load_gives_the_same_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut config = Config::defaults_for(&dell());
        config.monitors[0].ports[1].hidden = true;

        config.save(&path).unwrap();

        assert_eq!(load_or_create(&path, &[]).unwrap(), config);
    }

    #[test]
    fn broken_file_gives_an_error_with_path_and_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[[monitor]]\nmodel = \"DELL\"\n[[monitor.port]]\ncode = \"twenty\"\n",
        )
        .unwrap();

        let msg = load_or_create(&path, &[dell()]).unwrap_err().to_string();

        assert!(msg.contains(&path.display().to_string()), "{msg}");
        assert!(msg.contains("line 4"), "{msg}");
    }

    #[test]
    fn monitors_match_by_serial_before_model() {
        let config = Config::parse(
            "[[monitor]]\nmodel = \"DELL U3223QE\"\nserial = \"OTHER\"\n\n[[monitor]]\nmodel = \"DELL U3223QE\"\nserial = \"9CY9834\"\n[[monitor.port]]\ncode = 27\n",
        )
        .unwrap();
        assert_eq!(config.monitor(&dell()).unwrap().ports.len(), 1);
    }

    #[test]
    fn no_monitor_detected_on_first_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert_eq!(load_or_create(&path, &[]).unwrap(), Config::default());
        assert!(!path.exists());
    }

    #[test]
    fn a_serial_this_machine_does_not_report_still_matches_by_model() {
        // The config came from the Mac; Linux may report no serial or another form.
        let config = Config::defaults_for(&dell());
        let no_serial = Display {
            name: "DELL U3223QE".into(),
            serial: None,
        };
        let other_serial = Display {
            name: "DELL U3223QE".into(),
            serial: Some("1095840844".into()),
        };
        assert!(config.monitor(&no_serial).is_some());
        assert!(config.monitor(&other_serial).is_some());
    }
}
