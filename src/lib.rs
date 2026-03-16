#[cfg(any(windows, target_os = "linux"))]
mod icon;
pub mod tray;

pub use tray::{
    TrayClickAction, TrayClickKind, TrayClickPolicy, TrayEvent, TrayItem, TrayMenuItem,
    TrayMenuItemRole, TrayToggleType,
};
