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

#[derive(Clone)]
pub struct TrayState {
    pub(crate) visible: bool,
    pub(crate) icon: Option<Rc<Image>>,
    pub(crate) title: String,
    pub(crate) tooltip: String,
    pub(crate) description: String,
    pub(crate) submenus: Vec<TrayMenuItem>,
    pub(crate) click_policy: TrayClickPolicy,
}

impl TrayState {
    pub fn new() -> Self {
        Self {
            visible: true,
            icon: None,
            title: String::new(),
            tooltip: String::new(),
            description: String::new(),
            submenus: Vec::new(),
            click_policy: TrayClickPolicy::default(),
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
}

impl Default for TrayState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub(crate) struct VersionedTrayState {
    pub(crate) version: u64,
    pub(crate) state: TrayState,
}

#[derive(Clone)]
pub(crate) struct TrayRuntimeState {
    pub(crate) desired_state: Option<VersionedTrayState>,
    pub(crate) applied_state: Option<VersionedTrayState>,
    pub(crate) flush_scheduled: bool,
    pub(crate) flushing: bool,
    next_version: u64,
}

impl TrayRuntimeState {
    pub(crate) fn new(initial: TrayState) -> Self {
        let mut runtime = Self {
            desired_state: None,
            applied_state: None,
            flush_scheduled: false,
            flushing: false,
            next_version: 1,
        };
        let _ = runtime.set_desired_state(initial);
        runtime
    }

    pub(crate) fn set_desired_state(&mut self, state: TrayState) -> bool {
        let version = self.next_version;
        self.next_version = self.next_version.saturating_add(1);
        self.desired_state = Some(VersionedTrayState { version, state });
        self.request_flush()
    }

    pub(crate) fn request_flush(&mut self) -> bool {
        if self.flush_scheduled {
            return false;
        }
        self.flush_scheduled = true;
        true
    }

    pub(crate) fn try_begin_flush(&mut self) -> Option<VersionedTrayState> {
        if self.flushing || !self.flush_scheduled {
            return None;
        }
        let desired = self.desired_state.clone()?;
        self.flushing = true;
        self.flush_scheduled = false;
        Some(desired)
    }

    pub(crate) fn finish_flush(&mut self, applied_state: VersionedTrayState) -> bool {
        self.applied_state = Some(applied_state.clone());
        self.flushing = false;

        if self.flush_scheduled {
            return true;
        }

        self.desired_state
            .as_ref()
            .map(|desired| desired.version != applied_state.version)
            .unwrap_or(false)
    }

    pub(crate) fn abort_flush(&mut self) {
        self.flushing = false;
        self.flush_scheduled = true;
    }

    pub(crate) fn has_pending_flush(&self) -> bool {
        self.flush_scheduled
    }
}

#[cfg(target_os = "macos")]
mod tray_macos;

#[cfg(windows)]
mod tray_windows;

#[cfg(target_os = "linux")]
mod tray_linux;

#[cfg(target_os = "macos")]
pub use tray_macos::TrayHandle;

#[cfg(windows)]
pub use tray_windows::TrayHandle;

#[cfg(target_os = "linux")]
pub use tray_linux::TrayHandle;

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
#[derive(Clone, Default)]
pub struct TrayHandle;

#[cfg(target_os = "macos")]
pub fn set_up_tray(
    cx: &mut App,
    async_app: AsyncApp,
    initial: TrayState,
    on_event: impl FnMut(TrayEvent, &mut App) + Send + 'static,
) -> anyhow::Result<TrayHandle> {
    tray_macos::set_up_tray(cx, async_app, initial, Box::new(on_event))
}

#[cfg(windows)]
pub fn set_up_tray(
    cx: &mut App,
    async_app: AsyncApp,
    initial: TrayState,
    on_event: impl FnMut(TrayEvent, &mut App) + Send + 'static,
) -> anyhow::Result<TrayHandle> {
    tray_windows::set_up_tray(cx, async_app, initial, Box::new(on_event))
}

#[cfg(target_os = "linux")]
pub fn set_up_tray(
    cx: &mut App,
    async_app: AsyncApp,
    initial: TrayState,
    on_event: impl FnMut(TrayEvent, &mut App) + Send + 'static,
) -> anyhow::Result<TrayHandle> {
    tray_linux::set_up_tray(cx, async_app, initial, Box::new(on_event))
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub fn set_up_tray(
    _cx: &mut App,
    _async_app: AsyncApp,
    _initial: TrayState,
    _on_event: impl FnMut(TrayEvent, &mut App) + Send + 'static,
) -> anyhow::Result<TrayHandle> {
    Ok(TrayHandle)
}

#[cfg(test)]
mod tests {
    use super::{TrayRuntimeState, TrayState};

    #[test]
    fn tray_state_clones_builder_data() {
        let state = TrayState::new()
            .title("hello")
            .tooltip("tip")
            .visible(false);
        let cloned = state.clone();

        assert!(!cloned.visible);
        assert_eq!(cloned.title, "hello");
        assert_eq!(cloned.tooltip, "tip");
    }

    #[test]
    fn latest_state_wins_after_multiple_updates() {
        let mut runtime = TrayRuntimeState::new(TrayState::new().title("A"));
        let _ = runtime.set_desired_state(TrayState::new().title("B"));
        let flushing = runtime.try_begin_flush().expect("pending flush");

        assert_eq!(flushing.state.title, "B");
        assert!(!runtime.flush_scheduled);
        assert!(runtime.flushing);
    }

    #[test]
    fn state_changes_during_flush_schedule_follow_up() {
        let mut runtime = TrayRuntimeState::new(TrayState::new().title("A"));
        let flushing = runtime.try_begin_flush().expect("pending flush");
        let _ = runtime.set_desired_state(TrayState::new().title("B"));

        assert!(runtime.finish_flush(flushing));

        let follow_up = runtime.try_begin_flush().expect("follow-up flush");
        assert_eq!(follow_up.state.title, "B");
    }

    #[test]
    fn aborting_flush_requeues_work() {
        let mut runtime = TrayRuntimeState::new(TrayState::new().title("A"));
        let _ = runtime.try_begin_flush().expect("pending flush");

        runtime.abort_flush();

        assert!(runtime.has_pending_flush());
        assert!(!runtime.flushing);
    }
}
