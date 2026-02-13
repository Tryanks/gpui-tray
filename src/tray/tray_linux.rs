use crate::tray::{TrayEvent, TrayItem, TrayMenuItem, TrayToggleType};
use anyhow::Result;
use gpui::{AsyncApp, MouseButton, Point};
use ksni::menu::{CheckmarkItem, StandardItem, SubMenu};
use ksni::{Handle, Icon, MenuItem as KsniMenuItem, Status, ToolTip, Tray, TrayService};
use std::sync::OnceLock;

#[derive(Clone)]
struct Handler {
    async_app: AsyncApp,
    callback: std::sync::Arc<
        std::sync::Mutex<Option<Box<dyn FnMut(TrayEvent, &mut gpui::App) + Send + 'static>>>,
    >,
}

impl Handler {
    fn dispatch(&self, event: TrayEvent) {
        let async_app = self.async_app.clone();
        let callback = self.callback.clone();
        async_app.update(|cx| {
            cx.defer(move |cx| {
                if let Ok(mut slot) = callback.lock() {
                    if let Some(cb) = slot.as_mut() {
                        cb(event, cx);
                    }
                }
            });
        });
    }
}

struct LinuxTray {
    handler: Handler,
    visible: bool,
    title: String,
    tooltip: String,
    description: String,
    icon_name: String,
    icon_pixmap: Vec<Icon>,
    menu: Vec<TrayMenuItem>,
}

impl LinuxTray {
    fn from_item(handler: Handler, mut item: TrayItem) -> Self {
        let icon_pixmap = icon_pixmap_from_item(&item).unwrap_or_default();

        // Callback is stored in the handler; item.event is consumed by set_up/sync.
        item.event = None;

        Self {
            handler,
            visible: item.visible,
            title: item.title,
            tooltip: item.tooltip,
            description: item.description,
            icon_name: String::new(),
            icon_pixmap,
            menu: item.submenus,
        }
    }

    fn update_from_item(&mut self, mut item: TrayItem) {
        let icon_pixmap = icon_pixmap_from_item(&item).unwrap_or_default();

        self.visible = item.visible;
        self.title = item.title;
        self.tooltip = item.tooltip;
        self.description = item.description;
        self.icon_name = String::new();
        self.icon_pixmap = icon_pixmap;
        self.menu = item.submenus;

        // If a new callback is provided, replace it.
        if let Some(cb) = item.event.take() {
            if let Ok(mut slot) = self.handler.callback.lock() {
                *slot = Some(cb);
            }
        }
    }

    fn build_menu_items(&self, items: &[TrayMenuItem]) -> Vec<KsniMenuItem<LinuxTray>> {
        items
            .iter()
            .flat_map(|item| match item {
                TrayMenuItem::Separator { .. } => vec![KsniMenuItem::Separator],
                TrayMenuItem::Submenu {
                    id,
                    label,
                    toggle_type,
                    children,
                } => {
                    if children.is_empty() {
                        let id = id.clone();
                        match toggle_type {
                            Some(TrayToggleType::Checkbox(checked))
                            | Some(TrayToggleType::Radio(checked)) => {
                                vec![KsniMenuItem::from(CheckmarkItem {
                                    label: label.clone(),
                                    checked: *checked,
                                    activate: Box::new(move |this| {
                                        this.handler
                                            .dispatch(TrayEvent::MenuClick { id: id.clone() })
                                    }),
                                    ..Default::default()
                                })]
                            }
                            None => vec![KsniMenuItem::from(StandardItem {
                                label: label.clone(),
                                activate: Box::new(move |this| {
                                    this.handler
                                        .dispatch(TrayEvent::MenuClick { id: id.clone() })
                                }),
                                ..Default::default()
                            })],
                        }
                    } else {
                        vec![KsniMenuItem::from(SubMenu {
                            label: label.clone(),
                            submenu: self.build_menu_items(children),
                            ..Default::default()
                        })]
                    }
                }
            })
            .collect()
    }
}

fn icon_pixmap_from_item(item: &TrayItem) -> Result<Option<Vec<Icon>>> {
    let Some(icon) = item.icon.as_ref() else {
        return Ok(None);
    };

    let (width, height, bgra) = crate::icon::decode_gpui_image_to_bgra32(icon)?;
    let data = bgra32_to_argb32(&bgra)?;
    Ok(Some(vec![Icon {
        width: width as i32,
        height: height as i32,
        data,
    }]))
}

fn bgra32_to_argb32(bgra: &[u8]) -> Result<Vec<u8>> {
    anyhow::ensure!(
        bgra.len() % 4 == 0,
        "expected BGRA32 byte length multiple of 4"
    );
    let mut argb = vec![0u8; bgra.len()];
    for (src, dst) in bgra.chunks_exact(4).zip(argb.chunks_exact_mut(4)) {
        let b = src[0];
        let g = src[1];
        let r = src[2];
        let a = src[3];
        dst[0] = a;
        dst[1] = r;
        dst[2] = g;
        dst[3] = b;
    }
    Ok(argb)
}

impl Tray for LinuxTray {
    fn activate(&mut self, x: i32, y: i32) {
        self.handler.dispatch(TrayEvent::TrayClick {
            button: MouseButton::Left,
            position: Point { x, y },
        });
    }

    fn secondary_activate(&mut self, x: i32, y: i32) {
        self.handler.dispatch(TrayEvent::TrayClick {
            button: MouseButton::Middle,
            position: Point { x, y },
        });
    }

    fn scroll(&mut self, delta: i32, dir: &str) {
        let dir = dir.to_ascii_lowercase();
        let scroll_detal = if dir.contains("horizontal") {
            Point { x: delta, y: 0 }
        } else {
            Point { x: 0, y: delta }
        };
        self.handler.dispatch(TrayEvent::Scroll { scroll_detal });
    }

    fn id(&self) -> String {
        // Avoids odd behavior on some trays.
        "gpui-tray".to_string()
    }

    fn title(&self) -> String {
        self.title.clone()
    }

    fn status(&self) -> Status {
        if self.visible {
            Status::Active
        } else {
            Status::Passive
        }
    }

    fn icon_name(&self) -> String {
        self.icon_name.clone()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icon_pixmap.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: self.tooltip.clone(),
            description: self.description.clone(),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<KsniMenuItem<Self>> {
        self.build_menu_items(&self.menu)
    }
}

static TRAY_HANDLE: OnceLock<Handle<LinuxTray>> = OnceLock::new();

pub fn set_up_tray(_cx: &mut gpui::App, async_app: AsyncApp, mut item: TrayItem) -> Result<()> {
    if TRAY_HANDLE.get().is_some() {
        anyhow::bail!("tray already initialized");
    }

    let callback = std::sync::Arc::new(std::sync::Mutex::new(item.event.take()));
    let handler = Handler {
        async_app,
        callback,
    };
    let tray = LinuxTray::from_item(handler, item);

    let service = TrayService::new(tray);
    let handle = service.handle();
    TRAY_HANDLE
        .set(handle)
        .map_err(|_| anyhow::anyhow!("tray storage already initialized"))?;

    service.spawn();
    Ok(())
}

pub fn sync_tray(_cx: &mut gpui::App, item: TrayItem) -> Result<()> {
    let Some(handle) = TRAY_HANDLE.get() else {
        return Ok(());
    };

    handle.update(|tray| tray.update_from_item(item));
    Ok(())
}
