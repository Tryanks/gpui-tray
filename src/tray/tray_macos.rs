#![allow(unsafe_op_in_unsafe_fn)]

use crate::tray::{TrayEvent, TrayItem, TrayMenuItem, TrayToggleType};
use anyhow::{Context as _, Result};
use gpui::AsyncApp;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, NSObject, Sel};
use objc2::{AnyThread, ClassType, MainThreadMarker, MainThreadOnly, msg_send, sel};
use objc2_app_kit::{
    NSCellImagePosition, NSControlStateValueOff, NSControlStateValueOn, NSImage, NSMenu,
    NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSData, NSSize, NSString};
use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::c_void,
    sync::{Arc, Mutex, OnceLock},
};

fn with_pool<T>(f: impl FnOnce() -> T) -> T {
    autoreleasepool(|_| f())
}

fn mtm() -> Result<MainThreadMarker> {
    MainThreadMarker::new().context("AppKit usage requires running on the main thread")
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

fn target_class() -> Result<&'static AnyClass> {
    static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();

    if let Some(class) = CLASS.get() {
        return Ok(class);
    }

    if let Some(existing) = AnyClass::get(c"GpuiTrayTarget") {
        let _ = CLASS.set(existing);
        return Ok(existing);
    }

    let class = {
        let mut builder = ClassBuilder::new(c"GpuiTrayTarget", NSObject::class())
            .context("failed to create Objective-C class declaration")?;

        builder.add_ivar::<*mut c_void>(c"rust_state");

        extern "C" fn on_menu_item(this: *mut NSObject, _cmd: Sel, sender: *mut AnyObject) {
            unsafe {
                let Some(this) = this.as_ref() else {
                    return;
                };
                if sender.is_null() {
                    return;
                }
                let cls = AnyClass::get(c"GpuiTrayTarget").unwrap();
                let ivar = cls.instance_variable(c"rust_state").unwrap();
                let state_ptr = *ivar.load::<*mut c_void>(this);
                if state_ptr.is_null() {
                    return;
                }

                let tag: i64 = msg_send![sender, tag];
                let state = &*(state_ptr as *const TargetState);
                state.handler.dispatch_tag(tag);
            }
        }

        extern "C" fn dealloc(this: *mut NSObject, _cmd: Sel) {
            unsafe {
                let Some(this_ref) = this.as_ref() else {
                    return;
                };
                let cls = AnyClass::get(c"GpuiTrayTarget").unwrap();
                let ivar = cls.instance_variable(c"rust_state").unwrap();
                let state_ptr = *ivar.load::<*mut c_void>(this_ref);
                if !state_ptr.is_null() {
                    drop(Box::from_raw(state_ptr as *mut TargetState));
                    *ivar.load_ptr::<*mut c_void>(this_ref) = std::ptr::null_mut::<c_void>();
                }
                // Super calls require a mutable message receiver.
                let this_any: &mut AnyObject = &mut *this.cast::<AnyObject>();
                let _: () = msg_send![super(this_any, NSObject::class()), dealloc];
            }
        }

        unsafe {
            let on_menu_item_fn: extern "C" fn(*mut NSObject, Sel, *mut AnyObject) = on_menu_item;
            let dealloc_fn: extern "C" fn(*mut NSObject, Sel) = dealloc;
            builder.add_method::<NSObject, _>(sel!(onMenuItem:), on_menu_item_fn);
            builder.add_method::<NSObject, _>(sel!(dealloc), dealloc_fn);
        }

        builder.register()
    };

    let _ = CLASS.set(class);
    Ok(class)
}

