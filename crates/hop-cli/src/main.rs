//! `hop` command-line interface.

use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use hop_core::config::{self, Config};
use hop_core::switch::{Readback, switch};
use hop_core::{DdcBackend, list_monitors};

#[derive(Parser)]
#[command(version, about = "Switch a monitor's input over DDC/CI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List monitors, their input ports, and the active input (*).
    List,
    /// Switch the monitor to a port: a name ("HDMI", "usb-c") or a raw VCP 0x60 code ("17", "0x11").
    Switch { port: String },
}

/// The monitor sends null replies for about 3 s after a switch.
const CONFIRM_FOR: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    let cli = Cli::parse();
    let backend = backend();
    let config = match load_config(&backend) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("hop: {e}");
            return ExitCode::FAILURE;
        }
    };
    match cli.command {
        Command::List => list(&backend, &config),
        Command::Switch { port } => switch_to(&backend, &config, &port),
    }
}

/// Loads the config; on first run, writes defaults for the detected monitor.
fn load_config(backend: &dyn DdcBackend) -> Result<Config, config::ConfigError> {
    // If detection fails here, the command itself reports it.
    let displays = backend.list_displays().unwrap_or_default();
    config::load_or_create(&config::default_path(), &displays)
}

fn switch_to(backend: &dyn DdcBackend, config: &Config, port: &str) -> ExitCode {
    match switch(backend, config, port, CONFIRM_FOR) {
        Ok(done) => match done.readback {
            Readback::Confirmed => ExitCode::SUCCESS,
            Readback::Unknown(e) => {
                eprintln!(
                    "hop: switched to {}, but could not confirm it: {e}",
                    done.code
                );
                ExitCode::SUCCESS
            }
            Readback::Other(got) => {
                eprintln!(
                    "hop: sent input {}, but the monitor reports input {got}",
                    done.code
                );
                ExitCode::FAILURE
            }
        },
        Err(e) => {
            eprintln!("hop: {e}");
            ExitCode::FAILURE
        }
    }
}

fn list(backend: &dyn DdcBackend, config: &Config) -> ExitCode {
    match list_monitors(backend, config) {
        Ok(monitors) if monitors.is_empty() => {
            eprintln!("No DDC/CI monitor found.");
            ExitCode::FAILURE
        }
        Ok(monitors) => {
            for monitor in monitors {
                print!("{monitor}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("hop: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "macos")]
fn backend() -> hop_core::macos::MacBackend {
    hop_core::macos::MacBackend::new()
}
