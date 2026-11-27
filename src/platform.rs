//! Frontera nativa de Xool Files. Todo lo que toca Win32 vive aquí.
//! La interfaz sigue siendo GPUI al cien por cien.

use crate::i18n::tr;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_void, OsStr};
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use gpui::{Bounds, Pixels, RenderImage, Window};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    GlobalFree, HANDLE, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WIN32_ERROR, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetMonitorInfoW, GetObjectW, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST, MonitorFromPoint, MonitorFromWindow,
    ReleaseDC, ScreenToClient,
};
use windows::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, IBindCtx, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, RegisterClipboardFormatW,
    SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT};
use windows::Win32::System::Ole::CF_HDROP;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetAsyncKeyState, HOT_KEY_MODIFIERS, VK_LBUTTON};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD,
};
use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
use windows::Win32::UI::Shell::{
    DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass, DragQueryFileW, DROPFILES, FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads,
    FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_Videos, IShellItemImageFactory,
    SHCreateItemFromParsingName, SHFileOperationW, SHGetFileInfoW, SHGetImageList,
    SHGetKnownFolderPath, SHFILEINFOW, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES, SHIL_EXTRALARGE,
    SHIL_JUMBO, SHIL_LARGE, SIIGBF, SIIGBF_BIGGERSIZEOK, SIIGBF_RESIZETOFIT, SHFILEOPSTRUCTW,
    SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW, ShellExecuteExW, ShellExecuteW, FO_COPY, FO_DELETE,
    FO_MOVE, SHEmptyRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND,
    SHQUERYRBINFO, SHQueryRecycleBinW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GA_ROOT, GetAncestor, GetCursorPos, GetIconInfo, GetWindowLongPtrW,
    HICON, ICONINFO, ICON_BIG, ICON_SMALL, LoadIconW, MB_ICONWARNING, MB_OK, MessageBoxW,
    GWL_STYLE, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    SW_RESTORE, SW_SHOWNORMAL, SendMessageW, SetWindowLongPtrW, SetWindowPos, ShowWindowAsync,
    DefWindowProcW, HTCAPTION, WindowFromPoint, WM_NCHITTEST, WM_NCLBUTTONDOWN,
    WM_NCLBUTTONDBLCLK, WM_NCDESTROY, WM_SETICON, WS_CAPTION,
    GetWindowPlacement, GetWindowRect, SetWindowPlacement, SW_SHOWMAXIMIZED, WINDOWPLACEMENT,
    WPF_RESTORETOMAXIMIZED,
};

use crate::settings::WindowPlacement;

#[derive(Clone)]
pub struct DriveInfo {
    pub path: PathBuf,
    pub letter: String,
    pub label: Option<String>,
    pub total: u64,
    pub free: u64,
    pub icon: Option<Arc<RenderImage>>,
}

impl DriveInfo {
    pub fn used_fraction(&self) -> f32 {
        if self.total == 0 { return 0.0; }
        self.total.saturating_sub(self.free) as f32 / self.total as f32
    }
}

const QUICK_SEARCH_HOTKEY_IDS: [i32; 2] = [0x5a46, 0x5a47];

struct QuickSearchHotkey {
    sender: futures::channel::mpsc::UnboundedSender<()>,
    registered: Option<(i32, (HOT_KEY_MODIFIERS, u32))>,
}

thread_local! {
    static QUICK_SEARCH_HOTKEYS: RefCell<HashMap<isize, QuickSearchHotkey>> = RefCell::new(HashMap::new());
    static TITLE_DRAG_AREAS: RefCell<HashMap<isize, [Option<Bounds<Pixels>>; 2]>> =
        RefCell::new(HashMap::new());
}

pub fn record_title_drag_area(window: &Window, slot: usize, bounds: Option<Bounds<Pixels>>) {
    let scale = window.scale_factor();
    let device_bounds = bounds.map(|bounds| Bounds::new(bounds.origin * scale,
        gpui::size(bounds.size.width * scale, bounds.size.height * scale)));
    TITLE_DRAG_AREAS.with(|areas| {
        areas.borrow_mut().entry(hwnd_value(window)).or_default()[slot] = device_bounds;
    });
}

unsafe extern "system" fn title_drag_proc(
    window: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, id: usize, _: usize,
) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::WM_COPYDATA;
    if msg == WM_COPYDATA {
        if let Some(folder) = unsafe { crate::instance::receive(lparam) } {
            return LRESULT(crate::tray::activate(window, folder) as isize);
        }
    } else if msg == WM_NCDESTROY {
        crate::instance::unpublish(window);
    }
    if crate::tray::handle_message(window, msg, wparam, lparam) { return LRESULT(0); }
    match msg {
        windows::Win32::UI::WindowsAndMessaging::WM_HOTKEY
            if QUICK_SEARCH_HOTKEY_IDS.contains(&(wparam.0 as i32)) =>
        {
            QUICK_SEARCH_HOTKEYS.with(|hotkeys| {
                if let Some(hotkey) = hotkeys.borrow().get(&(window.0 as isize))
                    && hotkey.registered.is_some_and(|(id, _)| id as usize == wparam.0)
                {
                    let _ = hotkey.sender.unbounded_send(());
                }
            });
            return LRESULT(0);
        }
        WM_NCHITTEST => {
            let mut cursor = POINT { x: lparam.0 as i16 as i32, y: (lparam.0 >> 16) as i16 as i32 };
            if unsafe { ScreenToClient(window, &mut cursor) }.as_bool() {
                let position = gpui::point(gpui::px(cursor.x as f32), gpui::px(cursor.y as f32));
                let in_caption = TITLE_DRAG_AREAS.with(|areas| {
                    areas.borrow().get(&(window.0 as isize)).is_some_and(|areas| {
                        areas.iter().flatten().any(|bounds| bounds.contains(&position))
                    })
                });
                if in_caption {
                    return LRESULT(HTCAPTION as isize);
                }
            }
        }
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK if wparam.0 == HTCAPTION as usize => {
            return unsafe { DefWindowProcW(window, msg, wparam, lparam) };
        }
        WM_NCDESTROY => {
            QUICK_SEARCH_HOTKEYS.with(|hotkeys| {
                if let Some(hotkey) = hotkeys.borrow_mut().remove(&(window.0 as isize))
                    && let Some((id, _)) = hotkey.registered
                {
                    let _ = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::UnregisterHotKey(Some(window), id) };
                }
            });
            TITLE_DRAG_AREAS.with(|areas| areas.borrow_mut().remove(&(window.0 as isize)));
            let _ = unsafe { RemoveWindowSubclass(window, Some(title_drag_proc), id) };
        }
        _ => {}
    }
    unsafe { DefSubclassProc(window, msg, wparam, lparam) }
}

pub(crate) fn quick_search_events(window: &Window) -> futures::channel::mpsc::UnboundedReceiver<()> {
    let (sender, receiver) = futures::channel::mpsc::unbounded();
    QUICK_SEARCH_HOTKEYS.with(|hotkeys| {
        hotkeys.borrow_mut().insert(hwnd_value(window), QuickSearchHotkey { sender, registered: None });
    });
    receiver
}

pub(crate) fn register_quick_search(window: &Window, keys: Option<&str>) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey};
    let handle = hwnd(window);
    let next = keys.map(quick_search_key).transpose()?;
    QUICK_SEARCH_HOTKEYS.with(|hotkeys| {
        let mut hotkeys = hotkeys.borrow_mut();
        let hotkey = hotkeys.get_mut(&(handle.0 as isize)).ok_or(tr("El atajo global no está inicializado.", "The global shortcut is not initialized."))?;
        let previous = hotkey.registered;
        if previous.map(|(_, keys)| keys) == next {
            return Ok(());
        }
        // Registra el nuevo primero para conservar el anterior si Windows lo rechaza.
        let new_registration = if let Some((modifiers, key)) = next {
            let [first, second] = QUICK_SEARCH_HOTKEY_IDS;
            let id = if previous.is_some_and(|(id, _)| id == first) { second } else { first };
            unsafe { RegisterHotKey(Some(handle), id, modifiers | MOD_NOREPEAT, key) }
                .map_err(|error| crate::localized!(format, "Windows no pudo registrar el atajo. {error}", "Windows could not register the shortcut. {error}"))?;
            Some((id, (modifiers, key)))
        } else {
            None
        };
        if let Some((previous_id, _)) = previous
            && let Err(error) = unsafe { UnregisterHotKey(Some(handle), previous_id) }
        {
            if let Some((new_id, _)) = new_registration {
                let _ = unsafe { UnregisterHotKey(Some(handle), new_id) };
            }
            return Err(crate::localized!(format, "No se pudo liberar el atajo anterior. {error}", "Could not release the previous shortcut. {error}"));
        }
        hotkey.registered = new_registration;
        Ok(())
    })
}

