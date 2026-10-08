//! Core of Hop: DDC/CI input switching, independent of the UI.

pub mod caps;
pub mod config;
pub mod ddc;
pub mod monitor;
pub mod switch;

#[cfg(target_os = "macos")]
pub mod macos;

pub use monitor::{DdcBackend, Display, Error, Monitor, list_monitors};
