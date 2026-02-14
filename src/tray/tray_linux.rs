use crate::tray::{TrayEvent, TrayEventCallbackSlot, TrayItem, TrayMenuItem, TrayToggleType};
use anyhow::{Context as _, Result};
use gpui::{AsyncApp, MouseButton, Point};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, atomic::AtomicU32, atomic::Ordering};

const STATUS_NOTIFIER_WATCHER_INTERFACE: &str = "org.kde.StatusNotifierWatcher";
const STATUS_NOTIFIER_WATCHER_PATH: &str = "/StatusNotifierWatcher";
const STATUS_NOTIFIER_WATCHER_DESTINATION: &str = "org.kde.StatusNotifierWatcher";

const STATUS_NOTIFIER_ITEM_PATH: &str = "/StatusNotifierItem";
const DBUS_MENU_PATH: &str = "/MenuBar";

fn dispatch_event(async_app: &AsyncApp, callback: &TrayEventCallbackSlot, event: TrayEvent) {
    let async_app = async_app.clone();
    let callback = callback.clone();
    async_app.update(|cx| {
        cx.defer(move |cx| {
            if let Ok(mut slot) = callback.lock()
                && let Some(cb) = slot.as_mut()
            {
                cb(event, cx);
            }
        });
    });
}

#[derive(Debug, Clone)]
enum LinuxEvent {
    Activate(i32, i32),
    SecondaryActivate(i32, i32),
    Scroll(i32, String),
    MenuClick(String),
}

#[derive(Default, Debug, Clone, zbus::zvariant::Type, serde::Serialize)]
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
            .expect("valid Pixmap zvariant structure")
    }
}

#[derive(Debug, Clone, zbus::zvariant::Type, serde::Serialize)]
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
            .expect("valid ToolTip zvariant structure")
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
    let icon_pixmaps = icon_pixmaps_from_item(&item)?.unwrap_or_default();
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
    anyhow::ensure!(width > 0 && height > 0, "icon has zero size");

    // Some SNI hosts don't reliably scale very large pixmaps. Provide a few common tray sizes.
    let sizes: [u32; 4] = [16, 24, 32, 48];
    let mut pixmaps = Vec::new();
    for size in sizes {
        if size > width || size > height {
            continue;
        }
        let scaled = resize_bgra32_nearest(&bgra, width, height, size, size)?;
        // Although the SNI spec says "ARGB32", many hosts interpret this as native-endian
        // 0xAARRGGBB pixels (e.g. Qt/cairo ARGB32). On little-endian systems that is
        // byte-ordered BGRA. GPUI already gives us BGRA8, so pass it through.
        pixmaps.push(Pixmap::new(size as i32, size as i32, scaled));
    }

    // Fallback: expose the original size if it's already small.
    if pixmaps.is_empty() {
        pixmaps.push(Pixmap::new(width as i32, height as i32, bgra));
    }

    Ok(Some(pixmaps))
}

