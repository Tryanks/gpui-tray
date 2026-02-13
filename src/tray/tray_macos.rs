#![allow(unsafe_op_in_unsafe_fn)]

use crate::tray::{TrayEvent, TrayItem, TrayMenuItem, TrayToggleType};
use anyhow::{Context as _, Result};
use cocoa::{
    appkit::{NSMenu, NSMenuItem, NSStatusBar, NSVariableStatusItemLength},
    base::{id, nil},
    foundation::{NSData, NSAutoreleasePool, NSSize, NSString},
};
use gpui::AsyncApp;
use objc::{
    class,
    declare::ClassDecl,
    msg_send,
    runtime::{Class, Object, Sel},
    sel,
    sel_impl,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::c_void,
    fs::OpenOptions,
    io::Write as _,
    sync::{Arc, Mutex, OnceLock},
};

const APP_ICON_PNG: &[u8] = include_bytes!("../image/app-icon.png");

#[allow(dead_code)]
#[repr(i64)]
enum CellImagePosition {
    ImageOnly = 1,
    ImageLeft = 2,
    ImageRight = 3,
}

fn with_pool<T>(f: impl FnOnce() -> T) -> T {
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let result = f();
        let _: () = msg_send![pool, drain];
        result
    }
}

fn debug_log(message: &str) {
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/gpui_tray_demo_tray.log")
    {
        if let Err(error) = writeln!(file, "{message}") {
            eprintln!("failed to write tray debug log: {error:#}");
        }
    }
}

#[derive(Clone)]
struct Handler {
    async_app: AsyncApp,
    callback: Arc<Mutex<Option<Box<dyn FnMut(TrayEvent, &mut gpui::App) + Send + 'static>>>>,
    tag_to_id: Arc<Mutex<HashMap<i64, String>>>,
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

    fn dispatch_tag(&self, tag: i64) {
        let id = self
            .tag_to_id
            .lock()
            .ok()
            .and_then(|m| m.get(&tag).cloned());
        if let Some(id) = id {
            self.dispatch(TrayEvent::MenuClick { id });
        }
    }
}

struct TargetState {
    handler: Handler,
}

fn target_class() -> Result<&'static Class> {
    static CLASS: OnceLock<&'static Class> = OnceLock::new();

    if let Some(class) = CLASS.get() {
        return Ok(class);
    }

    if let Some(existing) = Class::get("GpuiTrayTarget") {
        let _ = CLASS.set(existing);
        return Ok(existing);
    }

    let class = unsafe {
        let superclass = class!(NSObject);
        let mut decl = ClassDecl::new("GpuiTrayTarget", superclass)
            .context("failed to create Objective-C class declaration")?;

        decl.add_ivar::<*mut c_void>("rust_state");

        extern "C" fn on_menu_item(this: &Object, _cmd: Sel, sender: id) {
            unsafe {
                let state_ptr: *mut c_void = *this.get_ivar("rust_state");
                if state_ptr.is_null() {
                    return;
                }

                let tag: i64 = msg_send![sender, tag];
                let state = &*(state_ptr as *const TargetState);
                state.handler.dispatch_tag(tag);
            }
        }

        extern "C" fn dealloc(this: &mut Object, _cmd: Sel) {
            unsafe {
                let state_ptr: *mut c_void = *this.get_ivar("rust_state");
                if !state_ptr.is_null() {
                    drop(Box::from_raw(state_ptr as *mut TargetState));
                    this.set_ivar("rust_state", std::ptr::null_mut::<c_void>());
                }
                let superclass = class!(NSObject);
                let _: () = msg_send![super(this, superclass), dealloc];
            }
        }

        decl.add_method(
            sel!(onMenuItem:),
            on_menu_item as extern "C" fn(&Object, Sel, id),
        );
        decl.add_method(sel!(dealloc), dealloc as extern "C" fn(&mut Object, Sel));

        decl.register()
    };

    let _ = CLASS.set(class);
    Ok(class)
}