fn quick_search_key(keys: &str) -> Result<(HOT_KEY_MODIFIERS, u32), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MOD_ALT, MOD_CONTROL, MOD_SHIFT, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_F1, VK_HOME,
        VK_INSERT, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP,
        VkKeyScanW,
    };
    crate::shortcuts::validate(keys)?;
    let key = gpui::Keystroke::parse(keys).map_err(|_| tr("Combinación no válida.", "Invalid key combination.").to_owned())?;
    if key.modifiers.platform || key.modifiers.function {
        return Err(tr("Usa Ctrl, Alt o Mayús para el atajo global.", "Use Ctrl, Alt, or Shift for the global shortcut.").into());
    }
    let function = key.key.strip_prefix('f').and_then(|number| number.parse::<u16>().ok())
        .filter(|number| (1..=24).contains(number));
    if function == Some(12) {
        return Err(tr("Windows reserva F12. Elige otra tecla.", "Windows reserves F12. Choose another key.").into());
    }
    if function.is_none() && !(key.modifiers.control || key.modifiers.alt || key.modifiers.shift) {
        return Err(tr("Añade Ctrl, Alt o Mayús a la combinación.", "Add Ctrl, Alt, or Shift to the key combination.").into());
    }
    let mut modifiers = HOT_KEY_MODIFIERS(0);
    if key.modifiers.control { modifiers |= MOD_CONTROL; }
    if key.modifiers.alt { modifiers |= MOD_ALT; }
    if key.modifiers.shift { modifiers |= MOD_SHIFT; }
    if let Some(function) = function {
        return Ok((modifiers, u32::from(VK_F1.0 + function - 1)));
    }
    let virtual_key = match key.key.as_str() {
        "space" => VK_SPACE.0,
        "enter" => VK_RETURN.0,
        "tab" => VK_TAB.0,
        "backspace" => VK_BACK.0,
        "insert" => VK_INSERT.0,
        "delete" => VK_DELETE.0,
        "up" => VK_UP.0,
        "down" => VK_DOWN.0,
        "left" => VK_LEFT.0,
        "right" => VK_RIGHT.0,
        "home" => VK_HOME.0,
        "end" => VK_END.0,
        "pageup" => VK_PRIOR.0,
        "pagedown" => VK_NEXT.0,
        _ => {
            let mut units = key.key.encode_utf16();
            let unit = units.next().filter(|_| units.next().is_none())
                .ok_or(tr("Windows no admite esa tecla como atajo global.", "Windows does not support that key as a global shortcut."))?;
            let mapped = unsafe { VkKeyScanW(unit) };
            if mapped == -1 {
                return Err(tr("Esa tecla no está disponible en la distribución del teclado.", "This key is not available on the current keyboard layout.").into());
            }
            let keyboard_state = (mapped >> 8) as u8;
            if keyboard_state & 1 != 0 { modifiers |= MOD_SHIFT; }
            if keyboard_state & 2 != 0 { modifiers |= MOD_CONTROL; }
            if keyboard_state & 4 != 0 { modifiers |= MOD_ALT; }
            mapped as u16 & 0xff
        }
    };
    Ok((modifiers, u32::from(virtual_key)))
}

#[derive(Clone, Copy)]
pub(crate) struct SearchMonitor {
    work: RECT,
    scale: f32,
    pub display: Option<gpui::DisplayId>,
}

impl SearchMonitor {
    pub fn current(window: &Window, cx: &gpui::App) -> Self {
        use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

        let mut cursor = POINT::default();
        let monitor = unsafe {
            if GetCursorPos(&mut cursor).is_ok() {
                MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST)
            } else {
                MonitorFromWindow(hwnd(window), MONITOR_DEFAULTTONEAREST)
            }
        };
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            let display = window.display(cx).or_else(|| cx.primary_display());
            let bounds = display.as_ref().map(|display| display.bounds())
                .unwrap_or_else(|| gpui::Bounds::new(gpui::point(gpui::px(0.), gpui::px(0.)), gpui::size(gpui::px(1024.), gpui::px(768.))));
            return Self {
                work: RECT {
                    left: f32::from(bounds.origin.x) as i32,
                    top: f32::from(bounds.origin.y) as i32,
                    right: f32::from(bounds.origin.x + bounds.size.width) as i32,
                    bottom: f32::from(bounds.origin.y + bounds.size.height) as i32,
                },
                scale: 1.,
                display: display.map(|display| display.id()),
            };
        }

        let mut dpi_x = 96;
        let mut dpi_y = 96;
        let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
        let scale = dpi_x.max(96) as f32 / 96.;
        let rc = info.rcMonitor;
        let display = cx.displays().into_iter().find(|display| {
            let bounds = display.bounds();
            (f32::from(bounds.origin.x) - rc.left as f32 / scale).abs() < 1.
                && (f32::from(bounds.origin.y) - rc.top as f32 / scale).abs() < 1.
                && (f32::from(bounds.size.width) - (rc.right - rc.left) as f32 / scale).abs() < 1.
                && (f32::from(bounds.size.height) - (rc.bottom - rc.top) as f32 / scale).abs() < 1.
        }).map(|display| display.id());
        Self { work: info.rcWork, scale, display }
    }

    fn search_top(self) -> i32 {
        self.work.top + (self.work.bottom - self.work.top) / 5
    }

    pub fn size(self, width: f32, height: f32) -> gpui::Size<Pixels> {
        let space_below_anchor = (self.work.bottom - self.search_top()) as f32 / self.scale - 12.;
        gpui::size(
            gpui::px(width.min((self.work.right - self.work.left) as f32 / self.scale - 24.).max(1.)),
            gpui::px(height.min(space_below_anchor).max(1.)),
        )
    }

    pub fn bounds(self, size: gpui::Size<Pixels>, inset: f32) -> gpui::Bounds<Pixels> {
        let center = (self.work.left + (self.work.right - self.work.left) / 2) as f32 / self.scale;
        gpui::Bounds::new(gpui::point(gpui::px(center) - size.width / 2., gpui::px(self.search_top() as f32 / self.scale - inset)), size)
    }

    pub fn place(self, window: &Window, size: gpui::Size<Pixels>, radius: f32, inset: f32, cx: &gpui::App) {
        use windows::Win32::Graphics::Gdi::GetWindowRgnBox;
        use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, IsWindow};

        let owner = hwnd_value(window);
        let width = (f32::from(size.width) * self.scale).round() as i32;
        let height = (f32::from(size.height) * self.scale).round() as i32;
        let diameter = (radius * 2. * self.scale).round() as i32;
        let y = self.search_top() - (inset * self.scale).round() as i32;
        // SetWindowPos may dispatch Win32 messages. Let GPUI finish the event first.
        cx.foreground_executor().spawn(async move {
            let handle = hwnd_from_value(owner);
            unsafe {
                if IsWindow(Some(handle)).as_bool() {
                    let mut outer = RECT::default();
                    let mut client = RECT::default();
                    let (extra_w, extra_h) = if GetWindowRect(handle, &mut outer).is_ok()
                        && GetClientRect(handle, &mut client).is_ok() {
                        ((outer.right - outer.left - client.right).max(0),
                         (outer.bottom - outer.top - client.bottom).max(0))
                    } else {
                        (0, 0)
                    };
                    let width = width + extra_w;
                    let height = height + extra_h;
                    let x = self.work.left + (self.work.right - self.work.left - width) / 2;
                    let resized = outer.right - outer.left != width || outer.bottom - outer.top != height;
                    if outer.left != x || outer.top != y || resized {
                        let _ = SetWindowPos(handle, None, x, y, width, height, SWP_NOZORDER | SWP_NOACTIVATE);
                    }
                    let mut region = RECT::default();
                    if resized || GetWindowRgnBox(handle, &mut region).0 == 0 {
                        clip_search_frame(handle, diameter);
                    }
                }
            }
        }).detach();
    }
}

fn clip_search_frame(handle: HWND, diameter: i32) {
    use windows::Win32::Graphics::Gdi::{ClientToScreen, CreateRoundRectRgn, SetWindowRgn};
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

    let mut outer = RECT::default();
    let mut client = RECT::default();
    let mut origin = POINT::default();
    unsafe {
        if GetWindowRect(handle, &mut outer).is_err()
            || GetClientRect(handle, &mut client).is_err()
            || !ClientToScreen(handle, &mut origin).as_bool()
        {
            return;
        }
        // The region uses window coordinates, so it can exclude the non-client frame.
        let x = origin.x - outer.left;
        let y = origin.y - outer.top;
        let region = CreateRoundRectRgn(x, y, x + client.right, y + client.bottom, diameter, diameter);
        if !region.is_invalid() && SetWindowRgn(handle, Some(region), true) == 0 {
            let _ = DeleteObject(HGDIOBJ(region.0));
        }
    }
}

pub(crate) fn use_custom_search_border(window: &Window) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE,
        DWMWCP_DONOTROUND, DWMNCRP_DISABLED, DWMWA_NCRENDERING_POLICY,
    };
    let handle = hwnd(window);
    unsafe {
        let _ = DwmSetWindowAttribute(handle, DWMWA_BORDER_COLOR,
            (&DWMWA_COLOR_NONE as *const u32).cast(), size_of::<u32>() as u32);
        let _ = DwmSetWindowAttribute(handle, DWMWA_WINDOW_CORNER_PREFERENCE,
            &DWMWCP_DONOTROUND.0 as *const i32 as *const c_void, size_of::<i32>() as u32);
        let _ = DwmSetWindowAttribute(handle, DWMWA_NCRENDERING_POLICY,
            &DWMNCRP_DISABLED.0 as *const i32 as *const c_void, size_of::<i32>() as u32);
    }
}