fn resize_bgra32_nearest(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Result<Vec<u8>> {
    anyhow::ensure!(
        src_w > 0 && src_h > 0 && dst_w > 0 && dst_h > 0,
        "invalid size"
    );
    let src_w = src_w as usize;
    let src_h = src_h as usize;
    let dst_w = dst_w as usize;
    let dst_h = dst_h as usize;
    anyhow::ensure!(
        src.len() == src_w * src_h * 4,
        "expected BGRA32 buffer length {}",
        src_w * src_h * 4
    );

    let mut dst = vec![0u8; dst_w * dst_h * 4];
    for y in 0..dst_h {
        let sy = y * src_h / dst_h;
        for x in 0..dst_w {
            let sx = x * src_w / dst_w;
            let s = (sy * src_w + sx) * 4;
            let d = (y * dst_w + x) * 4;
            dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    Ok(dst)
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

#[derive(Default, Debug, Clone, zbus::zvariant::Type, serde::Serialize)]
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
            .expect("valid DBusMenuLayoutItem zvariant structure")
    }
}

#[derive(Default, Debug, Clone)]
struct MenuNode {
    id: i32,
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
        let mut root = MenuNode::default();
        // Some hosts treat missing properties as false; be explicit.
        root.properties
            .insert("enabled", MenuProperty::Enabled(true));
        root.properties
            .insert("visible", MenuProperty::Visible(true));
        nodes.insert(0, root);
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
                    ..Default::default()
                };
                node.properties
                    .insert("type", MenuProperty::Type("separator"));
                node.properties
                    .insert("enabled", MenuProperty::Enabled(true));
                node.properties
                    .insert("visible", MenuProperty::Visible(true));
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
                    user_id: Some(user_id.clone()),
                    ..Default::default()
                };
                node.properties
                    .insert("type", MenuProperty::Type("standard"));
                node.properties
                    .insert("label", MenuProperty::Label(label.clone()));
                node.properties
                    .insert("enabled", MenuProperty::Enabled(true));
                node.properties
                    .insert("visible", MenuProperty::Visible(true));

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
    // libdbusmenu uses Version=4. Some hosts won't populate menus without it.
    #[zbus(property, name = "Version")]
    fn version(&self) -> u32 {
        4
    }

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

    #[zbus(out_args("properties"))]
    async fn get_group_properties(
        &self,
        ids: Vec<i32>,
        property_names: Vec<String>,
    ) -> Vec<(i32, HashMap<String, zbus::zvariant::Value<'static>>)> {
        let menu = self
            .menu
            .lock()
            .ok()
            .map(|m| m.clone())
            .unwrap_or_else(DBusMenu::new);

        let include_all = property_names.is_empty();
        ids.into_iter()
            .map(|id| {
                let mut props: HashMap<String, zbus::zvariant::Value<'static>> = HashMap::new();
                if let Some(node) = menu.nodes.get(&id) {
                    for (k, v) in &node.properties {
                        if include_all || property_names.iter().any(|p| p == *k) {
                            props.insert((*k).to_string(), v.to_value());
                        }
                    }
                }
                (id, props)
            })
            .collect()
    }

    async fn get_property(&self, id: i32, name: String) -> zbus::zvariant::Value<'static> {
        let menu = self
            .menu
            .lock()
            .ok()
            .map(|m| m.clone())
            .unwrap_or_else(DBusMenu::new);

        if let Some(node) = menu.nodes.get(&id)
            && let Some(prop) = node.properties.get(name.as_str())
        {
            return prop.to_value();
        }

        // Hosts tend to treat missing properties as "unset"; return an empty string variant.
        zbus::zvariant::Value::from(String::new())
    }

    async fn event(
        &self,
        id: i32,
        event_id: String,
        _event_data: zbus::zvariant::Value<'_>,
        _timestamp: u32,
    ) {
        self.dispatch_menu_event(id, &event_id);
    }

    // Some hosts only send click events through EventGroup.
    async fn event_group(&self, events: Vec<(i32, String, zbus::zvariant::Value<'_>, u32)>) {
        for (id, event_id, _event_data, _timestamp) in events {
            self.dispatch_menu_event(id, &event_id);
        }
    }

    // Keep click mapping logic in one place so Event and EventGroup behave the same.
    fn dispatch_menu_event(&self, id: i32, event_id: &str) {
        let event_id_lower = event_id.to_ascii_lowercase();

        // Different hosts use different event ids for activation.
        let is_activation = matches!(
            event_id_lower.as_str(),
            "clicked" | "activate" | "activated" | "toggled"
        );
        if !is_activation {
            return;
        }

        if std::env::var_os("GPUI_TRAY_DEBUG").is_some() {
            eprintln!("dbusmenu click id={id} event_id={event_id}");
        }

        let user_id = self.menu.lock().ok().and_then(|m| m.user_id_for_node(id));
        if let Some(user_id) = user_id {
            let _ = self.events.send(LinuxEvent::MenuClick(user_id));
        }
    }

    async fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    #[zbus(signal, name = "LayoutUpdated")]
    async fn layout_updated(
        emitter: &zbus::object_server::SignalEmitter<'_>,
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
        // Fallback for hosts that ignore IconPixmap or misinterpret its byte order.
        // This should exist in standard icon themes.
        "application-x-executable".to_string()
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

    #[zbus(signal, name = "NewTitle")]
    async fn new_title(emitter: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal, name = "NewIcon")]
    async fn new_icon(emitter: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal, name = "NewToolTip")]
    async fn new_tooltip(emitter: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal, name = "NewStatus")]
    async fn new_status(
        emitter: &zbus::object_server::SignalEmitter<'_>,
        status: String,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "NewMenu")]
    async fn new_menu(emitter: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;
}