struct Tray {
    status_item: id,
    menu: id,
    target: id,
    handler: Handler,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            if self.status_item != nil {
                let _: () = msg_send![
                    NSStatusBar::systemStatusBar(nil),
                    removeStatusItem: self.status_item
                ];
            }
            let _: () = msg_send![self.menu, release];
            let _: () = msg_send![self.target, release];
        }
    }
}

pub fn set_up_tray(cx: &mut gpui::App, async_app: AsyncApp, mut item: TrayItem) -> Result<()> {
    with_pool(|| unsafe {
        let menu = NSMenu::new(nil);
        let _: () = msg_send![menu, retain];

        let callback = Arc::new(Mutex::new(item.event.take()));
        let tag_to_id = Arc::new(Mutex::new(HashMap::new()));
        let handler = Handler {
            async_app,
            callback,
            tag_to_id,
        };

        let state = Box::new(TargetState {
            handler: handler.clone(),
        });

        let target_class = target_class()?;
        let target: id = msg_send![target_class, new];
        let _: () = msg_send![target, retain];

        let state_ptr = Box::into_raw(state) as *mut c_void;
        (*target).set_ivar("rust_state", state_ptr);

        TRAY.with(|tray_cell| {
            let mut tray_slot = tray_cell
                .try_borrow_mut()
                .map_err(|_| anyhow::anyhow!("tray storage already borrowed"))?;
            if tray_slot.is_some() {
                anyhow::bail!("tray already initialized");
            }
            *tray_slot = Some(Tray {
                status_item: nil,
                menu,
                target,
                handler,
            });
            Ok(())
        })?;

        sync_tray(cx, item)
    })
}

pub fn sync_tray(cx: &mut gpui::App, mut item: TrayItem) -> Result<()> {
    with_pool(|| {
        TRAY.with(|tray_cell| {
            let mut tray_slot = tray_cell
                .try_borrow_mut()
                .map_err(|_| anyhow::anyhow!("tray storage already borrowed"))?;
            let tray = tray_slot
                .as_mut()
                .context("tray has not been initialized")?;

            if let Some(cb) = item.event.take() {
                if let Ok(mut slot) = tray.handler.callback.lock() {
                    *slot = Some(cb);
                }
            }

            tray.update(&item)
        })
    })
}

impl Tray {
    fn update(&mut self, item: &TrayItem) -> Result<()> {
        self.set_visible(item.visible)?;
        if !item.visible {
            return Ok(());
        }

        self.rebuild_menu(&item.submenus)?;

        unsafe {
            let status_item = self.status_item;
            (status_item != nil)
                .then_some(())
                .context("status item is nil")?;

            let _: () = msg_send![status_item, setMenu: self.menu];

            let button: id = msg_send![status_item, button];
            (button != nil)
                .then_some(())
                .context("status item button is nil")?;

            let tooltip = NSString::alloc(nil).init_str(item.tooltip.as_str());
            let _: () = msg_send![button, setToolTip: tooltip];

            let title = NSString::alloc(nil).init_str(item.title.as_str());
            let _: () = msg_send![button, setTitle: title];

            // Note: keep using an embedded PNG icon for simplicity.
            let nsdata =
                NSData::dataWithBytes_length_(nil, APP_ICON_PNG.as_ptr() as *const _, APP_ICON_PNG.len() as u64);
            let nsimage: id = msg_send![class!(NSImage), alloc];
            let nsimage: id = msg_send![nsimage, initWithData: nsdata];
            (nsimage != nil)
                .then_some(())
                .context("failed to create NSImage from icon bytes")?;

            let new_size = NSSize::new(18., 18.);
            let _: () = msg_send![button, setImage: nsimage];
            let _: () = msg_send![nsimage, setSize: new_size];
            let _: () = msg_send![button, setImagePosition: CellImagePosition::ImageLeft];
            let _: () = msg_send![nsimage, setTemplate: true];
        }

        Ok(())
    }

