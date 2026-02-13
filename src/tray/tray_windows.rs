use crate::tray::{TrayEvent, TrayItem, TrayMenuItem, TrayToggleType};
use anyhow::{Context as _, Result};
use gpui::{AsyncApp, MouseButton, Point};
use std::{
    cell::RefCell,
    collections::HashMap,
    ffi::OsStr,
    mem,
    os::windows::ffi::OsStrExt as _,
    ptr,
    sync::{Arc, Mutex, OnceLock},
};
use windows_sys::Win32::{
    Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT as WIN_POINT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Shell::{
            NOTIFYICONDATAW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
            NIM_SETVERSION, NOTIFYICON_VERSION_4, Shell_NotifyIconW,
        },
        WindowsAndMessaging::{
            AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
            DestroyWindow, GetCursorPos, LoadCursorW, LoadIconW, PostMessageW, PostQuitMessage,
            RegisterClassW, SetForegroundWindow, TrackPopupMenu, CREATESTRUCTW, CW_USEDEFAULT,
            HMENU, IDC_ARROW, IDI_APPLICATION, MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING,
            MF_UNCHECKED, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, WM_APP, WM_COMMAND,
            WM_CREATE, WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPEDWINDOW,
        },
    },
};

const TRAY_CALLBACK_MESSAGE: u32 = WM_APP + 1;
const WM_TRAY_OPEN_MENU: u32 = WM_APP + 2;

#[derive(Clone)]
struct Handler {
    async_app: AsyncApp,
    callback: Arc<Mutex<Option<Box<dyn FnMut(TrayEvent, &mut gpui::App) + Send + 'static>>>>,
    id_to_menu_id: Arc<Mutex<HashMap<u16, String>>>,
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

    fn dispatch_command(&self, cmd: u16) {
        let id = self
            .id_to_menu_id
            .lock()
            .ok()
            .and_then(|m| m.get(&cmd).cloned());
        if let Some(id) = id {
            self.dispatch(TrayEvent::MenuClick { id });
        }
    }
}

struct Tray {
    handler: Handler,
    hwnd: HWND,
    menu: HMENU,
    icon_added: bool,
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = self.delete_icon();
            if self.hwnd != 0 {
                DestroyWindow(self.hwnd);
            }
            if self.menu != 0 {
                DestroyMenu(self.menu);
            }
        }
    }
}

thread_local! {
    static TRAY: RefCell<Option<Box<Tray>>> = const { RefCell::new(None) };
}

fn to_wide_null(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

fn class_name() -> &'static [u16] {
    static NAME: OnceLock<Vec<u16>> = OnceLock::new();
    NAME.get_or_init(|| to_wide_null("GpuiTrayHiddenWindow")).as_slice()
}

unsafe fn tray_from_window(hwnd: HWND) -> Option<&'static mut Tray> {
    let state_ptr = windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
        hwnd,
        windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
    ) as *mut Tray;
    state_ptr.as_mut()
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CREATE => {
            let create = lparam as *const CREATESTRUCTW;
            if let Some(create) = create.as_ref() {
                windows_sys::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
                    hwnd,
                    windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
                    create.lpCreateParams as isize,
                );
            }
            0
        }
        TRAY_CALLBACK_MESSAGE => {
            let event = lparam as u32;
            if event == WM_RBUTTONUP {
                let _ = PostMessageW(hwnd, WM_TRAY_OPEN_MENU, 0, 0);
            } else if event == WM_LBUTTONUP {
                let _ = PostMessageW(hwnd, WM_TRAY_OPEN_MENU, 1, 0);
            }
            0
        }
        WM_TRAY_OPEN_MENU => {
            let Some(tray) = tray_from_window(hwnd) else {
                return 0;
            };

            let mut point = WIN_POINT { x: 0, y: 0 };
            let _ = GetCursorPos(&mut point);

            if wparam == 1 {
                tray.handler.dispatch(TrayEvent::TrayClick {
                    button: MouseButton::Left,
                    position: Point {
                        x: point.x,
                        y: point.y,
                    },
                });
                return 0;
            }

            let _ = SetForegroundWindow(hwnd);
            let command = TrackPopupMenu(
                tray.menu,
                TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD,
                point.x,
                point.y,
                0,
                hwnd,
                ptr::null(),
            );

            if command != 0 {
                let _ = PostMessageW(hwnd, WM_COMMAND, command as usize, 0);
            }

            0
        }
        WM_COMMAND => {
            let Some(tray) = tray_from_window(hwnd) else {
                return 0;
            };

            let id = (wparam & 0xffff) as u16;
            tray.handler.dispatch_command(id);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

fn register_window_class(instance: HINSTANCE) -> Result<()> {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    if REGISTERED.get().is_some() {
        return Ok(());
    }

    unsafe {
        let class = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: 0,
            hCursor: LoadCursorW(0, IDC_ARROW),
            hbrBackground: 0,
            lpszMenuName: ptr::null(),
            lpszClassName: class_name().as_ptr(),
        };
        let atom = RegisterClassW(&class);
        if atom == 0 {
            anyhow::bail!("RegisterClassW failed");
        }
    }

    let _ = REGISTERED.set(());
    Ok(())
}

