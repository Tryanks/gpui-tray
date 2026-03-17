use crate::tray::{
    TrayClickAction, TrayClickKind, TrayClickPolicy, TrayEvent, TrayEventCallback,
    TrayEventCallbackSlot, TrayMenuItem, TrayRuntimeState, TrayState, TrayToggleType,
    VersionedTrayState,
};
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
    Foundation::{HMODULE, HWND, LPARAM, LRESULT, POINT as WIN_POINT, WPARAM},
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
            MF_DISABLED, MF_POPUP, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, PostMessageW,
            PostQuitMessage, RegisterClassW, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
            TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_COMMAND, WM_CONTEXTMENU, WM_CREATE,
            WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_USER, WNDCLASSW,
            WS_OVERLAPPEDWINDOW,
        },
    },
};
use windows_sys::core::BOOL;

#[derive(Clone, Default)]
pub struct TrayHandle;

impl TrayHandle {
    pub fn set_state(&self, state: TrayState) -> Result<()> {
        let async_app = TRAY_RUNTIME.with(|runtime_cell| -> Result<Option<AsyncApp>> {
            let mut runtime_slot = runtime_cell
                .try_borrow_mut()
                .map_err(|_| anyhow::anyhow!("tray runtime already borrowed"))?;
            let runtime = runtime_slot
                .as_mut()
                .context("tray has not been initialized")?;
            let should_schedule = runtime.state.set_desired_state(state);
            Ok(should_schedule.then(|| runtime.async_app.clone()))
        })?;

        if let Some(async_app) = async_app {
            schedule_flush(async_app);
        }

        Ok(())
    }

    pub fn flush_now(&self, _cx: &mut gpui::App) -> Result<()> {
        flush_runtime()
    }
}

// Tray callback must be in WM_USER..0x7FFF per Shell_NotifyIconW requirements.
const TRAY_CALLBACK_MESSAGE: u32 = WM_USER + 1;
const WM_TRAY_OPEN_MENU: u32 = WM_USER + 2;
const TRAY_CLICK_LEFT_SINGLE: usize = 0;
const TRAY_CLICK_RIGHT_SINGLE: usize = 1;
const TRAY_CLICK_LEFT_DOUBLE: usize = 2;

#[derive(Clone)]
struct Handler {
    async_app: AsyncApp,
    callback: TrayEventCallbackSlot,
    id_to_menu_id: Arc<Mutex<HashMap<u16, String>>>,
}

impl Handler {
    fn dispatch(&self, event: TrayEvent) {
        let async_app = self.async_app.clone();
        let callback = self.callback.clone();
        async_app
            .foreground_executor()
            .spawn(async move {
                async_app.update(|cx| {
                    if let Ok(mut slot) = callback.lock()
                        && let Some(cb) = slot.as_mut()
                    {
                        cb(event, cx);
                    }
                });
            })
            .detach();
    }

    fn dispatch_command(&self, cmd: u16) {
        let id = self
            .id_to_menu_id
            .lock()
            .ok()
            .and_then(|map| map.get(&cmd).cloned());
        if let Some(id) = id {
            self.dispatch(TrayEvent::MenuClick { id });
        }
    }
}

struct TrayPlatform {
    handler: Handler,
    hwnd: HWND,
    menu: HMENU,
    click_policy: TrayClickPolicy,
    icon_added: bool,
    hicon: HICON,
    hicon_owned: bool,
}

struct TrayRuntime {
    async_app: AsyncApp,
    state: TrayRuntimeState,
    platform: Option<Box<TrayPlatform>>,
    interaction_active: bool,
}

impl Drop for TrayPlatform {
    fn drop(&mut self) {
        unsafe {
            let _ = self.delete_icon();
            if self.hicon_owned && self.hicon != ptr::null_mut() {
                DestroyIcon(self.hicon);
                self.hicon = ptr::null_mut();
                self.hicon_owned = false;
            }
            if self.hwnd != ptr::null_mut() {
                DestroyWindow(self.hwnd);
            }
            if self.menu != ptr::null_mut() {
                DestroyMenu(self.menu);
            }
        }
    }
}

