use gpui::{App, AsyncApp, MouseButton, Point};

/// An icon displayed in a tray menu.
#[derive(Clone, Debug)]
pub enum TrayIcon {
    /// Freedesktop/AppKit/Win32 theme icon name (platform dependent).
    Name(String),
    /// ARGB32 icon bytes.
    Image {
        width: u32,
        height: u32,
        bytes: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum TrayToggleType {
    Checkbox(bool),
    Radio(bool),
}

/// Item used to describe a tray context menu.
#[derive(Clone, Debug)]
pub enum TrayMenuItem {
    Separator { label: Option<String> },
    Submenu {
        id: String,
        label: String,
        toggle_type: Option<TrayToggleType>,
        children: Vec<TrayMenuItem>,
    },
}

impl TrayMenuItem {
    pub fn separator() -> Self {
        Self::Separator { label: None }
    }

    pub fn labeled_separator(label: impl Into<String>) -> Self {
        Self::Separator {
            label: Some(label.into()),
        }
    }

    pub fn menu(id: impl Into<String>, label: impl Into<String>, children: Vec<TrayMenuItem>) -> Self {
        Self::Submenu {
            id: id.into(),
            label: label.into(),
            toggle_type: None,
            children,
        }
    }

    pub fn checkbox(id: impl Into<String>, label: impl Into<String>, checked: bool) -> Self {
        Self::Submenu {
            id: id.into(),
            label: label.into(),
            toggle_type: Some(TrayToggleType::Checkbox(checked)),
            children: Vec::new(),
        }
    }

    pub fn radio(id: impl Into<String>, label: impl Into<String>, checked: bool) -> Self {
        Self::Submenu {
            id: id.into(),
            label: label.into(),
            toggle_type: Some(TrayToggleType::Radio(checked)),
            children: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum TrayEvent {
    TrayClick {
        button: MouseButton,
        position: Point<i32>,
    },
    Scroll {
        scroll_detal: Point<i32>,
    },
    MenuClick {
        id: String,
    },
}

pub struct TrayItem {
    pub(crate) visible: bool,
    pub(crate) icon: TrayIcon,
    pub(crate) title: String,
    pub(crate) tooltip: String,
    pub(crate) description: String,
    pub(crate) submenus: Vec<TrayMenuItem>,
    pub(crate) event: Option<Box<dyn FnMut(TrayEvent, &mut App) + Send + 'static>>,
}

impl TrayItem {
    pub fn new() -> Self {
        Self {
            visible: true,
            icon: TrayIcon::Name(String::new()),
            title: String::new(),
            tooltip: String::new(),
            description: String::new(),
            submenus: Vec::new(),
            event: None,
        }
    }

    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    pub fn icon(mut self, icon: TrayIcon) -> Self {
        self.icon = icon;
        self
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    pub fn tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.tooltip = tooltip.into();
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    pub fn submenu(mut self, submenu: TrayMenuItem) -> Self {
        self.submenus.push(submenu);
        self
    }

    pub fn on_event(
        mut self,
        event: impl FnMut(TrayEvent, &mut App) + Send + 'static,
    ) -> Self {
        self.event = Some(Box::new(event));
        self
    }
}

#[cfg(target_os = "macos")]
mod tray_macos;

#[cfg(windows)]
mod tray_windows;

#[cfg(target_os = "linux")]
mod tray_linux;

#[cfg(target_os = "macos")]
pub fn set_up_tray(cx: &mut App, async_app: AsyncApp, item: TrayItem) -> anyhow::Result<()> {
    tray_macos::set_up_tray(cx, async_app, item)
}

#[cfg(windows)]
pub fn set_up_tray(cx: &mut App, async_app: AsyncApp, item: TrayItem) -> anyhow::Result<()> {
    tray_windows::set_up_tray(cx, async_app, item)
}

#[cfg(target_os = "linux")]
pub fn set_up_tray(cx: &mut App, async_app: AsyncApp, item: TrayItem) -> anyhow::Result<()> {
    tray_linux::set_up_tray(cx, async_app, item)
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub fn set_up_tray(_cx: &mut App, _async_app: AsyncApp, _item: TrayItem) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn sync_tray(cx: &mut App, item: TrayItem) -> anyhow::Result<()> {
    tray_macos::sync_tray(cx, item)
}

#[cfg(windows)]
pub fn sync_tray(cx: &mut App, item: TrayItem) -> anyhow::Result<()> {
    tray_windows::sync_tray(cx, item)
}

#[cfg(target_os = "linux")]
pub fn sync_tray(cx: &mut App, item: TrayItem) -> anyhow::Result<()> {
    tray_linux::sync_tray(cx, item)
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub fn sync_tray(_cx: &mut App, _item: TrayItem) -> anyhow::Result<()> {
    Ok(())
}

