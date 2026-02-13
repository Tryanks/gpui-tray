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
    Foundation::{BOOL, HMODULE, HWND, LPARAM, LRESULT, POINT as WIN_POINT, WPARAM},
    Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
        DeleteObject,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Shell::{
            NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
            NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
        },
        WindowsAndMessaging::{
            AppendMenuW, CREATESTRUCTW, CW_USEDEFAULT, CreateIconIndirect, CreatePopupMenu,
            CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, GetCursorPos,
            HICON, HMENU, ICONINFO, IDC_ARROW, IDI_APPLICATION, LoadCursorW, LoadIconW, MF_CHECKED,
            MF_POPUP, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, PostMessageW, PostQuitMessage,
            RegisterClassW, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD,
            TPM_RIGHTBUTTON, TrackPopupMenu, WM_COMMAND, WM_CONTEXTMENU, WM_CREATE, WM_DESTROY,
            WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_NULL, WM_RBUTTONDOWN, WM_RBUTTONUP,
            WM_USER, WNDCLASSW, WS_OVERLAPPEDWINDOW,
        },
    },
};

// Tray callback must be in WM_USER..0x7FFF per Shell_NotifyIconW requirements.
const TRAY_CALLBACK_MESSAGE: u32 = WM_USER + 1;
const WM_TRAY_OPEN_MENU: u32 = WM_USER + 2;

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
    hicon: HICON,
    hicon_owned: bool,
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = self.delete_icon();
            if self.hicon_owned && self.hicon != 0 {
                DestroyIcon(self.hicon);
                self.hicon = 0;
                self.hicon_owned = false;
            }
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
    NAME.get_or_init(|| to_wide_null("GpuiTrayHiddenWindow"))
        .as_slice()
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
            let event = (lparam as u32) & 0xFFFF;
            if event == WM_RBUTTONUP || event == WM_RBUTTONDOWN || event == WM_CONTEXTMENU {
                let _ = PostMessageW(hwnd, WM_TRAY_OPEN_MENU, 0, 0);
            } else if event == WM_LBUTTONUP
                || event == WM_LBUTTONDOWN
                || event == WM_LBUTTONDBLCLK
                || event == NIN_SELECT
            {
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
                TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                hwnd,
                ptr::null(),
            );

            if command != 0 {
                let _ = PostMessageW(hwnd, WM_COMMAND, command as usize, 0);
            }
            let _ = PostMessageW(hwnd, WM_NULL, 0, 0);

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

fn register_window_class(instance: HMODULE) -> Result<()> {
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
    unsafe fn notify_data(&self, item: &TrayItem) -> NOTIFYICONDATAW {
        let mut data: NOTIFYICONDATAW = mem::zeroed();
        data.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.hwnd;
        data.uID = 1;
        data.uFlags = NIF_MESSAGE | NIF_TIP | NIF_ICON;
        data.uCallbackMessage = TRAY_CALLBACK_MESSAGE;

        data.hIcon = if self.hicon != 0 {
            self.hicon
        } else {
            LoadIconW(0, IDI_APPLICATION)
        };

        let tooltip_wide = to_wide_null(item.tooltip.as_str());
        let copy_len = (tooltip_wide.len().saturating_sub(1)).min(data.szTip.len() - 1);
        data.szTip[..copy_len].copy_from_slice(&tooltip_wide[..copy_len]);
        data.szTip[copy_len] = 0;

        data
    }

    unsafe fn add_icon(&mut self, item: &TrayItem) -> Result<()> {
        if self.icon_added {
            return Ok(());
        }

        let data = self.notify_data(item);
        let ok = Shell_NotifyIconW(NIM_ADD, &data);
        (ok != 0)
            .then_some(())
            .context("Shell_NotifyIconW(NIM_ADD) failed")?;

        let mut data_version = data;
        // `uVersion` lives in an anonymous union in windows-sys 0.48
        unsafe {
            data_version.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        }
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &data_version);

        self.icon_added = true;
        Ok(())
    }

    unsafe fn delete_icon(&mut self) -> Result<()> {
        if !self.icon_added {
            return Ok(());
        }

        let mut dummy = TrayItem::new();
        dummy.tooltip = String::new();
        let data = self.notify_data(&dummy);
        let ok = Shell_NotifyIconW(NIM_DELETE, &data);
        (ok != 0)
            .then_some(())
            .context("Shell_NotifyIconW(NIM_DELETE) failed")?;

        self.icon_added = false;
        Ok(())
    }

    unsafe fn modify_icon(&mut self, item: &TrayItem) -> Result<()> {
        if !self.icon_added {
            return Ok(());
        }

        let data = self.notify_data(item);
        let ok = Shell_NotifyIconW(NIM_MODIFY, &data);
        (ok != 0)
            .then_some(())
            .context("Shell_NotifyIconW(NIM_MODIFY) failed")?;
        Ok(())
    }

    unsafe fn set_icon(&mut self, icon: Option<&gpui::Image>) -> Result<()> {
        let (width, height, bgra) = match icon {
            None => {
                if self.hicon_owned && self.hicon != 0 {
                    DestroyIcon(self.hicon);
                }
                self.hicon = 0;
                self.hicon_owned = false;
                return Ok(());
            }
            Some(image) => crate::icon::decode_gpui_image_to_bgra32(image)
                .context("failed to decode gpui::Image")?,
        };

        let new_hicon = hicon_from_bgra32(width, height, &bgra)?;
        if self.hicon_owned && self.hicon != 0 {
            DestroyIcon(self.hicon);
        }
        self.hicon = new_hicon;
        self.hicon_owned = true;
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
            self.set_icon(item.icon.as_deref())?;
            self.add_icon(&item)?;
            self.modify_icon(&item)?;
        } else {
            self.delete_icon()?;
        }

        Ok(())
    }
}

