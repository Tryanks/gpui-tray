use crate::tray::{TrayEvent, TrayItem, TrayMenuItem, TrayToggleType};
use anyhow::{Context as _, Result};
use gpui::{AsyncApp, MouseButton, Point};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, atomic::AtomicU32, atomic::Ordering};

const STATUS_NOTIFIER_WATCHER_INTERFACE: &str = "org.kde.StatusNotifierWatcher";
const STATUS_NOTIFIER_WATCHER_PATH: &str = "/StatusNotifierWatcher";
const STATUS_NOTIFIER_WATCHER_DESTINATION: &str = "org.kde.StatusNotifierWatcher";

const STATUS_NOTIFIER_ITEM_PATH: &str = "/StatusNotifierItem";
const DBUS_MENU_PATH: &str = "/MenuBar";

#[derive(Clone)]
struct Handler {
    async_app: AsyncApp,
    callback: Arc<Mutex<Option<Box<dyn FnMut(TrayEvent, &mut gpui::App) + Send + 'static>>>>,
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

#[derive(Debug, Clone)]
enum LinuxEvent {
    Activate(i32, i32),
    SecondaryActivate(i32, i32),
    Scroll(i32, String),
    MenuClick(String),
}

#[derive(Default, Debug, Clone, zbus::zvariant::Type)]
struct Pixmap {
    width: i32,
    height: i32,
    bytes: Vec<u8>,
}

impl Pixmap {
    fn new(width: i32, height: i32, bytes: Vec<u8>) -> Self {
        Self {
            width,
            height,
            bytes,
        }
    }
}

impl From<Pixmap> for zbus::zvariant::Structure<'_> {
    fn from(value: Pixmap) -> Self {
        zbus::zvariant::StructureBuilder::new()
            .add_field(value.width)
            .add_field(value.height)
            .add_field(value.bytes)
            .build()
    }
}

#[derive(Debug, Clone, zbus::zvariant::Type)]
struct ToolTip {
    icon_name: String,
    icon_pixmap: Vec<Pixmap>,
    title: String,
    description: String,
}

impl From<ToolTip> for zbus::zvariant::Structure<'_> {
    fn from(value: ToolTip) -> Self {
        zbus::zvariant::StructureBuilder::new()
            .add_field(value.icon_name)
            .add_field(value.icon_pixmap)
            .add_field(value.title)
            .add_field(value.description)
            .build()
    }
}

#[derive(Debug, Clone)]
struct LinuxTrayItem {
    visible: bool,
    title: String,
    tooltip: String,
    description: String,
    icon_pixmaps: Vec<Pixmap>,
    menu: DBusMenu,
}

fn linux_item_from_tray_item(item: TrayItem) -> Result<LinuxTrayItem> {
    let icon_pixmaps = icon_pixmaps_from_item(&item).unwrap_or_default();
    let menu = DBusMenu::from_tray_menu_items(&item.submenus);
    Ok(LinuxTrayItem {
        visible: item.visible,
        title: item.title,
        tooltip: item.tooltip,
        description: item.description,
        icon_pixmaps,
        menu,
    })
}

fn icon_pixmaps_from_item(item: &TrayItem) -> Result<Option<Vec<Pixmap>>> {
    let Some(icon) = item.icon.as_ref() else {
        return Ok(None);
    };

    let (width, height, bgra) = crate::icon::decode_gpui_image_to_bgra32(icon)?;
    let data = bgra32_to_argb32(&bgra)?;
    Ok(Some(vec![Pixmap::new(width as i32, height as i32, data)]))
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

#[derive(Debug, Clone)]
enum MenuToggleType {
    Checkmark,
    Radio,
}

#[derive(Debug, Clone)]
enum MenuProperty {
    Type(&'static str),
    Label(String),
    Enabled(bool),
    Visible(bool),
    ToggleType(MenuToggleType),
    ToggleState(i32),
}

impl MenuProperty {
    fn key(&self) -> &'static str {
        match self {
            Self::Type(_) => "type",
            Self::Label(_) => "label",
            Self::Enabled(_) => "enabled",
            Self::Visible(_) => "visible",
            Self::ToggleType(_) => "toggle-type",
            Self::ToggleState(_) => "toggle-state",
        }
    }

    fn to_value(&self) -> zbus::zvariant::Value<'static> {
        match self {
            Self::Type(t) => zbus::zvariant::Value::from(*t),
            Self::Label(s) => zbus::zvariant::Value::from(s.clone()),
            Self::Enabled(b) => zbus::zvariant::Value::from(*b),
            Self::Visible(b) => zbus::zvariant::Value::from(*b),
            Self::ToggleType(t) => match t {
                MenuToggleType::Checkmark => zbus::zvariant::Value::from("checkmark"),
                MenuToggleType::Radio => zbus::zvariant::Value::from("radio"),
            },
            Self::ToggleState(s) => zbus::zvariant::Value::from(*s),
        }
    }
}