struct Tray {
    mtm: MainThreadMarker,
    status_item: Option<Retained<NSStatusItem>>,
    menu: Retained<NSMenu>,
    target: Retained<AnyObject>,
    handler: Handler,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

impl Drop for Tray {
    fn drop(&mut self) {
        if let Some(item) = self.status_item.take() {
            NSStatusBar::systemStatusBar().removeStatusItem(&item);
        }
    }
}

pub fn set_up_tray(cx: &mut gpui::App, async_app: AsyncApp, mut item: TrayItem) -> Result<()> {
    with_pool(|| unsafe {
        let mtm = mtm()?;
        let menu = NSMenu::new(mtm);

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
        let state_ptr = Box::into_raw(state) as *mut c_void;
        let target: Retained<AnyObject> = msg_send![target_class, new];
        let ivar = target_class.instance_variable(c"rust_state").unwrap();
        *ivar.load_ptr::<*mut c_void>(&*target) = state_ptr;

        TRAY.with(|tray_cell| {
            let mut tray_slot = tray_cell
                .try_borrow_mut()
                .map_err(|_| anyhow::anyhow!("tray storage already borrowed"))?;
            if tray_slot.is_some() {
                anyhow::bail!("tray already initialized");
            }
            *tray_slot = Some(Tray {
                mtm,
                status_item: None,
                menu,
                target,
                handler,
            });
            Ok(())
        })?;

        sync_tray(cx, item)
    })
}

pub fn sync_tray(_cx: &mut gpui::App, mut item: TrayItem) -> Result<()> {
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

        let status_item = self.status_item.as_ref().context("status item is nil")?;
        status_item.setMenu(Some(&self.menu));

        let button = status_item
            .button(self.mtm)
            .context("status item button is nil")?;

        let tooltip = NSString::from_str(item.tooltip.as_str());
        button.setToolTip(Some(&tooltip));

        let title = NSString::from_str(item.title.as_str());
        button.setTitle(&title);

        let nsimage = item.icon.as_deref().map(nsimage_from_image).transpose()?;

        if let Some(nsimage) = nsimage {
            let new_size = NSSize::new(18., 18.);
            button.setImage(Some(&nsimage));
            nsimage.setSize(new_size);
            button.setImagePosition(NSCellImagePosition::ImageLeft);
            nsimage.setTemplate(true);
        } else {
            button.setImage(None);
        }

        Ok(())
    }

    fn set_visible(&mut self, visible: bool) -> Result<()> {
        if visible {
            if self.status_item.is_some() {
                return Ok(());
            }

            let status_bar = NSStatusBar::systemStatusBar();
            let status_item = status_bar.statusItemWithLength(NSVariableStatusItemLength);
            self.status_item = Some(status_item);
        } else {
            if self.status_item.is_none() {
                return Ok(());
            }
            if let Some(item) = self.status_item.take() {
                NSStatusBar::systemStatusBar().removeStatusItem(&item);
            }
        }

        Ok(())
    }

    fn rebuild_menu(&mut self, items: &[TrayMenuItem]) -> Result<()> {
        with_pool(|| unsafe {
            self.menu.removeAllItems();

            if let Ok(mut map) = self.handler.tag_to_id.lock() {
                map.clear();
            }

            let mut next_tag: i64 = 1;
            for item in items {
                add_tray_menu_item(
                    &self.menu,
                    item,
                    &self.handler,
                    &self.target,
                    self.mtm,
                    &mut next_tag,
                )?;
            }

            Ok(())
        })
    }
}

fn nsimage_from_image(image: &gpui::Image) -> Result<Retained<NSImage>> {
    let nsdata = unsafe {
        NSData::dataWithBytes_length(image.bytes.as_ptr().cast(), image.bytes.len() as _)
    };
    NSImage::initWithData(NSImage::alloc(), &nsdata)
        .context("failed to create NSImage from gpui::Image bytes")
}

unsafe fn add_tray_menu_item(
    menu: &NSMenu,
    item: &TrayMenuItem,
    handler: &Handler,
    target: &AnyObject,
    mtm: MainThreadMarker,
    next_tag: &mut i64,
) -> Result<()> {
    match item {
        TrayMenuItem::Separator { .. } => {
            let separator = NSMenuItem::separatorItem(mtm);
            menu.addItem(&separator);
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

                let title = NSString::from_str(label.as_str());
                let key_equiv = NSString::from_str("");
                let menu_item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(mtm),
                        &title,
                        Some(sel!(onMenuItem:)),
                        &key_equiv,
                    )
                };

                unsafe { menu_item.setTarget(Some(target)) };
                menu_item.setTag(tag as _);

                let checked = match toggle_type {
                    Some(TrayToggleType::Checkbox(checked)) => *checked,
                    Some(TrayToggleType::Radio(checked)) => *checked,
                    None => false,
                };
                let state_value = if checked {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                };
                menu_item.setState(state_value);

                menu.addItem(&menu_item);
            } else {
                let title = NSString::from_str(label.as_str());
                let key_equiv = NSString::from_str("");

                let submenu_item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(mtm),
                        &title,
                        None,
                        &key_equiv,
                    )
                };

                let submenu = NSMenu::new(mtm);
                for child in children {
                    add_tray_menu_item(&submenu, child, handler, target, mtm, next_tag)?;
                }

                submenu_item.setSubmenu(Some(&submenu));
                menu.addItem(&submenu_item);
            }
        }
    }

    Ok(())
}
