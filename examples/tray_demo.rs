use gpui::{
    App, Application, Context, Div, Global, QuitMode, SharedString, Stateful, Window,
    WindowOptions, actions, div, prelude::*, Menu, MenuItem,
};
use gpui_tray::{TrayEvent, TrayIcon, TrayItem, TrayMenuItem};

#[derive(PartialEq)]
enum ViewMode {
    List,
    Grid,
}

impl ViewMode {
    fn as_str(&self) -> &'static str {
        match self {
            Self::List => "List",
            Self::Grid => "Grid",
        }
    }

    fn toggle(&mut self) {
        *self = match self {
            Self::List => Self::Grid,
            Self::Grid => Self::List,
        };
    }
}

impl From<ViewMode> for SharedString {
    fn from(value: ViewMode) -> Self {
        match value {
            ViewMode::List => "List",
            ViewMode::Grid => "Grid",
        }
        .into()
    }
}

struct AppState {
    view_mode: ViewMode,
    tray_visible: bool,
    tray_title: SharedString,
    tray_tooltip: SharedString,
}

impl AppState {
    fn new() -> Self {
        Self {
            view_mode: ViewMode::List,
            tray_visible: true,
            tray_title: "Tray App".into(),
            tray_tooltip: "This is a tray icon".into(),
        }
    }
}

impl Global for AppState {}

actions!(
    tray_demo,
    [Quit, ToggleCheck, ToggleVisible, HideWindow, ShowWindow]
);

struct Example;

impl Render for Example {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        fn button(id: &'static str) -> Stateful<Div> {
            div()
                .id(id)
                .py_0p5()
                .px_3()
                .bg(gpui::black())
                .active(|this| this.bg(gpui::black().opacity(0.8)))
                .text_color(gpui::white())
        }

        let app_state = cx.global::<AppState>();

        div()
            .bg(gpui::white())
            .flex()
            .flex_col()
            .gap_4()
            .size_full()
            .justify_center()
            .items_center()
            .child("Tray demo (gpui-tray)")
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .child(
                        button("toggle-visible")
                            .child(format!("Visible: {}", app_state.tray_visible))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(ToggleVisible), cx);
                            }),
                    )
                    .child(
                        button("toggle-mode")
                            .child(format!("Mode: {}", app_state.view_mode.as_str()))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(ToggleCheck), cx);
                            }),
                    ),
            )
    }
}

fn build_tray_item(app_state: &AppState) -> TrayItem {
    let list_checked = app_state.view_mode == ViewMode::List;
    let grid_checked = app_state.view_mode == ViewMode::Grid;

    TrayItem::new()
        .visible(app_state.tray_visible)
        .icon(TrayIcon::Name("folder".to_string()))
        .title(app_state.tray_title.to_string())
        .tooltip(app_state.tray_tooltip.to_string())
        .description(String::new())
        .submenu(TrayMenuItem::radio("List", "List", list_checked))
        .submenu(TrayMenuItem::radio("Grid", "Grid", grid_checked))
        .submenu(TrayMenuItem::separator())
        .submenu(TrayMenuItem::menu("HideWindow", "Hide Window", Vec::new()))
        .submenu(TrayMenuItem::menu("ShowWindow", "Show Window", Vec::new()))
        .submenu(TrayMenuItem::separator())
        .submenu(TrayMenuItem::menu(
            "ToggleVisible",
            "Hide Tray Icon",
            Vec::new(),
        ))
        .submenu(TrayMenuItem::menu(
            "Submenu",
            "Submenu",
            vec![
                TrayMenuItem::checkbox("SubToggleCheck", "Toggle Check", false),
                TrayMenuItem::menu("SubToggleVisible", "Toggle Visible", Vec::new()),
            ],
        ))
        .submenu(TrayMenuItem::separator())
        .submenu(TrayMenuItem::menu("Quit", "Quit", Vec::new()))
}

fn sync_tray(cx: &mut App) {
    let item = build_tray_item(cx.global::<AppState>());
    if let Err(error) = gpui_tray::tray::sync_tray(cx, item) {
        eprintln!("failed to sync tray: {error:#}");
    }
}