pub(crate) fn search_has_foreground(window: &Window) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindow, GW_OWNER};

    let popup = hwnd(window);
    let mut foreground = unsafe { GetForegroundWindow() };
    while !foreground.is_invalid() {
        if foreground == popup { return true; }
        foreground = unsafe { GetWindow(foreground, GW_OWNER) }.unwrap_or_default();
    }
    false
}

/// Mantiene los controles personalizados y deja que Windows mueva la ventana por su barra.
pub fn enable_native_snap(window: &Window) {
    let handle = hwnd(window);
    if handle.is_invalid() { return; }
    unsafe {
        let _ = SetWindowSubclass(handle, Some(title_drag_proc), 1, 0);
        let style = GetWindowLongPtrW(handle, GWL_STYLE);
        let caption = WS_CAPTION.0 as isize;
        if style & caption == caption { return; }
        SetWindowLongPtrW(handle, GWL_STYLE, style | caption);
        let _ = SetWindowPos(handle, None, 0, 0, 0, 0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

pub fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

pub fn hwnd(window: &Window) -> HWND {
    match HasWindowHandle::window_handle(window).map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Win32(handle)) => HWND(handle.hwnd.get() as *mut c_void),
        _ => HWND::default(),
    }
}

/// Identificador de ventana listo para cruzar al ejecutor en segundo plano.
/// Los diálogos nativos del Shell necesitan una ventana propietaria para
/// aparecer centrados y bloquear correctamente a Xool Files.
pub fn hwnd_value(window: &Window) -> isize {
    hwnd(window).0 as isize
}

fn hwnd_from_value(value: isize) -> HWND {
    HWND(value as *mut c_void)
}

/// Deshabilita la ventana propietaria mientras Windows muestra un diálogo
/// nativo. Así Xool Files no sigue procesando teclas (por ejemplo, Intro
/// abriendo el archivo seleccionado) con la confirmación del sistema delante.
/// El `Drop` vuelve a habilitarla aunque la operación falle o se cancele.
struct ModalOwner(HWND);

impl ModalOwner {
    fn disable(owner: isize) -> Self {
        let window = hwnd_from_value(owner);
        if !window.is_invalid() {
            unsafe {
                let _ = EnableWindow(window, false);
            }
        }
        Self(window)
    }
}

impl Drop for ModalOwner {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = EnableWindow(self.0, true);
            }
        }
    }
}

pub fn restore_window(window: &Window) {
    unsafe {
        let _ = ShowWindowAsync(hwnd(window), SW_RESTORE);
    }
}

pub(crate) fn window_placement(window: &Window) -> Option<WindowPlacement> {
    let handle = hwnd(window);
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    unsafe { GetWindowPlacement(handle, &mut placement) }.ok()?;
    let mut rect = placement.rcNormalPosition;
    if placement.showCmd == SW_SHOWNORMAL.0 as u32 {
        // Una ventana acoplada puede conservar en rcNormalPosition sus
        // dimensiones previas al acople. Guarda el rectángulo visible.
        let mut visible = RECT::default();
        let mut monitor = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        unsafe {
            let display = MonitorFromWindow(handle, MONITOR_DEFAULTTONEAREST);
            if GetWindowRect(handle, &mut visible).is_ok()
                && GetMonitorInfoW(display, &mut monitor).as_bool()
            {
                let dx = monitor.rcWork.left - monitor.rcMonitor.left;
                let dy = monitor.rcWork.top - monitor.rcMonitor.top;
                rect = RECT {
                    left: visible.left - dx,
                    top: visible.top - dy,
                    right: visible.right - dx,
                    bottom: visible.bottom - dy,
                };
            }
        }
    }
    Some(WindowPlacement {
        normal_rect: [rect.left, rect.top, rect.right, rect.bottom],
        maximized: placement.showCmd == SW_SHOWMAXIMIZED.0 as u32
            || (placement.flags & WPF_RESTORETOMAXIMIZED) == WPF_RESTORETOMAXIMIZED,
    })
}

pub(crate) fn restore_window_placement(window: &Window, saved: WindowPlacement) {
    let [left, top, right, bottom] = saved.normal_rect;
    if !right.checked_sub(left).is_some_and(|width| width > 0)
        || !bottom.checked_sub(top).is_some_and(|height| height > 0)
    {
        return;
    }
    let placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        showCmd: if saved.maximized { SW_SHOWMAXIMIZED.0 } else { SW_SHOWNORMAL.0 } as u32,
        rcNormalPosition: RECT { left, top, right, bottom },
        ..Default::default()
    };
    // Windows conserva las coordenadas del área de trabajo y corrige una
    // colocación fuera de pantalla si cambió la distribución de monitores.
    let _ = unsafe { SetWindowPlacement(hwnd(window), &placement) };
}

/// Indica si el botón izquierdo del ratón sigue pulsado.
pub fn left_button_down() -> bool {
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}

/// Indica si el cursor está sobre la ventana indicada. Durante un arrastre
/// interno el cursor sigue dentro; al salir, el arrastre pasa a OLE.
pub fn cursor_over_hwnd(target: HWND) -> bool {
    let mut point = POINT::default();
    unsafe {
        if GetCursorPos(&mut point).is_err() {
            return false;
        }
        let under = WindowFromPoint(point);
        if under.0.is_null() {
            return false;
        }
        GetAncestor(under, GA_ROOT) == target
    }
}

/// Aplica a la ventana (y a la barra de tareas) el icono incrustado en el
/// ejecutable con el identificador de recurso 1.
pub fn set_window_icon(window: &Window) {
    unsafe {
        let Ok(module) = GetModuleHandleW(PCWSTR::null()) else {
            return;
        };
        let resource = PCWSTR::from_raw(1 as *const u16);
        let Ok(icon) = LoadIconW(Some(HINSTANCE(module.0)), resource) else {
            return;
        };
        let target = hwnd(window);
        let icon = LPARAM(icon.0 as isize);
        let _ = SendMessageW(
            target,
            WM_SETICON,
            Some(WPARAM(ICON_BIG as usize)),
            Some(icon),
        );
        let _ = SendMessageW(
            target,
            WM_SETICON,
            Some(WPARAM(ICON_SMALL as usize)),
            Some(icon),
        );
    }
}

pub fn message_box(window: &Window, text: &str, title: &str) {
    unsafe {
        MessageBoxW(
            Some(hwnd(window)),
            PCWSTR(wide(text).as_ptr()),
            PCWSTR(wide(title).as_ptr()),
            MB_OK | MB_ICONWARNING,
        );
    }
}

pub fn known_folders() -> Vec<(String, PathBuf)> {
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\"));

    let mut result = Vec::with_capacity(8);
    let entries: [(&str, &windows::core::GUID, &str); 7] = [
        (tr("Inicio", "Home"), &FOLDERID_Profile, ""),
        (tr("Escritorio", "Desktop"), &FOLDERID_Desktop, "Desktop"),
        (tr("Descargas", "Downloads"), &FOLDERID_Downloads, "Downloads"),
        (tr("Documentos", "Documents"), &FOLDERID_Documents, "Documents"),
        (tr("Imágenes", "Pictures"), &FOLDERID_Pictures, "Pictures"),
        (tr("Música", "Music"), &FOLDERID_Music, "Music"),
        (tr("Vídeos", "Videos"), &FOLDERID_Videos, "Videos"),
    ];

    for (label, id, fallback) in entries {
        let path = known_folder(id).unwrap_or_else(|| {
            if fallback.is_empty() {
                home.clone()
            } else {
                home.join(fallback)
            }
        });
        result.push((label.to_string(), path));
    }
    result
}

fn known_folder(id: &windows::core::GUID) -> Option<PathBuf> {
    unsafe {
        let ptr = SHGetKnownFolderPath(id, windows::Win32::UI::Shell::KNOWN_FOLDER_FLAG(0), None)
            .ok()?;
        let mut len = 0usize;
        while *ptr.0.add(len) != 0 {
            len += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(ptr.0, len));
        CoTaskMemFree(Some(ptr.0 as *const c_void));
        Some(PathBuf::from(text))
    }
}

pub(crate) fn fixed_drive_roots() -> Vec<PathBuf> {
    const DRIVE_FIXED: u32 = 3;
    let mask = unsafe { GetLogicalDrives() };
    (0..26u32).filter(|index| mask & (1 << index) != 0).filter_map(|index| {
        let letter = (b'A' + index as u8) as char;
        let path = PathBuf::from(format!("{letter}:\\"));
        (unsafe { GetDriveTypeW(PCWSTR(wide(&path).as_ptr())) } == DRIVE_FIXED).then_some(path)
    }).collect()
}

pub fn logical_drive_mask() -> u32 {
    unsafe { GetLogicalDrives() }
}

impl DriveInfo {
    pub fn is_in_mask(&self, mask: u32) -> bool {
        let index = self.letter.as_bytes()[0] - b'A';
        mask & (1 << index) != 0
    }
}

pub fn drives(mask: u32) -> Vec<DriveInfo> {
    let mut result = Vec::new();
    for index in 0..26u32 {
        if mask & (1 << index) == 0 {
            continue;
        }
        let letter = (b'A' + index as u8) as char;
        let path = PathBuf::from(format!("{letter}:\\"));
        let (total, free) = disk_space(&path).unwrap_or((0, 0));
        result.push(DriveInfo {
            icon: shell_icon(&path, true, 64),
            path: path.clone(),
            letter: format!("{letter}:"),
            label: volume_label(&path),
            total,
            free,
        });
    }
    result
}