#[derive(Default, Debug, Clone, zbus::zvariant::Type)]
struct DBusMenuLayoutItem {
    id: i32,
    properties: HashMap<String, zbus::zvariant::Value<'static>>,
    children: Vec<zbus::zvariant::Value<'static>>,
}

impl From<DBusMenuLayoutItem> for zbus::zvariant::Structure<'_> {
    fn from(value: DBusMenuLayoutItem) -> Self {
        zbus::zvariant::StructureBuilder::new()
            .add_field(value.id)
            .add_field(value.properties)
            .add_field(value.children)
            .build()
    }
}

#[derive(Default, Debug, Clone)]
struct MenuNode {
    id: i32,
    parent_id: i32,
    user_id: Option<String>,
    properties: HashMap<&'static str, MenuProperty>,
    children: Vec<i32>,
}

#[derive(Debug, Clone)]
struct DBusMenu {
    nodes: HashMap<i32, MenuNode>,
}

impl DBusMenu {
    fn new() -> Self {
        let mut nodes = HashMap::new();
        nodes.insert(0, MenuNode::default());
        Self { nodes }
    }

    fn from_tray_menu_items(items: &[TrayMenuItem]) -> Self {
        let mut menu = DBusMenu::new();
        let mut next_id: i32 = 1;
        for item in items {
            next_id = menu.add_item(0, item, next_id);
        }
        menu
    }

    fn add_item(&mut self, parent_id: i32, item: &TrayMenuItem, next_id: i32) -> i32 {
        match item {
            TrayMenuItem::Separator { .. } => {
                let id = next_id;
                let mut node = MenuNode {
                    id,
                    parent_id,
                    ..Default::default()
                };
                node.properties
                    .insert("type", MenuProperty::Type("separator"));
                self.insert_node(parent_id, node);
                next_id + 1
            }
            TrayMenuItem::Submenu {
                id: user_id,
                label,
                toggle_type,
                children,
            } => {
                let id = next_id;
                let mut node = MenuNode {
                    id,
                    parent_id,
                    user_id: Some(user_id.clone()),
                    ..Default::default()
                };
                node.properties
                    .insert("type", MenuProperty::Type("standard"));
                node.properties
                    .insert("label", MenuProperty::Label(label.clone()));

                if let Some(toggle) = toggle_type {
                    match toggle {
                        TrayToggleType::Checkbox(checked) => {
                            node.properties.insert(
                                "toggle-type",
                                MenuProperty::ToggleType(MenuToggleType::Checkmark),
                            );
                            node.properties.insert(
                                "toggle-state",
                                MenuProperty::ToggleState(if *checked { 1 } else { 0 }),
                            );
                        }
                        TrayToggleType::Radio(checked) => {
                            node.properties.insert(
                                "toggle-type",
                                MenuProperty::ToggleType(MenuToggleType::Radio),
                            );
                            node.properties.insert(
                                "toggle-state",
                                MenuProperty::ToggleState(if *checked { 1 } else { 0 }),
                            );
                        }
                    }
                }

                self.insert_node(parent_id, node);
                let mut next_id = next_id + 1;
                for child in children {
                    next_id = self.add_item(id, child, next_id);
                }
                next_id
            }
        }
    }

    fn insert_node(&mut self, parent_id: i32, node: MenuNode) {
        let id = node.id;
        self.nodes.insert(id, node);
        if let Some(parent) = self.nodes.get_mut(&parent_id) {
            parent.children.push(id);
        }
    }

    fn user_id_for_node(&self, id: i32) -> Option<String> {
        self.nodes.get(&id).and_then(|n| n.user_id.clone())
    }