fn main() -> anyhow::Result<()> {
    Application::new()
        .with_quit_mode(QuitMode::Explicit)
        .run(|cx: &mut App| {
            cx.set_global(AppState::new());

            // Ensure macOS shows our app's menu bar when our window is frontmost.
            cx.set_menus(vec![Menu {
                name: "tray_demo".into(),
                items: vec![MenuItem::action("Quit", Quit)],
            }]);

            cx.activate(true);
            cx.on_action(quit);
            cx.on_action(toggle_check);
            cx.on_action(toggle_visible);
            cx.on_action(hide_window);
            cx.on_action(show_window);

            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    #[cfg(target_os = "macos")]
                    {
                        if let Err(error) = set_shows_in_dock(false) {
                            eprintln!("failed to hide Dock icon: {error:#}");
                        }
                    }
                }
            })
            .detach();

            if let Err(error) =
                cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| Example))
            {
                eprintln!("failed to open window: {error:#}");
            }

            let async_app = cx.to_async();
            let item = build_tray_item(cx.global::<AppState>()).on_event(on_tray_event);
            if let Err(error) = gpui_tray::tray::set_up_tray(cx, async_app, item) {
                eprintln!("failed to set up tray: {error:#}");
            }
        });

    Ok(())
}

fn on_tray_event(event: TrayEvent, cx: &mut App) {
    match event {
        TrayEvent::TrayClick { button, .. } => {
            if button == gpui::MouseButton::Left {
                show_window(&ShowWindow, cx);
            }
        }
        TrayEvent::MenuClick { id } => match id.as_str() {
            "List" => {
                let current_is_list = cx.global::<AppState>().view_mode == ViewMode::List;
                if !current_is_list {
                    toggle_check(&ToggleCheck, cx);
                }
            }
            "Grid" => {
                let current_is_grid = cx.global::<AppState>().view_mode == ViewMode::Grid;
                if !current_is_grid {
                    toggle_check(&ToggleCheck, cx);
                }
            }
            "SubToggleCheck" => toggle_check(&ToggleCheck, cx),
            "ToggleVisible" | "SubToggleVisible" => toggle_visible(&ToggleVisible, cx),
            "HideWindow" => hide_window(&HideWindow, cx),
            "ShowWindow" => show_window(&ShowWindow, cx),
            "Quit" => quit(&Quit, cx),
            _ => {}
        },
        _ => {}
    }
}

fn quit(_: &Quit, cx: &mut App) {
    cx.quit();
}

fn toggle_check(_: &ToggleCheck, cx: &mut App) {
    {
        let app_state = cx.global_mut::<AppState>();
        app_state.view_mode.toggle();
        app_state.tray_title = format!("Mode: {}", app_state.view_mode.as_str()).into();
        app_state.tray_tooltip =
            format!("This is a tooltip, mode: {}", app_state.view_mode.as_str()).into();
    }

    sync_tray(cx);
    cx.refresh_windows();
}

fn toggle_visible(_: &ToggleVisible, cx: &mut App) {
    {
        let app_state = cx.global_mut::<AppState>();
        app_state.tray_visible = !app_state.tray_visible;
    }

    sync_tray(cx);
    cx.refresh_windows();
}

fn hide_window(_: &HideWindow, cx: &mut App) {
    cx.defer(|cx| {
        let handles: Vec<_> = cx.windows().iter().cloned().collect();
        for handle in handles {
            if let Err(error) = handle.update(cx, |_, window, _| window.remove_window()) {
                eprintln!("failed to remove window: {error:#}");
            }
        }
    });
}

fn show_window(_: &ShowWindow, cx: &mut App) {
    #[cfg(target_os = "macos")]
    {
        if let Err(error) = set_shows_in_dock(true) {
            eprintln!("failed to show Dock icon: {error:#}");
        }
    }

    if let Some(handle) = cx
        .active_window()
        .or_else(|| cx.windows().iter().next().cloned())
    {
        let _ = handle.update(cx, |_, window, _| {
            window.activate_window();
        });
        cx.activate(true);
        return;
    }

    if let Err(error) = cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| Example)) {
        eprintln!("failed to open window: {error:#}");
    }
    cx.activate(true);
}

#[cfg(target_os = "macos")]
fn set_shows_in_dock(shows_in_dock: bool) -> anyhow::Result<()> {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};

    #[repr(i64)]
    enum ActivationPolicy {
        Regular = 0,
        Accessory = 1,
    }

    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        if app.is_null() {
            anyhow::bail!("NSApplication.sharedApplication returned nil");
        }

        let policy = if shows_in_dock {
            ActivationPolicy::Regular
        } else {
            ActivationPolicy::Accessory
        };

        let success: bool = msg_send![app, setActivationPolicy: policy as i64];
        if !success {
            anyhow::bail!("setActivationPolicy returned false");
        }
    }

    Ok(())
}