thread_local! {
    static TRAY_RUNTIME: RefCell<Option<TrayRuntime>> = const { RefCell::new(None) };
}

fn to_wide_null(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

fn class_name() -> &'static [u16] {
    static NAME: OnceLock<Vec<u16>> = OnceLock::new();
    NAME.get_or_init(|| to_wide_null("GpuiTrayHiddenWindow"))
        .as_slice()
}

unsafe fn tray_from_window(hwnd: HWND) -> Option<&'static mut TrayPlatform> {
    let state_ptr = windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
        hwnd,
        windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
    ) as *mut TrayPlatform;
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
            if event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
                let _ = PostMessageW(hwnd, WM_TRAY_OPEN_MENU, TRAY_CLICK_RIGHT_SINGLE, 0);
            } else if event == WM_LBUTTONUP || event == NIN_SELECT {
                let _ = PostMessageW(hwnd, WM_TRAY_OPEN_MENU, TRAY_CLICK_LEFT_SINGLE, 0);
            } else if event == WM_LBUTTONDBLCLK {
                let _ = PostMessageW(hwnd, WM_TRAY_OPEN_MENU, TRAY_CLICK_LEFT_DOUBLE, 0);
            }
            0
        }
        WM_TRAY_OPEN_MENU => {
            let _ = handle_tray_click(wparam);
            0
        }
        WM_COMMAND => {
            if let Some(tray) = tray_from_window(hwnd) {
                let id = (wparam & 0xffff) as u16;
                tray.handler.dispatch_command(id);
            }
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
            hIcon: ptr::null_mut(),
            hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            hbrBackground: ptr::null_mut(),
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

pub fn set_up_tray(
    cx: &mut gpui::App,
    async_app: AsyncApp,
    initial: TrayState,
    on_event: TrayEventCallback,
) -> Result<TrayHandle> {
    let instance = unsafe { GetModuleHandleW(ptr::null()) };
    (instance != ptr::null_mut())
        .then_some(())
        .context("GetModuleHandleW failed")?;

    register_window_class(instance)?;

    let callback = Arc::new(Mutex::new(Some(on_event)));
    let id_to_menu_id = Arc::new(Mutex::new(HashMap::new()));
    let handler = Handler {
        async_app: async_app.clone(),
        callback,
        id_to_menu_id,
    };

    let menu = unsafe { CreatePopupMenu() };
    (menu != ptr::null_mut())
        .then_some(())
        .context("CreatePopupMenu failed")?;

    let mut platform = Box::new(TrayPlatform {
        handler,
        hwnd: ptr::null_mut(),
        menu,
        click_policy: TrayClickPolicy::default(),
        icon_added: false,
        hicon: ptr::null_mut(),
        hicon_owned: false,
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
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            platform.as_mut() as *mut TrayPlatform as *const _,
        );
        (hwnd != ptr::null_mut())
            .then_some(())
            .context("CreateWindowExW failed")?;
        platform.hwnd = hwnd;
    }

    TRAY_RUNTIME.with(|runtime_cell| {
        let mut runtime_slot = runtime_cell
            .try_borrow_mut()
            .map_err(|_| anyhow::anyhow!("tray runtime already borrowed"))?;
        if runtime_slot.is_some() {
            anyhow::bail!("tray already initialized");
        }

        *runtime_slot = Some(TrayRuntime {
            async_app: async_app.clone(),
            state: TrayRuntimeState::new(initial),
            platform: Some(platform),
            interaction_active: false,
        });
        Ok(())
    })?;

    let handle = TrayHandle;
    handle.flush_now(cx)?;
    Ok(handle)
}

fn schedule_flush(async_app: AsyncApp) {
    async_app
        .foreground_executor()
        .spawn(async move {
            let _ = flush_runtime();
        })
        .detach();
}

fn handle_tray_click(click_code: usize) -> Result<()> {
    let mut platform = TRAY_RUNTIME.with(|runtime_cell| {
        let mut runtime_slot = runtime_cell
            .try_borrow_mut()
            .map_err(|_| anyhow::anyhow!("tray runtime already borrowed"))?;
        let runtime = runtime_slot
            .as_mut()
            .context("tray has not been initialized")?;
        runtime.interaction_active = true;
        runtime
            .platform
            .take()
            .context("tray platform missing during click")
    })?;

    let click_result = unsafe { platform.handle_click(click_code) };

    let async_app = TRAY_RUNTIME.with(|runtime_cell| -> Result<Option<AsyncApp>> {
        let mut runtime_slot = runtime_cell
            .try_borrow_mut()
            .map_err(|_| anyhow::anyhow!("tray runtime already borrowed"))?;
        let runtime = runtime_slot
            .as_mut()
            .context("tray has not been initialized")?;
        runtime.platform = Some(platform);
        runtime.interaction_active = false;
        Ok(runtime
            .state
            .has_pending_flush()
            .then(|| runtime.async_app.clone()))
    })?;

    if let Some(async_app) = async_app {
        schedule_flush(async_app);
    }

    click_result
}

fn flush_runtime() -> Result<()> {
    loop {
        let step = TRAY_RUNTIME.with(
            |runtime_cell| -> Result<Option<(Box<TrayPlatform>, VersionedTrayState)>> {
                let mut runtime_slot = runtime_cell
                    .try_borrow_mut()
                    .map_err(|_| anyhow::anyhow!("tray runtime already borrowed"))?;
                let runtime = runtime_slot
                    .as_mut()
                    .context("tray has not been initialized")?;

                if runtime.interaction_active {
                    return Ok(None);
                }

                let Some(versioned_state) = runtime.state.try_begin_flush() else {
                    return Ok(None);
                };

                let platform = runtime
                    .platform
                    .take()
                    .context("tray platform missing during flush")?;

                Ok(Some((platform, versioned_state)))
            },
        )?;

        let Some((mut platform, versioned_state)) = step else {
            return Ok(());
        };

        let apply_result = unsafe { platform.apply(&versioned_state.state) };

        let should_continue = TRAY_RUNTIME.with(|runtime_cell| -> Result<bool> {
            let mut runtime_slot = runtime_cell
                .try_borrow_mut()
                .map_err(|_| anyhow::anyhow!("tray runtime already borrowed"))?;
            let runtime = runtime_slot
                .as_mut()
                .context("tray has not been initialized")?;
            runtime.platform = Some(platform);

            if apply_result.is_ok() {
                Ok(runtime.state.finish_flush(versioned_state))
            } else {
                runtime.state.abort_flush();
                Ok(false)
            }
        })?;

        apply_result?;

        if !should_continue {
            return Ok(());
        }
    }
}

impl TrayPlatform {
    unsafe fn handle_click(&self, click_code: usize) -> Result<()> {
        let mut point = WIN_POINT { x: 0, y: 0 };
        let _ = GetCursorPos(&mut point);

        let (action, button, kind) = match click_code {
            TRAY_CLICK_LEFT_SINGLE => (
                self.click_policy.left,
                MouseButton::Left,
                TrayClickKind::Single,
            ),
            TRAY_CLICK_RIGHT_SINGLE => (
                self.click_policy.right,
                MouseButton::Right,
                TrayClickKind::Single,
            ),
            TRAY_CLICK_LEFT_DOUBLE => (
                self.click_policy.double_click,
                MouseButton::Left,
                TrayClickKind::Double,
            ),
            _ => return Ok(()),
        };

        match action {
            TrayClickAction::EmitEvent => {
                self.handler.dispatch(TrayEvent::TrayClick {
                    button,
                    kind,
                    position: Point {
                        x: point.x,
                        y: point.y,
                    },
                });
            }
            TrayClickAction::OpenMenu => {
                let _ = SetForegroundWindow(self.hwnd);
                let command = TrackPopupMenu(
                    self.menu,
                    TPM_LEFTALIGN | TPM_BOTTOMALIGN | TPM_RETURNCMD | TPM_RIGHTBUTTON,
                    point.x,
                    point.y,
                    0,
                    self.hwnd,
                    ptr::null(),
                );

                if command != 0 {
                    let _ = PostMessageW(self.hwnd, WM_COMMAND, command as usize, 0);
                }
                let _ = PostMessageW(self.hwnd, WM_NULL, 0, 0);
            }
            TrayClickAction::Ignore => {}
        }

        Ok(())
    }

    unsafe fn notify_data(&self, tooltip: &str) -> NOTIFYICONDATAW {
        let mut data: NOTIFYICONDATAW = mem::zeroed();
        data.cbSize = mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.hwnd;
        data.uID = 1;
        data.uFlags = NIF_MESSAGE | NIF_TIP | NIF_ICON;
        data.uCallbackMessage = TRAY_CALLBACK_MESSAGE;

        data.hIcon = if self.hicon != ptr::null_mut() {
            self.hicon
        } else {
            LoadIconW(ptr::null_mut(), IDI_APPLICATION)
        };

        let tooltip_wide = to_wide_null(tooltip);
        let copy_len = (tooltip_wide.len().saturating_sub(1)).min(data.szTip.len() - 1);
        data.szTip[..copy_len].copy_from_slice(&tooltip_wide[..copy_len]);
        data.szTip[copy_len] = 0;

        data
    }

    unsafe fn add_icon(&mut self, state: &TrayState) -> Result<()> {
        if self.icon_added {
            return Ok(());
        }

        let data = self.notify_data(state.tooltip.as_str());
        let ok = Shell_NotifyIconW(NIM_ADD, &data);
        (ok != 0)
            .then_some(())
            .context("Shell_NotifyIconW(NIM_ADD) failed")?;

        let mut data_version = data;
        data_version.Anonymous.uVersion = NOTIFYICON_VERSION_4;
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

    unsafe fn modify_icon(&mut self, state: &TrayState) -> Result<()> {
        if !self.icon_added {
            return Ok(());
        }

        let data = self.notify_data(state.tooltip.as_str());
        let ok = Shell_NotifyIconW(NIM_MODIFY, &data);
        (ok != 0)
            .then_some(())
            .context("Shell_NotifyIconW(NIM_MODIFY) failed")?;
        Ok(())
    }

    unsafe fn set_icon(&mut self, icon: Option<&gpui::Image>) -> Result<()> {
        let (width, height, bgra) = match icon {
            None => {
                if self.hicon_owned && self.hicon != ptr::null_mut() {
                    DestroyIcon(self.hicon);
                }
                self.hicon = ptr::null_mut();
                self.hicon_owned = false;
                return Ok(());
            }
            Some(image) => crate::icon::decode_gpui_image_to_bgra32(image)
                .context("failed to decode gpui::Image")?,
        };

        let new_hicon = hicon_from_bgra32(width, height, &bgra)?;
        if self.hicon_owned && self.hicon != ptr::null_mut() {
            DestroyIcon(self.hicon);
        }
        self.hicon = new_hicon;
        self.hicon_owned = true;
        Ok(())
    }

    unsafe fn rebuild_menu(&mut self, items: &[TrayMenuItem]) -> Result<()> {
        if self.menu != ptr::null_mut() {
            DestroyMenu(self.menu);
        }

        let menu = CreatePopupMenu();
        (menu != ptr::null_mut())
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

    unsafe fn apply(&mut self, state: &TrayState) -> Result<()> {
        self.click_policy = state.click_policy;
        self.rebuild_menu(&state.submenus)?;

        if state.visible {
            self.set_icon(state.icon.as_deref())?;
            self.add_icon(state)?;
            self.modify_icon(state)?;
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
    let color_bmp = CreateDIBSection(
        ptr::null_mut(),
        &bmi,
        DIB_RGB_COLORS,
        &mut bits_ptr,
        ptr::null_mut(),
        0,
    );
    anyhow::ensure!(color_bmp != ptr::null_mut(), "CreateDIBSection failed");
    anyhow::ensure!(
        !bits_ptr.is_null(),
        "CreateDIBSection returned null bits pointer"
    );
    ptr::copy_nonoverlapping(bgra.as_ptr(), bits_ptr.cast::<u8>(), bgra.len());

    let mask_stride = w.div_ceil(32) * 4;
    let mask_bytes = vec![0u8; mask_stride * h];
    let mask_bmp = CreateBitmap(
        width as i32,
        height as i32,
        1,
        1,
        mask_bytes.as_ptr().cast(),
    );
    anyhow::ensure!(mask_bmp != ptr::null_mut(), "CreateBitmap(mask) failed");

    let mut ii: ICONINFO = mem::zeroed();
    ii.fIcon = 1;
    ii.xHotspot = 0;
    ii.yHotspot = 0;
    ii.hbmColor = color_bmp;
    ii.hbmMask = mask_bmp;

    let hicon = CreateIconIndirect(&ii);
    let _ = DeleteObject(color_bmp);
    let _ = DeleteObject(mask_bmp);

    anyhow::ensure!(hicon != ptr::null_mut(), "CreateIconIndirect failed");
    Ok(hicon)
}

unsafe fn append_tray_menu_item(
    menu: HMENU,
    item: &TrayMenuItem,
    id_to_menu_id: &Arc<Mutex<HashMap<u16, String>>>,
    next_id: &mut u16,
) -> Result<()> {
    match item {
        TrayMenuItem::Separator { visible, .. } => {
            if !*visible {
                return Ok(());
            }
            let ok: BOOL = AppendMenuW(menu, MF_SEPARATOR, 0, ptr::null());
            if ok == 0 {
                anyhow::bail!("AppendMenuW(MF_SEPARATOR) failed")
            }
        }
        TrayMenuItem::Submenu {
            id: _,
            label,
            enabled,
            visible,
            role: _,
            toggle_type,
            children,
        } => {
            if !*visible {
                return Ok(());
            }

            let item_id = item.menu_event_id().map(str::to_owned);
            if children.is_empty() {
                let label_w = to_wide_null(label);
                let mut flags = MF_STRING;
                let checked = match toggle_type {
                    Some(TrayToggleType::Checkbox(checked)) => *checked,
                    Some(TrayToggleType::Radio(checked)) => *checked,
                    None => false,
                };
                flags |= if checked { MF_CHECKED } else { MF_UNCHECKED };
                if !*enabled {
                    flags |= MF_DISABLED;
                }

                let cmd = if let Some(item_id) = item_id {
                    let cmd = *next_id;
                    *next_id = next_id.wrapping_add(1).max(1000);

                    if let Ok(mut map) = id_to_menu_id.lock() {
                        map.insert(cmd, item_id);
                    }
                    cmd as usize
                } else {
                    0
                };

                let ok: BOOL = AppendMenuW(menu, flags, cmd, label_w.as_ptr());
                if ok == 0 {
                    anyhow::bail!("AppendMenuW(menu item) failed")
                }
            } else {
                let submenu = CreatePopupMenu();
                (submenu != ptr::null_mut())
                    .then_some(())
                    .context("CreatePopupMenu(submenu) failed")?;
                for child in children {
                    append_tray_menu_item(submenu, child, id_to_menu_id, next_id)?;
                }

                let label_w = to_wide_null(label);
                let mut flags = MF_POPUP;
                if !*enabled {
                    flags |= MF_DISABLED;
                }
                let ok: BOOL = AppendMenuW(menu, flags, submenu as usize, label_w.as_ptr());
                if ok == 0 {
                    anyhow::bail!("AppendMenuW(submenu) failed")
                }
            }
        }
    }

    Ok(())
}