    fn to_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: &[String],
    ) -> DBusMenuLayoutItem {
        let Some(node) = self.nodes.get(&parent_id) else {
            return DBusMenuLayoutItem {
                id: parent_id,
                ..Default::default()
            };
        };

        let mut layout = DBusMenuLayoutItem {
            id: node.id,
            ..Default::default()
        };

        let include_all = property_names.is_empty();
        for (k, v) in &node.properties {
            if include_all || property_names.iter().any(|p| p == *k) {
                layout.properties.insert((*k).to_string(), v.to_value());
            }
        }

        if !node.children.is_empty() && recursion_depth != 0 {
            layout.properties.insert(
                "children-display".into(),
                zbus::zvariant::Value::from("submenu"),
            );
            for child in &node.children {
                let child_layout = self.to_layout(*child, recursion_depth - 1, property_names);
                layout
                    .children
                    .push(zbus::zvariant::Value::from(child_layout));
            }
        }

        layout
    }
}

struct DBusMenuInterface {
    menu: Arc<Mutex<DBusMenu>>,
    revision: Arc<AtomicU32>,
    events: tokio::sync::mpsc::UnboundedSender<LinuxEvent>,
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl DBusMenuInterface {
    #[zbus(out_args("revision", "layout"))]
    async fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        properties: Vec<String>,
    ) -> (u32, DBusMenuLayoutItem) {
        let menu = self
            .menu
            .lock()
            .ok()
            .map(|m| m.clone())
            .unwrap_or_else(DBusMenu::new);
        let revision = self.revision.load(Ordering::Relaxed);
        (
            revision,
            menu.to_layout(parent_id, recursion_depth, &properties),
        )
    }

    async fn event(
        &self,
        id: i32,
        event_id: String,
        _event_data: zbus::zvariant::Value<'_>,
        _timestamp: u32,
    ) {
        if event_id == "clicked" {
            let user_id = self.menu.lock().ok().and_then(|m| m.user_id_for_node(id));
            if let Some(user_id) = user_id {
                let _ = self.events.send(LinuxEvent::MenuClick(user_id));
            }
        }
    }