enum Command {
    Update(LinuxTrayItem),
}

struct LinuxTrayHandle {
    callback: TrayEventCallbackSlot,
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
    let proxy = zbus::Proxy::new(
        connection,
        STATUS_NOTIFIER_WATCHER_DESTINATION,
        STATUS_NOTIFIER_WATCHER_PATH,
        STATUS_NOTIFIER_WATCHER_INTERFACE,
    )
    .await?;

    proxy
        .call_method("RegisterStatusNotifierItem", &(service))
        .await?;
    Ok(())
}

pub fn set_up_tray(_cx: &mut gpui::App, async_app: AsyncApp, mut item: TrayItem) -> Result<()> {
    if LINUX_TRAY.get().is_some() {
        anyhow::bail!("tray already initialized");
    }

    let callback = Arc::new(Mutex::new(item.event.take()));

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
            callback: callback.clone(),
            cmd_tx: cmd_tx.clone(),
        })
        .map_err(|_| anyhow::anyhow!("tray storage already initialized"))?;

    async_app
        .spawn(move |cx: &mut AsyncApp| {
        let async_app = cx.clone();
        let callback = callback.clone();
        async move {
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

            let builder = zbus::connection::Builder::session();
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
                                    let emitter = status_ref.signal_emitter();
                                    let _ = StatusNotifierItemInterface::new_title(emitter).await;
                                    let _ = StatusNotifierItemInterface::new_icon(emitter).await;
                                    let _ = StatusNotifierItemInterface::new_tooltip(emitter).await;
                                    let _ = StatusNotifierItemInterface::new_status(
                                        emitter,
                                        {
                                            let visible =
                                                state.lock().ok().map(|s| s.visible).unwrap_or(true);
                                            if visible {
                                                "Active".to_string()
                                            } else {
                                                "Passive".to_string()
                                            }
                                        },
                                    )
                                    .await;
                                    let _ = StatusNotifierItemInterface::new_menu(emitter).await;
                                }

                                if let Some(menu_ref) = menu_ref.as_ref() {
                                    let emitter = menu_ref.signal_emitter();
                                    let _ = DBusMenuInterface::layout_updated(emitter, rev, 0).await;
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
                        dispatch_event(&async_app, &callback, event);
                    }
                    else => break,
                }
            }
        }
    })
    .detach();

    // Push initial state through the same codepath (signals/layout update).
    let _ = cmd_tx.send(Command::Update(linux_item));
    Ok(())
}

pub fn sync_tray(_cx: &mut gpui::App, mut item: TrayItem) -> Result<()> {
    let Some(handle) = LINUX_TRAY.get() else {
        return Ok(());
    };

    // Replace callback if provided.
    if let Some(cb) = item.event.take()
        && let Ok(mut slot) = handle.callback.lock()
    {
        *slot = Some(cb);
    }

    let linux_item =
        linux_item_from_tray_item(item).context("failed to build linux tray payload")?;
    let _ = handle.cmd_tx.send(Command::Update(linux_item));
    Ok(())
}
