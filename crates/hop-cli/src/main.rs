//! `hop` command-line interface.

use std::process::ExitCode;

use clap::{Parser, Subcommand};
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
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let backend = backend();
    match cli.command {
        Command::List => list(&backend),
    }
}

fn list(backend: &dyn DdcBackend) -> ExitCode {
    match list_monitors(backend) {
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