    async fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    #[zbus::signal(name = "LayoutUpdated")]
    async fn layout_updated(
        &self,
        cx: &zbus::object_server::SignalContext<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;
}

#[derive(Debug, Clone, Default)]
struct StatusNotifierItemState {
    visible: bool,
    title: String,
    icon_pixmaps: Vec<Pixmap>,
    tooltip: String,
    description: String,
}

struct StatusNotifierItemInterface {
    state: Arc<Mutex<StatusNotifierItemState>>,
    events: tokio::sync::mpsc::UnboundedSender<LinuxEvent>,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl StatusNotifierItemInterface {
    #[zbus(property, name = "Category")]
    fn category(&self) -> String {
        "ApplicationStatus".to_string()
    }

    #[zbus(property, name = "Id")]
    fn id(&self) -> String {
        // Avoid odd behavior on some trays; keep stable.
        "gpui-tray".to_string()
    }

    #[zbus(property, name = "Title")]
    fn title(&self) -> String {
        self.state
            .lock()
            .ok()
            .map(|s| s.title.clone())
            .unwrap_or_default()
    }

    #[zbus(property, name = "Status")]
    fn status(&self) -> String {
        let visible = self.state.lock().ok().map(|s| s.visible).unwrap_or(true);
        if visible {
            "Active".to_string()
        } else {
            "Passive".to_string()
        }
    }

    #[zbus(property, name = "IconName")]
    fn icon_name(&self) -> String {
        String::new()
    }

    #[zbus(property, name = "IconPixmap")]
    fn icon_pixmap(&self) -> Vec<Pixmap> {
        self.state
            .lock()
            .ok()
            .map(|s| s.icon_pixmaps.clone())
            .unwrap_or_default()
    }

    #[zbus(property, name = "ToolTip")]
    fn tool_tip(&self) -> ToolTip {
        let state = self
            .state
            .lock()
            .ok()
            .map(|s| s.clone())
            .unwrap_or_default();
        ToolTip {
            icon_name: String::new(),
            icon_pixmap: state.icon_pixmaps,
            title: state.tooltip,
            description: state.description,
        }
    }

    #[zbus(property, name = "ItemIsMenu")]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[zbus(property, name = "Menu")]
    fn menu(&self) -> zbus::zvariant::OwnedObjectPath {
        zbus::zvariant::OwnedObjectPath::try_from(DBUS_MENU_PATH).expect("valid dbus path")
    }

    async fn activate(&self, x: i32, y: i32) {
        let _ = self.events.send(LinuxEvent::Activate(x, y));
    }

    async fn secondary_activate(&self, x: i32, y: i32) {
        let _ = self.events.send(LinuxEvent::SecondaryActivate(x, y));
    }

    async fn scroll(&self, delta: i32, orientation: String) {
        let _ = self.events.send(LinuxEvent::Scroll(delta, orientation));
    }

    #[zbus::signal(name = "NewTitle")]
    async fn new_title(&self, cx: &zbus::object_server::SignalContext<'_>) -> zbus::Result<()>;

    #[zbus::signal(name = "NewIcon")]
    async fn new_icon(&self, cx: &zbus::object_server::SignalContext<'_>) -> zbus::Result<()>;

    #[zbus::signal(name = "NewToolTip")]
    async fn new_tooltip(&self, cx: &zbus::object_server::SignalContext<'_>) -> zbus::Result<()>;

    #[zbus::signal(name = "NewStatus")]
    async fn new_status(
        &self,
        cx: &zbus::object_server::SignalContext<'_>,
        status: String,
    ) -> zbus::Result<()>;

    #[zbus::signal(name = "NewMenu")]
    async fn new_menu(&self, cx: &zbus::object_server::SignalContext<'_>) -> zbus::Result<()>;
}

enum Command {
    Update(LinuxTrayItem),
}

struct LinuxTrayHandle {
    handler: Handler,
    cmd_tx: tokio::sync::mpsc::UnboundedSender<Command>,
}

static LINUX_TRAY: OnceLock<LinuxTrayHandle> = OnceLock::new();

fn make_bus_name() -> String {
    // Format inspired by common implementations; must be a unique well-formed bus name.
    // (No ':' here; that's for unique names assigned by the bus.)
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!(
        "org.freedesktop.StatusNotifierItem.gpui_tray_{}_{}",
        pid, nanos
    )
}

async fn register_with_watcher(connection: &zbus::Connection, service: &str) -> zbus::Result<()> {
    let proxy: zbus::Proxy = zbus::ProxyBuilder::new(connection)
        .interface(STATUS_NOTIFIER_WATCHER_INTERFACE)?
        .path(STATUS_NOTIFIER_WATCHER_PATH)?
        .destination(STATUS_NOTIFIER_WATCHER_DESTINATION)?
        .build()
        .await?;

    proxy
        .connection()
        .call_method(
            Some(STATUS_NOTIFIER_WATCHER_DESTINATION),
            STATUS_NOTIFIER_WATCHER_PATH,
            Some(STATUS_NOTIFIER_WATCHER_INTERFACE),
            "RegisterStatusNotifierItem",
            &(service),
        )
        .await?;
    Ok(())
}

pub fn set_up_tray(_cx: &mut gpui::App, async_app: AsyncApp, mut item: TrayItem) -> Result<()> {
    if LINUX_TRAY.get().is_some() {
        anyhow::bail!("tray already initialized");
    }

    let callback = Arc::new(Mutex::new(item.event.take()));
    let handler = Handler {
        async_app,
        callback,
    };

    let linux_item = linux_item_from_tray_item(item)?;
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<Command>();

    // Event fan-in for Activate/Scroll/Menu clicks from DBus interfaces.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<LinuxEvent>();

    let state = Arc::new(Mutex::new(StatusNotifierItemState {
        visible: linux_item.visible,
        title: linux_item.title.clone(),
        icon_pixmaps: linux_item.icon_pixmaps.clone(),
        tooltip: linux_item.tooltip.clone(),
        description: linux_item.description.clone(),
    }));

    let menu = Arc::new(Mutex::new(linux_item.menu.clone()));
    let revision = Arc::new(AtomicU32::new(1));

    // Store handle before spawning so sync_tray can send updates immediately after setup.
    LINUX_TRAY
        .set(LinuxTrayHandle {
            handler: handler.clone(),
            cmd_tx: cmd_tx.clone(),
        })
        .map_err(|_| anyhow::anyhow!("tray storage already initialized"))?;

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let Ok(rt) = rt else {
            return;
        };

        rt.block_on(async move {
            let service = make_bus_name();

            let status_iface = StatusNotifierItemInterface {
                state: state.clone(),
                events: event_tx.clone(),
            };

            let menu_iface = DBusMenuInterface {
                menu: menu.clone(),
                revision: revision.clone(),
                events: event_tx.clone(),
            };

            let builder = zbus::ConnectionBuilder::session();
            let Ok(builder) = builder else {
                return;
            };

            let builder = builder.name(service.clone());
            let Ok(builder) = builder else {
                return;
            };

            let builder = builder.serve_at(STATUS_NOTIFIER_ITEM_PATH, status_iface);
            let Ok(builder) = builder else {
                return;
            };

            let builder = builder.serve_at(DBUS_MENU_PATH, menu_iface);
            let Ok(builder) = builder else {
                return;
            };

            let connection = builder.build().await;
            let Ok(connection) = connection else {
                return;
            };

            // Best-effort watcher registration; some environments may not have a watcher.
            let _ = register_with_watcher(&connection, &service).await;

            let status_ref = connection
                .object_server()
                .interface::<_, StatusNotifierItemInterface>(STATUS_NOTIFIER_ITEM_PATH)
                .await
                .ok();
            let menu_ref = connection
                .object_server()
                .interface::<_, DBusMenuInterface>(DBUS_MENU_PATH)
                .await
                .ok();

            loop {
                tokio::select! {
                    Some(cmd) = cmd_rx.recv() => {
                        match cmd {
                            Command::Update(update) => {
                                if let Ok(mut s) = state.lock() {
                                    s.visible = update.visible;
                                    s.title = update.title;
                                    s.icon_pixmaps = update.icon_pixmaps;
                                    s.tooltip = update.tooltip;
                                    s.description = update.description;
                                }
                                if let Ok(mut m) = menu.lock() {
                                    *m = update.menu;
                                }
                                let rev = revision.fetch_add(1, Ordering::Relaxed).saturating_add(1);

                                if let Some(status_ref) = status_ref.as_ref() {
                                    if let Ok(cx) = status_ref.signal_context().await {
                                        let _ = StatusNotifierItemInterface::new_title(status_ref, &cx).await;
                                        let _ = StatusNotifierItemInterface::new_icon(status_ref, &cx).await;
                                        let _ = StatusNotifierItemInterface::new_tooltip(status_ref, &cx).await;
                                        let _ = StatusNotifierItemInterface::new_status(status_ref, &cx, {
                                            let visible = state.lock().ok().map(|s| s.visible).unwrap_or(true);
                                            if visible { "Active".to_string() } else { "Passive".to_string() }
                                        }).await;
                                        let _ = StatusNotifierItemInterface::new_menu(status_ref, &cx).await;
                                    }
                                }

                                if let Some(menu_ref) = menu_ref.as_ref() {
                                    if let Ok(cx) = menu_ref.signal_context().await {
                                        let _ = DBusMenuInterface::layout_updated(menu_ref, &cx, rev, 0).await;
                                    }
                                }
                            }
                        }
                    }
                    Some(ev) = event_rx.recv() => {
                        let event = match ev {
                            LinuxEvent::Activate(x,y) => TrayEvent::TrayClick{
                                button: MouseButton::Left,
                                position: Point { x, y },
                            },
                            LinuxEvent::SecondaryActivate(x,y) => TrayEvent::TrayClick{
                                button: MouseButton::Middle,
                                position: Point { x, y },
                            },
                            LinuxEvent::Scroll(delta, orientation) => {
                                let o = orientation.to_ascii_lowercase();
                                let scroll_detal = if o.contains("horizontal") {
                                    Point { x: delta, y: 0 }
                                } else {
                                    Point { x: 0, y: delta }
                                };
                                TrayEvent::Scroll { scroll_detal }
                            }
                            LinuxEvent::MenuClick(id) => TrayEvent::MenuClick { id },
                        };
                        handler.dispatch(event);
                    }
                    else => break,
                }
            }
        });
    });

    // Push initial state through the same codepath (signals/layout update).
    let _ = cmd_tx.send(Command::Update(linux_item));
    Ok(())
}

pub fn sync_tray(_cx: &mut gpui::App, mut item: TrayItem) -> Result<()> {
    let Some(handle) = LINUX_TRAY.get() else {
        return Ok(());
    };

    // Replace callback if provided.
    if let Some(cb) = item.event.take() {
        if let Ok(mut slot) = handle.handler.callback.lock() {
            *slot = Some(cb);
        }
    }

    let linux_item =
        linux_item_from_tray_item(item).context("failed to build linux tray payload")?;
    let _ = handle.cmd_tx.send(Command::Update(linux_item));
    Ok(())
}