unsafe fn hicon_from_bgra32(width: u32, height: u32, bgra: &[u8]) -> Result<HICON> {
    let (w, h) = (width as usize, height as usize);
    let expected = w
        .checked_mul(h)
        .and_then(|px| px.checked_mul(4))
        .context("icon dimensions overflow")?;
    anyhow::ensure!(
        bgra.len() == expected,
        "icon bytes length mismatch: got {}, expected {} ({}x{}x4)",
        bgra.len(),
        expected,
        width,
        height
    );

    let mut bmi: BITMAPINFO = mem::zeroed();
    bmi.bmiHeader = BITMAPINFOHEADER {
        biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width as i32,
        // Negative height = top-down, so we don't need to flip rows.
        biHeight: -(height as i32),
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB as u32,
        biSizeImage: 0,
        biXPelsPerMeter: 0,
        biYPelsPerMeter: 0,
        biClrUsed: 0,
        biClrImportant: 0,
    };

    let mut bits_ptr: *mut core::ffi::c_void = ptr::null_mut();
    let color_bmp = CreateDIBSection(0, &bmi, DIB_RGB_COLORS, &mut bits_ptr, 0, 0);
    anyhow::ensure!(color_bmp != 0, "CreateDIBSection failed");
    anyhow::ensure!(
        !bits_ptr.is_null(),
        "CreateDIBSection returned null bits pointer"
    );
    ptr::copy_nonoverlapping(bgra.as_ptr(), bits_ptr.cast::<u8>(), bgra.len());

    // 1bpp mask bitmap must be initialized to 0 (opaque). Row is padded to 32 bits.
    let mask_stride = ((w + 31) / 32) * 4;
    let mask_bytes = vec![0u8; mask_stride * h];
    let mask_bmp = CreateBitmap(
        width as i32,
        height as i32,
        1,
        1,
        mask_bytes.as_ptr().cast(),
    );
    anyhow::ensure!(mask_bmp != 0, "CreateBitmap(mask) failed");

    let mut ii: ICONINFO = mem::zeroed();
    ii.fIcon = 1;
    ii.xHotspot = 0;
    ii.yHotspot = 0;
    ii.hbmColor = color_bmp;
    ii.hbmMask = mask_bmp;

    let hicon = CreateIconIndirect(&ii);
    // The icon copies the bitmaps; we can delete them afterwards.
    let _ = DeleteObject(color_bmp);
    let _ = DeleteObject(mask_bmp);

    anyhow::ensure!(hicon != 0, "CreateIconIndirect failed");
    Ok(hicon)
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