fn volume_label(path: &Path) -> Option<String> {
    let root = wide(path);
    let mut name = [0u16; 261];
    unsafe {
        GetVolumeInformationW(PCWSTR(root.as_ptr()), Some(&mut name), None, None, None, None)
    }.ok()?;
    let end = name.iter().position(|&unit| unit == 0).unwrap_or(name.len());
    (end > 0).then(|| String::from_utf16_lossy(&name[..end]))
}

/// Espacio total y libre del volumen que contiene `path`.
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    let wide_path = wide(path);
    let (mut available, mut total) = (0u64, 0u64);
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(wide_path.as_ptr()),
            Some(&mut available),
            Some(&mut total),
            None,
        )
    };
    if ok.is_err() || total == 0 {
        None
    } else {
        Some((total, available.min(total)))
    }
}

pub fn system_is_light() -> bool {
    let mut value = 0u32;
    let mut length = size_of::<u32>() as u32;
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let name = wide("AppsUseLightTheme");
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut c_void),
            Some(&mut length),
        )
    };
    result == WIN32_ERROR(0) && value != 0
}

// Shell extensions may pump messages. Keep their modal loops away from GPUI.
fn needs_open_with(is_file: bool, code: windows::core::HRESULT, legacy_code: isize) -> bool {
    use windows::Win32::Foundation::ERROR_NO_ASSOCIATION;
    use windows::Win32::UI::Shell::SE_ERR_NOASSOC;
    is_file && (code == ERROR_NO_ASSOCIATION.to_hresult() || legacy_code == SE_ERR_NOASSOC as isize)
}

pub fn open(path: &Path) -> futures::channel::oneshot::Receiver<bool> {
    let path = path.to_path_buf();
    let (sender, receiver) = futures::channel::oneshot::channel();
    std::thread::spawn(move || unsafe {
        let is_file = path.is_file() && !crate::fs_model::shortcut_targets_directory(&path);
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let file = wide(&path);
        use windows::Win32::Foundation::ERROR_CANCELLED;
        use windows::Win32::UI::Shell::{OAIF_EXEC, OPENASINFO, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHOpenWithDialog};
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
            lpFile: PCWSTR(file.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        let result = match ShellExecuteExW(&mut info) {
            Err(error) if needs_open_with(is_file, error.code(), info.hInstApp.0 as isize) =>
            {
                let choice = OPENASINFO {
                    pcszFile: PCWSTR(file.as_ptr()),
                    pcszClass: PCWSTR::null(),
                    oaifInFlags: OAIF_EXEC,
                };
                SHOpenWithDialog(None, &choice)
            }
            result => result,
        };
        let _ = sender.send(is_file && result.is_ok());
        if let Err(error) = result && error.code() != ERROR_CANCELLED.to_hresult() {
            MessageBoxW(None, PCWSTR(wide(crate::localized!(format, "No se pudo abrir {}: {}", "Could not open {}: {}", path.display(), error)).as_ptr()),
                PCWSTR(wide("Xool Files").as_ptr()), MB_OK | MB_ICONWARNING);
        }
        if initialized { CoUninitialize(); }
    });
    receiver
}

#[cfg(test)]
#[test]
fn unassociated_files_use_the_windows_app_picker() {
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_CANCELLED, ERROR_NO_ASSOCIATION};
    assert!(needs_open_with(true, ERROR_NO_ASSOCIATION.to_hresult(), 0));
    assert!(needs_open_with(true, ERROR_CANCELLED.to_hresult(),
        windows::Win32::UI::Shell::SE_ERR_NOASSOC as isize));
    assert!(!needs_open_with(false, ERROR_NO_ASSOCIATION.to_hresult(), 0));
    assert!(!needs_open_with(true, ERROR_CANCELLED.to_hresult(), 0));
    assert!(!needs_open_with(true, ERROR_ACCESS_DENIED.to_hresult(), 0));
}

/// Abre la Papelera de reciclaje del sistema en una ventana del Explorador.
/// Abre la Papelera del sistema en una ventana del Explorador. Xool Files usa
/// su propia vista virtual; esta función queda como escape para casos en los
/// que el Shell necesite mostrar la Papelera original.
pub fn open_recycle_bin() {
    std::thread::spawn(move || unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let file = wide("shell:RecycleBinFolder");
        let verb = wide("open");
        let mut info = SHELLEXECUTEINFOW::default();
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = windows::Win32::UI::Shell::SEE_MASK_NOASYNC;
        info.lpVerb = PCWSTR(verb.as_ptr());
        info.lpFile = PCWSTR(file.as_ptr());
        info.nShow = SW_SHOWNORMAL.0;
        if let Err(error) = ShellExecuteExW(&mut info) {
            MessageBoxW(
                None,
                PCWSTR(wide(crate::localized!(format, "No se pudo abrir la Papelera: {error}", "Could not open the Recycle Bin: {error}")).as_ptr()),
                PCWSTR(wide("Xool Files").as_ptr()),
                MB_OK | MB_ICONWARNING,
            );
        }
        if initialized {
            CoUninitialize();
        }
    });
}

/// Número de elementos de la Papelera en todas las unidades.
fn recycle_bin_item_count() -> Option<i64> {
    let mut info = SHQUERYRBINFO {
        cbSize: size_of::<SHQUERYRBINFO>() as u32,
        ..Default::default()
    };
    unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info).ok()? };
    Some(info.i64NumItems)
}

/// Vacía la Papelera del sistema. Con `confirm` se muestra la ventana
/// nativa de Windows (confirmación y progreso); sin él, la operación es
/// silenciosa. Devuelve `Ok(false)` si el usuario canceló la ventana.
pub fn empty_recycle_bin(owner: isize, confirm: bool) -> Result<bool, String> {
    let flags = if confirm {
        0
    } else {
        SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND
    };
    let before = confirm.then(recycle_bin_item_count).flatten();
    let _modal = confirm.then(|| ModalOwner::disable(owner));
    unsafe {
        SHEmptyRecycleBinW(Some(hwnd_from_value(owner)), None, flags)
            .map_err(|error| crate::localized!(format, "No se pudo vaciar la Papelera: {error}", "Could not empty the Recycle Bin: {error}"))?;
    }
    // Al cancelar, `SHEmptyRecycleBinW` devuelve S_FALSE y la conversión de
    // windows-rs lo trata como éxito. Comparar el número de elementos evita
    // anunciar un vaciado que el usuario no confirmó.
    if let Some(before) = before {
        if let Some(after) = recycle_bin_item_count() {
            return Ok(after < before);
        }
    }
    Ok(true)
}

/// Standard Shell data supports multi-selection and Unicode paths. Windows
/// negotiates copy/move with the destination; we never delete sources ourselves.
/// El objeto se entrega envuelto (`drag_object::synchronous`) para que OLE no
/// lo ponga en modo asíncrono y los destinos reciban la ruta al primer intento.
pub fn start_file_drag(
    paths: Vec<PathBuf>,
    ready: std::sync::mpsc::Sender<()>,
    proceed: std::sync::mpsc::Receiver<()>,
) {
    let source_thread = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    std::thread::spawn(move || unsafe {
        drag_diagnostic("drag thread started");
        use windows::Win32::System::Com::IDataObject;
        use windows::Win32::System::Ole::{OleInitialize, OleUninitialize, IDropSource, DROPEFFECT_COPY, DROPEFFECT_MOVE};
        use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
        use windows::Win32::UI::Shell::{
            BHID_DataObject, SHCreateShellItemArrayFromIDLists, SHDoDragDrop, SHParseDisplayName,
        };
        if paths.is_empty() {
            return;
        }
        if OleInitialize(None).is_err() {
            drag_diagnostic("Error: OleInitialize");
            return;
        }
        let mut ids = Vec::new();
        let result = (|| -> windows::core::Result<()> {
            for path in &paths {
                let mut id = std::ptr::null_mut();
                SHParseDisplayName(PCWSTR(wide(path).as_ptr()), None::<&IBindCtx>, &mut id, 0, None)?;
                ids.push(id);
            }
            let refs: Vec<_> = ids.iter().map(|id| *id as *const _).collect();
            // Los PIDL son absolutos; el arreglo de elementos del shell sabe
            // convertirlos en un IDataObject con CF_HDROP para cualquier
            // destino (Escritorio, Explorador u otra aplicación).
            let items = SHCreateShellItemArrayFromIDLists(&refs)?;
            let data: IDataObject =
                items.BindToHandler(None::<&IBindCtx>, &BHID_DataObject)?;
            // El objeto del Shell acepta el modo asíncrono de OLE; envuelto en
            // un IDataObject simple entrega siempre los datos completos.
            let data = crate::drag_object::synchronous(data);
            // La interfaz mantiene su propio arrastre mientras el cursor está
            // dentro de la ventana, para que soltar sobre una carpeta de Xool Files
            // siga funcionando. Cuando el cursor sale, cede el ratón a OLE.
            if ready.send(()).is_err() {
                return Ok(());
            }
            if proceed.recv().is_err() {
                drag_diagnostic("Cancelado: arrastre interno");
                return Ok(());
            }
            if !left_button_down() {
                drag_diagnostic("Cancelado: botón liberado antes de OLE");
                return Ok(());
            }
            // OLE necesita compartir la cola de entrada con el hilo de la
            // interfaz para recibir el movimiento y la suelta del ratón;
            // sin esto el bucle de arrastre se queda esperando.
            let thread = GetCurrentThreadId();
            let attached = AttachThreadInput(thread, source_thread, true).as_bool();
            drag_diagnostic(if attached { "calling SHDoDragDrop (attached)" } else { "calling SHDoDragDrop (not attached)" });
            let effect = SHDoDragDrop(None, &data, None::<&IDropSource>, DROPEFFECT_COPY | DROPEFFECT_MOVE);
            if attached { let _ = AttachThreadInput(thread, source_thread, false); }
            drag_diagnostic(&format!("Completed: {}", effect?.0));
            Ok(())
        })();
        for id in ids { CoTaskMemFree(Some(id.cast())); }
        if let Err(error) = result {
            drag_diagnostic(&format!("Error: {error}"));
            MessageBoxW(None, PCWSTR(wide(crate::localized!(format, "No se pudo arrastrar: {error}", "Could not drag: {error}")).as_ptr()),
                PCWSTR(wide("Xool Files").as_ptr()), MB_OK | MB_ICONWARNING);
        }
        OleUninitialize();
    });
}