impl Tray {
    unsafe fn notify_data(&self, tooltip: &str) -> NOTIFYICONDATAW {
        let mut data: NOTIFYICONDATAW = mem::zeroed();
        data.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.hwnd;
        data.uID = 1;
        data.uFlags = NIF_MESSAGE | NIF_TIP | NIF_ICON;
        data.uCallbackMessage = TRAY_CALLBACK_MESSAGE;

        data.hIcon = LoadIconW(0, IDI_APPLICATION);

        let tooltip_wide = to_wide_null(tooltip);
        let copy_len = (tooltip_wide.len().saturating_sub(1)).min(data.szTip.len() - 1);
        data.szTip[..copy_len].copy_from_slice(&tooltip_wide[..copy_len]);
        data.szTip[copy_len] = 0;

        data
    }

    unsafe fn add_icon(&mut self, tooltip: &str) -> Result<()> {
        if self.icon_added {
            return Ok(());
        }

        let data = self.notify_data(tooltip);
        let ok = Shell_NotifyIconW(NIM_ADD, &data);
        (ok != 0)
            .then_some(())
            .context("Shell_NotifyIconW(NIM_ADD) failed")?;

        let mut data_version = data;
        data_version.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &data_version);

        self.icon_added = true;
        Ok(())
    }

    unsafe fn delete_icon(&mut self) -> Result<()> {
        if !self.icon_added {
            return Ok(());
        }

        let data = self.notify_data("");
        let ok = Shell_NotifyIconW(NIM_DELETE, &data);
        (ok != 0)
            .then_some(())
            .context("Shell_NotifyIconW(NIM_DELETE) failed")?;

        self.icon_added = false;
        Ok(())
    }

    unsafe fn modify_icon(&mut self, tooltip: &str) -> Result<()> {
        if !self.icon_added {
            return Ok(());
        }

        let data = self.notify_data(tooltip);
        let ok = Shell_NotifyIconW(NIM_MODIFY, &data);
        (ok != 0)
            .then_some(())
            .context("Shell_NotifyIconW(NIM_MODIFY) failed")?;
        Ok(())
    }

    unsafe fn rebuild_menu(&mut self, items: &[TrayMenuItem]) -> Result<()> {
        if self.menu != 0 {
            DestroyMenu(self.menu);
        }

        let menu = CreatePopupMenu();
        (menu != 0)
            .then_some(())
            .context("CreatePopupMenu failed")?;

        if let Ok(mut map) = self.handler.id_to_menu_id.lock() {
            map.clear();
        }

        let mut next_id: u16 = 1000;
        for item in items {
            append_tray_menu_item(menu, item, &self.handler.id_to_menu_id, &mut next_id)?;
        }

        self.menu = menu;
        Ok(())
    }

    unsafe fn sync(&mut self, item: TrayItem) -> Result<()> {
        if let Some(cb) = item.event {
            if let Ok(mut slot) = self.handler.callback.lock() {
                *slot = Some(cb);
            }
        }

        self.rebuild_menu(&item.submenus)?;

        if item.visible {
            self.add_icon(item.tooltip.as_str())?;
            self.modify_icon(item.tooltip.as_str())?;
        } else {
            self.delete_icon()?;
        }

        Ok(())
    }
}

