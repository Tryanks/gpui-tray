use gpui::{App, AsyncApp, Image, MouseButton, Point};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

pub(crate) type TrayEventCallback = Box<dyn FnMut(TrayEvent, &mut App) + Send + 'static>;
pub(crate) type TrayEventCallbackSlot = Arc<Mutex<Option<TrayEventCallback>>>;

#[derive(Clone, Copy, Debug)]
pub enum TrayToggleType {
    Checkbox(bool),
    Radio(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayMenuItemRole {
    Standard,
    Info,
}

/// Item used to describe a tray context menu.
#[derive(Clone, Debug)]
pub enum TrayMenuItem {
    Separator {
        label: Option<String>,
        visible: bool,
    },
    Submenu {
        id: Option<String>,
        label: String,
        enabled: bool,
        visible: bool,
        role: TrayMenuItemRole,
        toggle_type: Option<TrayToggleType>,
        children: Vec<TrayMenuItem>,
    },
}

impl TrayMenuItem {
    pub fn separator() -> Self {
        Self::Separator {
            label: None,
            visible: true,
        }
    }

    pub fn labeled_separator(label: impl Into<String>) -> Self {
        Self::Separator {
            label: Some(label.into()),
            visible: true,
        }
    }

    pub fn menu(
        id: impl Into<String>,
        label: impl Into<String>,
        children: Vec<TrayMenuItem>,
    ) -> Self {
        Self::Submenu {
            id: Some(id.into()),
            label: label.into(),
            enabled: true,
            visible: true,
            role: TrayMenuItemRole::Standard,
            toggle_type: None,
            children,
        }
    }

    pub fn checkbox(id: impl Into<String>, label: impl Into<String>, checked: bool) -> Self {
        Self::Submenu {
            id: Some(id.into()),
            label: label.into(),
            enabled: true,
            visible: true,
            role: TrayMenuItemRole::Standard,
            toggle_type: Some(TrayToggleType::Checkbox(checked)),
            children: Vec::new(),
        }
    }

    pub fn radio(id: impl Into<String>, label: impl Into<String>, checked: bool) -> Self {
        Self::Submenu {
            id: Some(id.into()),
            label: label.into(),
            enabled: true,
            visible: true,
            role: TrayMenuItemRole::Standard,
            toggle_type: Some(TrayToggleType::Radio(checked)),
            children: Vec::new(),
        }
    }

    pub fn label(label: impl Into<String>) -> Self {
        Self::info(label)
    }

    pub fn info(label: impl Into<String>) -> Self {
        Self::Submenu {
            id: None,
            label: label.into(),
            enabled: false,
            visible: true,
            role: TrayMenuItemRole::Info,
            toggle_type: None,
            children: Vec::new(),
        }
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        if let Self::Submenu {
            enabled: item_enabled,
            ..
        } = &mut self
        {
            *item_enabled = enabled;
        }
        self
    }

    pub fn visible(mut self, visible: bool) -> Self {
        match &mut self {
            Self::Separator {
                visible: item_visible,
                ..
            } => *item_visible = visible,
            Self::Submenu {
                visible: item_visible,
                ..
            } => *item_visible = visible,
        }
        self
    }

    pub(crate) fn menu_event_id(&self) -> Option<&str> {
        match self {
            Self::Separator { .. } => None,
            Self::Submenu {
                id,
                enabled,
                role,
                children,
                ..
            } if *enabled && *role == TrayMenuItemRole::Standard && children.is_empty() => {
                id.as_deref()
            }
            Self::Submenu { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayClickAction {
    EmitEvent,
    OpenMenu,
    Ignore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayClickKind {
    Single,
    Double,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrayClickPolicy {
    pub left: TrayClickAction,
    pub right: TrayClickAction,
    pub double_click: TrayClickAction,
}

impl TrayClickPolicy {
    pub fn platform_default() -> Self {
        Self::default()
    }

    pub fn left(mut self, action: TrayClickAction) -> Self {
        self.left = action;
        self
    }

    pub fn right(mut self, action: TrayClickAction) -> Self {
        self.right = action;
        self
    }

    pub fn double_click(mut self, action: TrayClickAction) -> Self {
        self.double_click = action;
        self
    }
}

impl Default for TrayClickPolicy {
    fn default() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self {
                left: TrayClickAction::OpenMenu,
                right: TrayClickAction::OpenMenu,
                double_click: TrayClickAction::OpenMenu,
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            Self {
                left: TrayClickAction::EmitEvent,
                right: TrayClickAction::OpenMenu,
                double_click: TrayClickAction::EmitEvent,
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum TrayEvent {
    TrayClick {
        button: MouseButton,
        kind: TrayClickKind,
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
    pub(crate) icon: Option<Rc<Image>>,
    pub(crate) title: String,
    pub(crate) tooltip: String,
    pub(crate) description: String,
    pub(crate) submenus: Vec<TrayMenuItem>,
    pub(crate) click_policy: TrayClickPolicy,
    pub(crate) event: Option<TrayEventCallback>,
}

impl TrayItem {
    pub fn new() -> Self {
        Self {
            visible: true,
            icon: None,
            title: String::new(),
            tooltip: String::new(),
            description: String::new(),
            submenus: Vec::new(),
            click_policy: TrayClickPolicy::default(),
            event: None,
        }
    }

    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }

    pub fn icon(mut self, icon: impl Into<Image>) -> Self {
        self.icon = Some(Rc::new(icon.into()));
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

    pub fn click_policy(mut self, click_policy: TrayClickPolicy) -> Self {
        self.click_policy = click_policy;
        self
    }

    pub fn on_event(mut self, event: impl FnMut(TrayEvent, &mut App) + Send + 'static) -> Self {
        self.event = Some(Box::new(event));
        self
    }
}

impl Default for TrayItem {
    fn default() -> Self {
        Self::new()
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