pub(crate) fn drag_diagnostic(message: &str) {
    if crate::settings::data_dir_override().is_some() {
        if let Some(path) = crate::settings::settings_path() {
            let _ = std::fs::write(path.with_file_name("drag-test.log"), message);
        }
    }
}

/// Añade una línea al registro de formatos del arrastre, sin borrar las
/// anteriores: el destino puede consultar varios formatos seguidos.
pub(crate) fn drag_diagnostic_append(message: &str) {
    if crate::settings::data_dir_override().is_some() {
        if let Some(path) = crate::settings::settings_path() {
            let path = path.with_file_name("drag-formats.log");
            let mut current = std::fs::read_to_string(&path).unwrap_or_default();
            current.push_str(message);
            current.push('\n');
            let _ = std::fs::write(path, current);
        }
    }
}
pub fn open_with(window: &Window, path: &Path) {
    let file = wide(path);
    let verb = wide("openas");
    let result = unsafe {
        ShellExecuteW(
            Some(hwnd(window)),
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        let args = format!("shell32.dll,OpenAs_RunDLL \"{}\"", path.display());
        let file = wide("rundll32.exe");
        let args = wide(args);
        unsafe {
            ShellExecuteW(
                Some(hwnd(window)),
                PCWSTR::null(),
                PCWSTR(file.as_ptr()),
                PCWSTR(args.as_ptr()),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
    }
}

pub fn reveal(window: &Window, path: &Path) {
    let explorer = wide("explorer.exe");
    let args = wide(format!("/select,\"{}\"", path.display()));
    unsafe {
        ShellExecuteW(
            Some(hwnd(window)),
            PCWSTR::null(),
            PCWSTR(explorer.as_ptr()),
            PCWSTR(args.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

pub fn properties(window: &Window, path: &Path) {
    let owner = hwnd_value(window);
    let path = path.to_path_buf();
    std::thread::spawn(move || unsafe {
        // The shell runs a native message loop while showing the property sheet.
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let verb = wide("properties");
        let file = wide(&path);
        let mut info = SHELLEXECUTEINFOW::default();
        info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
        info.fMask = SEE_MASK_INVOKEIDLIST | windows::Win32::UI::Shell::SEE_MASK_NOASYNC;
        info.hwnd = hwnd_from_value(owner);
        info.lpVerb = PCWSTR(verb.as_ptr());
        info.lpFile = PCWSTR(file.as_ptr());
        info.nShow = SW_SHOWNORMAL.0;
        if let Err(error) = ShellExecuteExW(&mut info) {
            let message = wide(crate::localized!(format, "No se pudieron abrir las propiedades: {error}", "Could not open properties: {error}"));
            MessageBoxW(None, PCWSTR(message.as_ptr()), PCWSTR(wide("Xool Files").as_ptr()),
                MB_OK | MB_ICONWARNING);
        }
        if initialized { CoUninitialize(); }
    });
}

pub fn open_terminal(window: &Window, path: &Path) {
    let wt = wide("wt.exe");
    let args = wide(format!("-d \"{}\"", path.display()));
    let result = unsafe {
        ShellExecuteW(
            Some(hwnd(window)),
            PCWSTR::null(),
            PCWSTR(wt.as_ptr()),
            PCWSTR(args.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize > 32 {
        return;
    }

    let ps = wide("powershell.exe");
    let args = wide(format!(
        "-NoExit -Command \"Set-Location -LiteralPath '{}'\"",
        path.display().to_string().replace('\'', "''")
    ));
    unsafe {
        ShellExecuteW(
            Some(hwnd(window)),
            PCWSTR::null(),
            PCWSTR(ps.as_ptr()),
            PCWSTR(args.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

pub fn cancel_clipboard_cut(window: &Window, sequence: u32) -> Result<(), String> {
    unsafe {
        OpenClipboard(Some(hwnd(window))).map_err(|err| err.to_string())?;
        let result = if clipboard_sequence() == sequence {
            EmptyClipboard().map_err(|err| err.to_string())
        } else {
            Ok(())
        };
        let _ = CloseClipboard();
        result
    }
}

pub fn clipboard_sequence() -> u32 {
    unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

pub fn clipboard_write_files(window: &Window, paths: &[PathBuf], cut: bool) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }

    let mut payload: Vec<u16> = paths.iter().flat_map(|path| wide(path)).collect();
    payload.push(0);

    unsafe {
        OpenClipboard(Some(hwnd(window))).map_err(|err| err.to_string())?;
        let result = (|| -> Result<(), String> {
            EmptyClipboard().map_err(|err| err.to_string())?;

            let size = size_of::<DROPFILES>() + payload.len() * 2;
            let memory = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, size)
                .map_err(|err| err.to_string())?;
            let pointer = GlobalLock(memory) as *mut u8;
            if pointer.is_null() {
                return Err(tr("No se pudo preparar el portapapeles.", "Could not prepare the clipboard.").to_string());
            }
            let drop = pointer as *mut DROPFILES;
            (*drop).pFiles = size_of::<DROPFILES>() as u32;
            (*drop).pt = POINT { x: 0, y: 0 };
            (*drop).fNC = false.into();
            (*drop).fWide = true.into();
            std::ptr::copy_nonoverlapping(
                payload.as_ptr() as *const u8,
                pointer.add(size_of::<DROPFILES>()),
                payload.len() * 2,
            );
            let _ = GlobalUnlock(memory);

            if SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(memory.0))).is_err() {
                return Err(tr("Windows rechazó los archivos en el portapapeles.", "Windows rejected the files on the clipboard.").to_string());
            }

            let effect = GlobalAlloc(GMEM_MOVEABLE, 4).map_err(|err| err.to_string())?;
            let effect_ptr = GlobalLock(effect) as *mut u32;
            if !effect_ptr.is_null() {
                *effect_ptr = if cut { 2 } else { 5 };
                let _ = GlobalUnlock(effect);
                let format_name = wide("Preferred DropEffect");
                let format = RegisterClipboardFormatW(PCWSTR(format_name.as_ptr()));
                if SetClipboardData(format, Some(HANDLE(effect.0))).is_err() {
                    let _ = GlobalFree(Some(HGLOBAL(effect.0)));
                }
            } else {
                let _ = GlobalFree(Some(HGLOBAL(effect.0)));
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

pub fn clipboard_read_files(window: &Window) -> Result<(Vec<PathBuf>, bool), String> {
    unsafe {
        OpenClipboard(Some(hwnd(window))).map_err(|err| err.to_string())?;
        let result = (|| -> Result<(Vec<PathBuf>, bool), String> {
            let mut paths = Vec::new();
            if let Ok(handle) = GetClipboardData(CF_HDROP.0 as u32) {
                let drop = windows::Win32::UI::Shell::HDROP(handle.0);
                let count = DragQueryFileW(drop, u32::MAX, None);
                for index in 0..count {
                    let length = DragQueryFileW(drop, index, None);
                    let mut buffer = vec![0u16; length as usize + 1];
                    let written = DragQueryFileW(drop, index, Some(&mut buffer));
                    if written > 0 {
                        paths.push(PathBuf::from(String::from_utf16_lossy(
                            &buffer[..written as usize],
                        )));
                    }
                }
            }

            let mut cut = false;
            let format_name = wide("Preferred DropEffect");
            let format = RegisterClipboardFormatW(PCWSTR(format_name.as_ptr()));
            if format != 0
                && let Ok(handle) = GetClipboardData(format)
                && GlobalSize(HGLOBAL(handle.0)) >= 4
            {
                let pointer = GlobalLock(HGLOBAL(handle.0)) as *const u32;
                if !pointer.is_null() {
                    cut = *pointer == 2;
                    let _ = GlobalUnlock(HGLOBAL(handle.0));
                }
            }
            Ok((paths, cut))
        })();
        let _ = CloseClipboard();
        result
    }
}

pub fn clipboard_write_text(window: &Window, text: &str) -> Result<(), String> {
    unsafe {
        OpenClipboard(Some(hwnd(window))).map_err(|err| err.to_string())?;
        let result = (|| -> Result<(), String> {
            EmptyClipboard().map_err(|err| err.to_string())?;
            let data = wide(text);
            let memory = GlobalAlloc(GMEM_MOVEABLE, data.len() * 2)
                .map_err(|err| err.to_string())?;
            let pointer = GlobalLock(memory) as *mut u16;
            if pointer.is_null() {
                return Err(tr("No se pudo escribir en el portapapeles.", "Could not write to the clipboard.").to_string());
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), pointer, data.len());
            let _ = GlobalUnlock(memory);
            SetClipboardData(13, Some(HANDLE(memory.0)))
                .map_err(|err| err.to_string())?;
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

fn double_terminated(paths: &[PathBuf]) -> Vec<u16> {
    let mut data: Vec<u16> = paths.iter().flat_map(|path| wide(path)).collect();
    data.push(0);
    data
}

fn run_file_operation(operation: u32, from: &[PathBuf], to: Option<&Path>) -> Result<(), String> {
    if from.is_empty() {
        return Ok(());
    }
    let from_data = double_terminated(from);
    let to_data = to.map(|path| double_terminated(&[path.to_path_buf()]));
    let mut op = SHFILEOPSTRUCTW::default();
    op.wFunc = operation;
    op.pFrom = PCWSTR(from_data.as_ptr());
    if let Some(to_data) = to_data.as_ref() {
        op.pTo = PCWSTR(to_data.as_ptr());
    }
    const FOF_SILENT: u16 = 0x0004;
    const FOF_RENAMEONCOLLISION: u16 = 0x0008;
    const FOF_NOCONFIRMATION: u16 = 0x0010;
    const FOF_NOERRORUI: u16 = 0x0400;
    op.fFlags = FOF_SILENT | FOF_RENAMEONCOLLISION | FOF_NOCONFIRMATION | FOF_NOERRORUI;
    let code = unsafe { SHFileOperationW(&mut op) };
    if op.fAnyOperationsAborted.as_bool() {
        return Err(tr("La operación se canceló.", "The operation was canceled.").to_string());
    }
    if code != 0 {
        return Err(crate::localized!(format, "Windows no pudo completar la operación (código {code}).", "Windows could not complete the operation (code {code})."));
    }
    Ok(())
}

/// Mueve archivos a la Papelera con el diálogo nativo de Windows.
/// Devuelve `Ok(false)` si el usuario canceló la operación.
pub fn delete_to_recycle(paths: &[PathBuf], owner: isize, confirm: bool) -> Result<bool, String> {
    if paths.is_empty() {
        return Ok(true);
    }
    const FOF_ALLOWUNDO: u16 = 0x0040;
    const FOF_NOCONFIRMATION: u16 = 0x0010;
    const FOF_WANTNUKEWARNING: u16 = 0x4000;
    let from_data = double_terminated(paths);
    let mut op = SHFILEOPSTRUCTW::default();
    op.hwnd = hwnd_from_value(owner);
    op.wFunc = FO_DELETE;
    op.pFrom = PCWSTR(from_data.as_ptr());
    op.fFlags = FOF_ALLOWUNDO | FOF_WANTNUKEWARNING;
    if !confirm {
        op.fFlags |= FOF_NOCONFIRMATION | 0x0004; // FOF_SILENT
    }
    let _modal = confirm.then(|| ModalOwner::disable(owner));
    let code = unsafe { SHFileOperationW(&mut op) };
    if op.fAnyOperationsAborted.as_bool() {
        return Ok(false);
    }
    if code != 0 {
        return Err(crate::localized!(format, "No se pudo mover a la papelera (código {code}).", "Could not move to the Recycle Bin (code {code})."));
    }
    Ok(true)
}

fn transfer_items(paths: &[PathBuf], destination: &Path, moving: bool) -> Result<usize, String> {
    if paths.is_empty() { return Ok(0); }
    let resolved_destination = destination.canonicalize()
        .ok().filter(|path| path.is_dir())
        .ok_or_else(|| tr("La carpeta de destino no está disponible.", "The destination folder is unavailable.").to_string())?;
    let mut pending = Vec::with_capacity(paths.len());
    let mut seen = std::collections::HashSet::new();
    for path in paths {
        if !seen.insert(path.as_path()) { continue; }
        let resolved = path.canonicalize()
            .map_err(|_| crate::localized!(format, "El elemento de origen ya no está disponible: {}", "The source item is no longer available: {}", path.display()))?;
        if resolved.is_dir() && resolved_destination.starts_with(&resolved) {
            return Err(tr("No se puede colocar una carpeta dentro de sí misma.", "A folder cannot be placed inside itself.").to_string());
        }
        let same_folder = resolved.parent().is_some_and(|parent|
            parent.as_os_str().eq_ignore_ascii_case(resolved_destination.as_os_str()));
        if !(same_folder && moving) { pending.push(path.clone()); }
    }
    run_file_operation(if moving { FO_MOVE } else { FO_COPY }, &pending, Some(destination))?;
    Ok(pending.len())
}

pub fn copy_items(paths: &[PathBuf], destination: &Path) -> Result<usize, String> {
    transfer_items(paths, destination, false)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    volume: u32,
    index: u64,
}

pub(crate) fn file_identity(path: &Path) -> std::io::Result<FileIdentity> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        GetFileInformationByHandle,
    };

    let file = std::fs::OpenOptions::new().access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
        .map_err(std::io::Error::other)?;
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    if index == 0 {
        return Err(std::io::Error::other(tr("Identidad de archivo no disponible.", "File identity is unavailable.")));
    }
    Ok(FileIdentity { volume: info.dwVolumeSerialNumber, index })
}

pub(crate) fn undo_move(from: &Path, to: &Path) -> std::io::Result<()> {
    use windows::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let source = wide(from);
    let target = wide(to);
    unsafe {
        MoveFileExW(PCWSTR(source.as_ptr()), PCWSTR(target.as_ptr()), MOVEFILE_WRITE_THROUGH)
    }.map_err(std::io::Error::other)
}

pub fn move_items(paths: &[PathBuf], destination: &Path) -> Result<usize, String> {
    transfer_items(paths, destination, true)
}

/// Icono nativo del shell para un archivo o carpeta, con caché.
/// Devuelve píxeles BGRA listos para GPUI.
pub fn recycle_bin_icon() -> Option<Arc<RenderImage>> {
    use windows::Win32::UI::Shell::{
        SHGetStockIconInfo, SHGSI_ICON, SHSTOCKICONINFO, SIID_RECYCLER, SIID_RECYCLERFULL,
    };
    static EMPTY: OnceLock<Option<Arc<RenderImage>>> = OnceLock::new();
    static FULL: OnceLock<Option<Arc<RenderImage>>> = OnceLock::new();
    let (cache, stock) = if recycle_bin_item_count().unwrap_or(0) > 0 {
        (&FULL, SIID_RECYCLERFULL)
    } else {
        (&EMPTY, SIID_RECYCLER)
    };
    cache.get_or_init(|| {
        let mut info = SHSTOCKICONINFO {
            cbSize: size_of::<SHSTOCKICONINFO>() as u32,
            ..Default::default()
        };
        unsafe {
            SHGetStockIconInfo(stock, SHGSI_ICON, &mut info).ok()?;
            let image = hicon_to_image(info.hIcon);
            let _ = DestroyIcon(info.hIcon);
            image
        }
    }).clone()
}

struct MenuApplication {
    names: &'static [&'static str],
    executable: &'static str,
    program_path: Option<&'static str>,
    local_path: Option<&'static str>,
    icon: OnceLock<Arc<RenderImage>>,
}

impl MenuApplication {
    const fn new(names: &'static [&'static str], executable: &'static str,
        program_path: Option<&'static str>, local_path: Option<&'static str>,
    ) -> Self {
        Self { names, executable, program_path, local_path, icon: OnceLock::new() }
    }

    fn icon(&self) -> Option<Arc<RenderImage>> {
        use windows::Win32::UI::Shell::{ExtractIconExW, FOLDERID_LocalAppData};
        if let Some(icon) = self.icon.get() { return Some(icon.clone()); }
        let registered = registered_application_path(self.executable);
        let path = registered.filter(|path| path.is_file())
            .or_else(|| self.local_path.and_then(|path| {
                known_folder(&FOLDERID_LocalAppData)
                    .map(|root| root.join(path)).filter(|path| path.is_file())
            }))
            .or_else(|| self.program_path.and_then(|relative| {
                ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"].into_iter()
                    .filter_map(std::env::var_os)
                    .map(|root| PathBuf::from(root).join(relative))
                    .find(|path| path.is_file())
            }))
            .or_else(|| std::env::var_os("SystemRoot").map(PathBuf::from)
                .map(|root| root.join("System32").join(self.executable))
                .filter(|path| path.is_file()))?;

        let image = unsafe {
            let path = wide(path);
            let mut icon = HICON::default();
            if ExtractIconExW(PCWSTR(path.as_ptr()), 0, None, Some(&mut icon), 1) != 1 {
                return None;
            }
            let image = hicon_to_image(icon);
            let _ = DestroyIcon(icon);
            image
        }?;

        Some(self.icon.get_or_init(|| image).clone())
    }
}

static MENU_APPLICATIONS: [MenuApplication; 6] = [
    MenuApplication::new(&["winrar"], "WinRAR.exe", Some(r"WinRAR\WinRAR.exe"), None),
    MenuApplication::new(&["7-zip"], "7zFM.exe", Some(r"7-Zip\7zFM.exe"), None),
    MenuApplication::new(&["itop easy desktop"], "iEasyDesk.exe", Some(r"iTop Easy Desktop\iEasyDesk.exe"), None),
    MenuApplication::new(&["bloc de notas", "notepad"], "notepad.exe", None, None),
    MenuApplication::new(&["microsoft paint", "con paint", "with paint"], "mspaint.exe", None, None),
    MenuApplication::new(&["visual studio code", "vs code"], "Code.exe",
        Some(r"Microsoft VS Code\Code.exe"), Some(r"Programs\Microsoft VS Code\Code.exe")),
];

struct WindowsMenuIcon {
    labels: &'static [&'static str],
    application: Option<&'static str>,
    stock: Option<windows::Win32::UI::Shell::SHSTOCKICONID>,
    image: OnceLock<Option<Arc<RenderImage>>>,
}

impl WindowsMenuIcon {
    const fn new(
        labels: &'static [&'static str], application: Option<&'static str>,
        stock: Option<windows::Win32::UI::Shell::SHSTOCKICONID>,
    ) -> Self {
        Self { labels, application, stock, image: OnceLock::new() }
    }

    fn load(&self) -> Option<Arc<RenderImage>> {
        self.image.get_or_init(|| {
            self.application.and_then(packaged_application_icon)
                .or_else(|| self.stock.and_then(stock_icon))
        }).clone()
    }
}

use windows::Win32::UI::Shell::{SIID_APPLICATION, SIID_SHIELD};

static WINDOWS_MENU_ICONS: [WindowsMenuIcon; 7] = [
    WindowsMenuIcon::new(&["fotos", "photos", "microsoft photos", "editar con photos",
        "editar con fotos", "edit with photos", "edit in photos", "editar en fotos"],
        Some("Microsoft.Windows.Photos_8wekyb3d8bbwe!App"), None),
    WindowsMenuIcon::new(&["herramienta de recortes", "herramienta recortes", "recortes", "snipping tool"],
        Some("Microsoft.ScreenSketch_8wekyb3d8bbwe!App"), None),
    WindowsMenuIcon::new(&["paint", "microsoft paint", "editar con paint", "edit with paint"],
        Some("Microsoft.Paint_8wekyb3d8bbwe!App"), None),
    WindowsMenuIcon::new(&["bloc de notas", "notepad", "editar en el bloc de notas",
        "editar con el bloc de notas", "editar en bloc de notas", "edit with notepad", "edit in notepad"],
        Some("Microsoft.WindowsNotepad_8wekyb3d8bbwe!App"), None),
    WindowsMenuIcon::new(&["microsoft store", "buscar en microsoft store", "buscar en la microsoft store",
        "search the microsoft store", "look for an app in the microsoft store"],
        Some("Microsoft.WindowsStore_8wekyb3d8bbwe!App"), None),
    WindowsMenuIcon::new(&["analizar con microsoft defender", "analizar con windows defender",
        "scan with microsoft defender", "scan with windows defender"],
        Some("Microsoft.SecHealthUI_8wekyb3d8bbwe!SecHealthUI"), Some(SIID_SHIELD)),
    WindowsMenuIcon::new(&["elegir otra aplicación", "elegir otra app", "choose another app",
        "choose another application"], None, Some(SIID_APPLICATION)),
];

pub(crate) fn windows_menu_icon(label: &str) -> Option<Arc<RenderImage>> {
    let label = label.trim_end_matches(['.', '…']).trim_end().to_lowercase();
    WINDOWS_MENU_ICONS.iter().find(|item| item.labels.contains(&label.as_str()))
        .and_then(WindowsMenuIcon::load)
}

fn stock_icon(stock: windows::Win32::UI::Shell::SHSTOCKICONID) -> Option<Arc<RenderImage>> {
    use windows::Win32::UI::Shell::{SHGSI_ICON, SHGSI_SMALLICON, SHGetStockIconInfo, SHSTOCKICONINFO};
    let mut info = SHSTOCKICONINFO { cbSize: size_of::<SHSTOCKICONINFO>() as u32, ..Default::default() };
    unsafe {
        SHGetStockIconInfo(stock, SHGSI_ICON | SHGSI_SMALLICON, &mut info).ok()?;
        let image = hicon_to_image(info.hIcon);
        let _ = DestroyIcon(info.hIcon);
        image
    }
}

fn packaged_application_icon(app_id: &str) -> Option<Arc<RenderImage>> {
    use windows::Win32::UI::Shell::SIIGBF_ICONONLY;
    let path = format!(r"shell:AppsFolder\{app_id}");
    shell_item_image(Path::new(&path), 24, SIIGBF_ICONONLY)
}

static TERMINAL_MENU_ICON: OnceLock<Option<Arc<RenderImage>>> = OnceLock::new();

fn load_terminal_menu_icon() -> Option<Arc<RenderImage>> {
    packaged_application_icon("Microsoft.WindowsTerminal_8wekyb3d8bbwe!App").or_else(|| {
        let system = std::env::var_os("SystemRoot")?;
        load_shell_icon(&PathBuf::from(system).join(r"System32\cmd.exe"), false, 24)
    })
}

pub(crate) fn terminal_menu_icon() -> Option<Arc<RenderImage>> {
    TERMINAL_MENU_ICON.get().cloned().flatten()
}

pub(crate) fn preload_menu_application_icons() {
    TERMINAL_MENU_ICON.get_or_init(load_terminal_menu_icon);
    for icon in &WINDOWS_MENU_ICONS { icon.load(); }
    for application in &MENU_APPLICATIONS {
        application.icon();
    }
}

pub(crate) fn menu_application_icon(label: &str) -> Option<Arc<RenderImage>> {
    let label = label.to_ascii_lowercase();
    MENU_APPLICATIONS.iter()
        .find(|application| application.names.iter().any(|name| label.contains(name)))
        .and_then(MenuApplication::icon)
}

fn registered_application_path(executable: &str) -> Option<PathBuf> {
    use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    let subkey = format!("Software\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{executable}");
    [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE].into_iter()
        .find_map(|hive| registered_icon_path(hive, &subkey, None))
}

fn registered_icon_path(
    hive: windows::Win32::System::Registry::HKEY, subkey: &str, value_name: Option<&str>,
) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::Registry::RRF_RT_REG_SZ;
    let subkey = wide(subkey);
    let value_name = value_name.map(wide);
    let value_name = value_name.as_ref().map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr()));
    let mut value = [0u16; 1024];
    let mut bytes = std::mem::size_of_val(&value) as u32;
    let result = unsafe {
        RegGetValueW(
            hive, PCWSTR(subkey.as_ptr()), value_name, RRF_RT_REG_SZ, None,
            Some(value.as_mut_ptr().cast()), Some(&mut bytes),
        )
    };
    if result.is_err() { return None; }
    let len = value.iter().position(|&letter| letter == 0).unwrap_or(value.len());
    Some(PathBuf::from(std::ffi::OsString::from_wide(&value[..len])))
}

pub fn shell_icon(path: &Path, is_dir: bool, size: u32) -> Option<Arc<RenderImage>> {
    let key = (icon_cache_key(path, is_dir), icon_size_bucket(size));
    if let Some(cached) = icon_cache()
        .lock()
        .ok()
        .and_then(|cache| cache.get(&key).cloned())
    {
        return cached;
    }

    let icon = load_shell_icon(path, is_dir, size);
    if let Ok(mut cache) = icon_cache().lock() {
        if cache.len() >= 160 {
            cache.clear();
        }
        cache.insert(key, icon.clone());
    }
    icon
}

type IconCache = HashMap<((String, bool), u32), Option<Arc<RenderImage>>>;

pub fn default_folder_icon() -> Option<Arc<RenderImage>> {
    static ICON: OnceLock<Option<Arc<RenderImage>>> = OnceLock::new();
    ICON.get_or_init(|| load_shell_icon(Path::new("Folder"), true, 256)).clone()
}

pub fn cached_shell_icon(path: &Path, is_dir: bool, size: u32) -> Option<Arc<RenderImage>> {
    let key = (icon_cache_key(path, is_dir), icon_size_bucket(size));
    icon_cache().lock().ok()?.get(&key)?.clone()
}

fn icon_size_bucket(size: u32) -> u32 {
    if size >= 64 { 64 } else if size >= 28 { 28 } else { 0 }
}

fn icon_cache() -> &'static Mutex<IconCache> {
    static CACHE: OnceLock<Mutex<IconCache>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn icon_cache_key(path: &Path, is_dir: bool) -> (String, bool) {
    if is_dir {
        return (path.to_string_lossy().to_lowercase(), true);
    }
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if matches!(extension.as_str(), "exe" | "lnk" | "ico" | "msi") {
        return (path.to_string_lossy().to_lowercase(), false);
    }
    (extension, false)
}

/// Los proveedores de iconos y miniaturas del shell no están garantizados
/// para trabajar en paralelo. Serializarlos evita cierres nativos al abrir
/// carpetas con muchos archivos o imágenes grandes.
fn shell_com_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn load_shell_icon(path: &Path, is_dir: bool, size: u32) -> Option<Arc<RenderImage>> {
    let _shell_guard = shell_com_lock().lock().ok();
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = (|| {
            let path_data = wide(path);
            let attributes = if is_dir {
                FILE_ATTRIBUTE_DIRECTORY
            } else {
                FILE_ATTRIBUTE_NORMAL
            };
            let mut info = SHFILEINFOW::default();
            let mut list = SHGetFileInfoW(
                PCWSTR(path_data.as_ptr()),
                attributes,
                Some(&mut info),
                size_of::<SHFILEINFOW>() as u32,
                SHGFI_SYSICONINDEX,
            );
            if list == 0 {
                list = SHGetFileInfoW(
                    PCWSTR(path_data.as_ptr()),
                    attributes,
                    Some(&mut info),
                    size_of::<SHFILEINFOW>() as u32,
                    SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES,
                );
            }
            if list == 0 {
                return None;
            }

            let candidates: &[u32] = if size >= 64 {
                &[SHIL_JUMBO, SHIL_EXTRALARGE, SHIL_LARGE]
            } else if size >= 28 {
                &[SHIL_EXTRALARGE, SHIL_LARGE]
            } else {
                &[SHIL_LARGE]
            };

            for image_list_type in candidates {
                let Ok(image_list) = SHGetImageList::<IImageList>(*image_list_type as i32) else {
                    continue;
                };
                let Ok(icon) = image_list.GetIcon(info.iIcon, ILD_TRANSPARENT.0) else {
                    continue;
                };
                let image = hicon_to_image(icon);
                let _ = DestroyIcon(icon);
                if image.is_some() {
                    return image;
                }
            }
            None
        })();
        if initialized {
            CoUninitialize();
        }
        result
    }
}

fn hicon_to_image(icon: HICON) -> Option<Arc<RenderImage>> {
    unsafe {
        let mut info = ICONINFO::default();
        if GetIconInfo(icon, &mut info).is_err() {
            return None;
        }
        let image = if !info.hbmColor.is_invalid() {
            bitmap_to_image(info.hbmColor)
        } else {
            None
        };
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        }
        image
    }
}

/// Miniatura nativa del shell. Devuelve píxeles BGRA listos para GPUI.
pub fn shell_thumbnail(path: &Path, size: u32) -> Option<Arc<RenderImage>> {
    let _shell_guard = shell_com_lock().lock().ok();
    shell_item_image(path, size, SIIGBF(SIIGBF_RESIZETOFIT.0 | SIIGBF_BIGGERSIZEOK.0))
}

fn shell_item_image(path: &Path, size: u32, flags: SIIGBF) -> Option<Arc<RenderImage>> {
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let result = (|| {
            let path_data = wide(path);
            let factory: IShellItemImageFactory = SHCreateItemFromParsingName(
                PCWSTR(path_data.as_ptr()),
                None::<&IBindCtx>,
            )
            .ok()?;
            let bitmap = factory
                .GetImage(
                    SIZE {
                        cx: size as i32,
                        cy: size as i32,
                    },
                    flags,
                )
                .ok()?;
            bitmap_to_image(bitmap)
        })();
        if initialized {
            CoUninitialize();
        }
        result
    }
}

fn bitmap_to_image(bitmap: windows::Win32::Graphics::Gdi::HBITMAP) -> Option<Arc<RenderImage>> {
    let image = read_bitmap(bitmap, 8192);
    let _ = unsafe { DeleteObject(HGDIOBJ(bitmap.0)) };
    image
}

pub(crate) fn menu_bitmap_to_image(bitmap: windows::Win32::Graphics::Gdi::HBITMAP) -> Option<Arc<RenderImage>> {
    read_bitmap(bitmap, 128)
}

fn read_bitmap(bitmap: windows::Win32::Graphics::Gdi::HBITMAP, limit: u32) -> Option<Arc<RenderImage>> {
    unsafe {
        let mut info = BITMAP::default();
        if GetObjectW(
            HGDIOBJ(bitmap.0),
            size_of::<BITMAP>() as i32,
            Some(&mut info as *mut BITMAP as *mut c_void),
        ) == 0
        {
            return None;
        }

        let width = info.bmWidth.max(1) as u32;
        let height = info.bmHeight.max(1) as u32;
        if width > limit || height > limit {
            return None;
        }

        let mut header = BITMAPINFO::default();
        header.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        header.bmiHeader.biWidth = width as i32;
        header.bmiHeader.biHeight = -(height as i32);
        header.bmiHeader.biPlanes = 1;
        header.bmiHeader.biBitCount = 32;
        header.bmiHeader.biCompression = BI_RGB.0 as u32;

        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let dc = GetDC(None);
        let read = GetDIBits(
            dc,
            bitmap,
            0,
            height,
            Some(pixels.as_mut_ptr() as *mut c_void),
            &mut header,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, dc);
        if read == 0 {
            return None;
        }

        let has_alpha = pixels.chunks_exact(4).any(|pixel| pixel[3] != 0);
        if has_alpha {
            for pixel in pixels.chunks_exact_mut(4) {
                let alpha = pixel[3] as u32;
                if alpha == 0 {
                    pixel[0] = 0;
                    pixel[1] = 0;
                    pixel[2] = 0;
                } else if alpha < 255 {
                    pixel[0] = ((pixel[0] as u32 * 255) / alpha).min(255) as u8;
                    pixel[1] = ((pixel[1] as u32 * 255) / alpha).min(255) as u8;
                    pixel[2] = ((pixel[2] as u32 * 255) / alpha).min(255) as u8;
                }
            }
        } else {
            for pixel in pixels.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
        }

        let image = image::RgbaImage::from_raw(width, height, pixels)?;
        Some(Arc::new(RenderImage::new(smallvec::smallvec![
            image::Frame::new(image)
        ])))
    }
}

#[cfg(test)]
mod drive_usage_tests {
    use super::DriveInfo;

    #[test]
    fn drive_bar_shows_used_space_and_handles_unknown_capacity() {
        let mut drive = DriveInfo {
            letter: "D:".into(),
            label: None,
            path: std::path::PathBuf::new(),
            total: 1000,
            free: 750,
            icon: None,
        };
        assert_eq!(drive.used_fraction(), 0.25);
        drive.free = drive.total;
        assert_eq!(drive.used_fraction(), 0.0);
        drive.free = 0;
        assert_eq!(drive.used_fraction(), 1.0);
        drive.total = 0;
        assert_eq!(drive.used_fraction(), 0.0);
    }
}
#[cfg(test)]
mod file_transfer_tests {
    use super::*;

    #[test]
    fn undo_move_preserves_an_occupied_destination_and_file_identity() {
        let root = std::env::temp_dir().join(format!(
            "xool-undo-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let from = root.join("moved.txt");
        let to = root.join("original.txt");
        std::fs::write(&from, b"moved").unwrap();
        std::fs::write(&to, b"occupied").unwrap();
        let identity = file_identity(&from).unwrap();
        let blocked = undo_move(&from, &to).is_err();
        let occupied = std::fs::read(&to).unwrap();
        std::fs::remove_file(&to).unwrap();
        let restored = undo_move(&from, &to).is_ok();
        let same_file = file_identity(&to).ok() == Some(identity);
        let contents = std::fs::read(&to).unwrap_or_default();
        let _ = std::fs::remove_file(&from);
        let _ = std::fs::remove_file(&to);
        std::fs::remove_dir(root).unwrap();
        assert!(blocked && restored && same_file);
        assert_eq!(occupied, b"occupied");
        assert_eq!(contents, b"moved");
    }

    #[test]
    fn copies_and_moves_preserve_existing_files() {
        let root = std::env::temp_dir().join(format!(
            "xool-transfer-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
        }
        let _cleanup = Cleanup(root.clone());

        let source = root.join("original.png");
        let other = root.join("destino");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(&source, b"contenido original").unwrap();
        let sources = [source.clone()];
        assert_eq!(copy_items(&sources, &root), Ok(1));
        assert_eq!(copy_items(&sources, &root), Ok(1));
        assert_eq!(move_items(&sources, &root), Ok(0));

        let local_copies: Vec<_> = std::fs::read_dir(&root).unwrap().flatten()
            .map(|entry| entry.path()).filter(|path| path.is_file()).collect();
        assert_eq!(local_copies.len(), 3);
        for path in local_copies {
            assert_eq!(path.extension().and_then(OsStr::to_str), Some("png"));
            assert_eq!(std::fs::read(path).unwrap(), b"contenido original");
        }

        let existing = other.join("original.png");
        std::fs::write(&existing, b"archivo existente").unwrap();
        assert_eq!(copy_items(&sources, &other), Ok(1));
        assert!(source.is_file());
        assert_eq!(move_items(&sources, &other), Ok(1));
        assert!(!source.exists());
        assert_eq!(std::fs::read(existing).unwrap(), b"archivo existente");
        assert_eq!(std::fs::read_dir(&other).unwrap().count(), 3);

        let folder = root.join(tr("carpeta", "folder"));
        let inner = folder.join("interior");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("archivo.txt"), b"carpeta").unwrap();
        assert!(copy_items(std::slice::from_ref(&folder), &inner).is_err());
        assert!(move_items(std::slice::from_ref(&folder), &inner).is_err());
        assert_eq!(copy_items(std::slice::from_ref(&folder), &root), Ok(1));
        assert_eq!(std::fs::read_dir(&root).unwrap().flatten().filter(|entry| entry.path().is_dir()).count(), 3);
    }
}