unsafe fn append_tray_menu_item(
    menu: HMENU,
    item: &TrayMenuItem,
    id_to_menu_id: &Arc<Mutex<HashMap<u16, String>>>,
    next_id: &mut u16,
) -> Result<()> {
    match item {
        TrayMenuItem::Separator { .. } => {
            let _: BOOL = AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
        }
        TrayMenuItem::Submenu {
            id,
            label,
            toggle_type,
            children,
        } => {
            if children.is_empty() {
                let cmd = *next_id;
                *next_id = next_id.wrapping_add(1).max(1000);

                if let Ok(mut map) = id_to_menu_id.lock() {
                    map.insert(cmd, id.clone());
                }

                let label_w = to_wide_null(label);
                let mut flags = MF_STRING;
                let checked = match toggle_type {
                    Some(TrayToggleType::Checkbox(b)) => *b,
                    Some(TrayToggleType::Radio(b)) => *b,
                    None => false,
                };
                flags |= if checked { MF_CHECKED } else { MF_UNCHECKED };

                let _: BOOL = AppendMenuW(menu, flags, cmd as usize, label_w.as_ptr());
            } else {
                let submenu = CreatePopupMenu();
                (submenu != 0)
                    .then_some(())
                    .context("CreatePopupMenu(submenu) failed")?;
                for child in children {
                    append_tray_menu_item(submenu, child, id_to_menu_id, next_id)?;
                }

                let label_w = to_wide_null(label);
                let _: BOOL = AppendMenuW(menu, MF_POPUP, submenu as usize, label_w.as_ptr());
            }
        }
    }

    Ok(())
}

pub fn set_up_tray(cx: &mut gpui::App, async_app: AsyncApp, mut item: TrayItem) -> Result<()> {
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    (instance != 0)
        .then_some(())
        .context("GetModuleHandleW failed")?;

    register_window_class(instance)?;

    TRAY.with(|tray_cell| {
        let mut tray_slot = tray_cell
            .try_borrow_mut()
            .map_err(|_| anyhow::anyhow!("tray storage already borrowed"))?;
        if tray_slot.is_some() {
            anyhow::bail!("tray already initialized");
        }

        let callback = Arc::new(Mutex::new(item.event.take()));
        let id_to_menu_id = Arc::new(Mutex::new(HashMap::new()));
        let handler = Handler {
            async_app,
            callback,
            id_to_menu_id,
        };

        let menu = unsafe { CreatePopupMenu() };
        (menu != 0)
            .then_some(())
            .context("CreatePopupMenu failed")?;

        let mut tray = Box::new(Tray {
            handler,
            hwnd: 0,
            menu,
            icon_added: false,
        });

        unsafe {
            let hwnd = CreateWindowExW(
                0,
                class_name().as_ptr(),
                class_name().as_ptr(),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                0,
                0,
                instance,
                tray.as_mut() as *mut Tray as *mut _,
            );
            (hwnd != 0)
                .then_some(())
                .context("CreateWindowExW failed")?;
            tray.hwnd = hwnd;
        }

        *tray_slot = Some(tray);
        Ok(())
    })?;

    sync_tray(cx, item)
}

pub fn sync_tray(cx: &mut gpui::App, item: TrayItem) -> Result<()> {
    TRAY.with(|tray_cell| {
        let mut tray_slot = tray_cell
            .try_borrow_mut()
            .map_err(|_| anyhow::anyhow!("tray storage already borrowed"))?;
        let tray = tray_slot
            .as_mut()
            .context("tray has not been initialized")?;
        unsafe { tray.sync(item) }
    })
}

