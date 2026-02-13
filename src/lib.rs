#[cfg(any(windows, target_os = "linux"))]
mod icon;
pub mod tray;

pub use tray::{TrayEvent, TrayItem, TrayMenuItem, TrayToggleType};