    fn set_visible(&mut self, visible: bool) -> Result<()> {
        if visible {
            if self.status_item != nil {
                return Ok(());
            }

            unsafe {
                let status_item: id = NSStatusBar::systemStatusBar(nil)
                    .statusItemWithLength_(NSVariableStatusItemLength);
                let _: () = msg_send![status_item, retain];
                self.status_item = status_item;
                debug_log(&format!("tray: created status item: {status_item:?}"));
            }
        } else {
            if self.status_item == nil {
                return Ok(());
            }
            unsafe {
                let _: () = msg_send![
                    NSStatusBar::systemStatusBar(nil),
                    removeStatusItem: self.status_item
                ];
            }
            self.status_item = nil;
        }

        Ok(())
    }

    fn rebuild_menu(&mut self, items: &[TrayMenuItem]) -> Result<()> {
        with_pool(|| unsafe {
            let _: () = msg_send![self.menu, removeAllItems];

            if let Ok(mut map) = self.handler.tag_to_id.lock() {
                map.clear();
            }

            let mut next_tag: i64 = 1;
            for item in items {
                add_tray_menu_item(
                    self.menu,
                    item,
                    &self.handler,
                    self.target,
                    &mut next_tag,
                )?;
            }

            Ok(())
        })
    }
}

unsafe fn add_tray_menu_item(
    menu: id,
    item: &TrayMenuItem,
    handler: &Handler,
    target: id,
    next_tag: &mut i64,
) -> Result<()> {
    match item {
        TrayMenuItem::Separator { .. } => {
            let separator: id = NSMenuItem::separatorItem(nil);
            let _: () = msg_send![menu, addItem: separator];
        }
        TrayMenuItem::Submenu {
            id: user_id,
            label,
            toggle_type,
            children,
        } => {
            if children.is_empty() {
                let tag = *next_tag;
                *next_tag += 1;

                if let Ok(mut map) = handler.tag_to_id.lock() {
                    map.insert(tag, user_id.clone());
                }

                let title = NSString::alloc(nil).init_str(label.as_str());
                let key_equiv = NSString::alloc(nil).init_str("");

                let item: id = msg_send![class!(NSMenuItem), alloc];
                let item: id = msg_send![
                    item,
                    initWithTitle: title
                    action: sel!(onMenuItem:)
                    keyEquivalent: key_equiv
                ];
                (item != nil)
                    .then_some(())
                    .context("failed to create NSMenuItem")?;

                let _: () = msg_send![item, setTarget: target];
                let _: () = msg_send![item, setTag: tag];

                let checked = match toggle_type {
                    Some(TrayToggleType::Checkbox(checked)) => *checked,
                    Some(TrayToggleType::Radio(checked)) => *checked,
                    None => false,
                };
                let state_value = if checked { 1i64 } else { 0i64 };
                let _: () = msg_send![item, setState: state_value];

                let _: () = msg_send![menu, addItem: item];
                let _: () = msg_send![item, release];
            } else {
                let title = NSString::alloc(nil).init_str(label.as_str());
                let key_equiv = NSString::alloc(nil).init_str("");

                let submenu_item: id = msg_send![class!(NSMenuItem), alloc];
                let submenu_item: id = msg_send![
                    submenu_item,
                    initWithTitle: title
                    action: std::ptr::null::<c_void>()
                    keyEquivalent: key_equiv
                ];
                (submenu_item != nil)
                    .then_some(())
                    .context("failed to create submenu NSMenuItem")?;

                let submenu = NSMenu::new(nil);
                for child in children {
                    add_tray_menu_item(submenu, child, handler, target, next_tag)?;
                }

                let _: () = msg_send![submenu_item, setSubmenu: submenu];
                let _: () = msg_send![menu, addItem: submenu_item];
                let _: () = msg_send![submenu_item, release];
            }
        }
    }

    Ok(())
}

