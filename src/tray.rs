use std::{cell::RefCell, collections::HashMap, mem::size_of, path::PathBuf, sync::OnceLock};

use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use gpui::Window;
use windows::{core::{PCWSTR, w}, Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, POINT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::{Shell::{NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW},
        WindowsAndMessaging::{AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, IsIconic, LoadIconW,
            MF_STRING, PostMessageW, RegisterWindowMessageW, SetForegroundWindow, ShowWindowAsync,
            SW_HIDE, SW_RESTORE, SW_SHOW, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
            WM_APP, WM_LBUTTONUP, WM_NCDESTROY, WM_NULL, WM_RBUTTONUP}},
}};

const CALLBACK: u32 = WM_APP + 17;
const ICON_ID: u32 = 1;

pub(crate) enum Event { Show(Option<PathBuf>), Exit }

struct Tray {
    registered: bool,
    wanted: bool,
    sender: UnboundedSender<Event>,
}

thread_local! {
    static TRAYS: RefCell<HashMap<isize, Tray>> = RefCell::new(HashMap::new());
}

fn taskbar_created() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

pub(crate) fn events(window: &Window) -> UnboundedReceiver<Event> {
    let (sender, receiver) = mpsc::unbounded();
    taskbar_created();
    TRAYS.with(|trays| {
        trays.borrow_mut().insert(crate::platform::hwnd_value(window),
            Tray { registered: false, wanted: false, sender });
    });
    receiver
}

fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: ICON_ID, ..Default::default() }
}

fn add_icon(hwnd: HWND) -> Result<(), String> {
    let mut data = icon_data(hwnd);
    data.hIcon = unsafe {
        let module = GetModuleHandleW(PCWSTR::null()).map_err(|error| error.to_string())?;
        LoadIconW(Some(HINSTANCE(module.0)), PCWSTR::from_raw(1 as *const u16)).map_err(|error| error.to_string())?
    };
    data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    data.uCallbackMessage = CALLBACK;
    for (slot, unit) in data.szTip.iter_mut().zip("Xool Files".encode_utf16()) { *slot = unit; }
    unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool().then_some(())
        .ok_or_else(|| crate::i18n::tr("Windows no pudo mostrar el icono en la bandeja.", "Windows could not display the tray icon.").into())
}

pub(crate) fn set_visible(window: &Window, visible: bool) -> Result<(), String> {
    let hwnd = crate::platform::hwnd(window);
    TRAYS.with(|trays| {
        let mut trays = trays.borrow_mut();
        let tray = trays.get_mut(&(hwnd.0 as isize)).ok_or(crate::i18n::tr("La bandeja no está inicializada.", "The system tray is not initialized."))?;
        if visible && !tray.registered {
            add_icon(hwnd)?;
            tray.registered = true;
        } else if !visible && tray.registered {
            unsafe { Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd)); }
            tray.registered = false;
        }
        tray.wanted = visible;
        Ok(())
    })
}

pub(crate) fn hide_if_reopenable(window: &Window) -> bool {
    let hwnd = crate::platform::hwnd(window);
    let registered = TRAYS.with(|trays| trays.borrow().get(&(hwnd.0 as isize)).is_some_and(|tray| tray.registered));
    registered && unsafe { ShowWindowAsync(hwnd, SW_HIDE) }.as_bool()
}

pub(crate) fn show(window: &Window) {
    let hwnd = crate::platform::hwnd(window);
    unsafe {
        let command = if IsIconic(hwnd).as_bool() { SW_RESTORE } else { SW_SHOW };
        let _ = ShowWindowAsync(hwnd, command);
        let _ = SetForegroundWindow(hwnd);
    }
    window.activate_window();
}

fn menu(hwnd: HWND) -> Option<Event> {
    let mut cursor = POINT::default();
    unsafe {
        GetCursorPos(&mut cursor).ok()?;
        let menu = CreatePopupMenu().ok()?;
        let _ = AppendMenuW(menu, MF_STRING, 1, windows::core::PCWSTR(windows::core::HSTRING::from(crate::i18n::tr("Abrir Xool Files", "Open Xool Files")).as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, 2, windows::core::PCWSTR(windows::core::HSTRING::from(crate::i18n::tr("Salir", "Quit")).as_ptr()));
        let _ = SetForegroundWindow(hwnd);
        let result = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
            cursor.x, cursor.y, None, hwnd, None).0;
        let _ = DestroyMenu(menu);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        match result {
            1 => Some(Event::Show(None)),
            2 => Some(Event::Exit),
            _ => None,
        }
    }
}

pub(crate) fn activate(hwnd: HWND, folder: Option<PathBuf>) -> bool {
    TRAYS.with(|trays| {
        trays.borrow().get(&(hwnd.0 as isize))
            .is_some_and(|tray| tray.sender.unbounded_send(Event::Show(folder)).is_ok())
    })
}

pub(crate) fn handle_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> bool {
    let key = hwnd.0 as isize;
    if msg == CALLBACK && wparam.0 == ICON_ID as usize {
        let sender = TRAYS.with(|trays| trays.borrow().get(&key).map(|tray| tray.sender.clone()));
        if let Some(sender) = sender {
            let event = match lparam.0 as u32 {
                WM_LBUTTONUP => Some(Event::Show(None)),
                WM_RBUTTONUP => menu(hwnd),
                _ => None,
            };
            if let Some(event) = event { let _ = sender.unbounded_send(event); }
            return true;
        }
    } else if msg == WM_NCDESTROY {
        TRAYS.with(|trays| {
            if trays.borrow_mut().remove(&key).is_some() {
                unsafe { Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd)); }
            }
        });
    } else if msg != 0 && msg == taskbar_created() {
        let lost_access = TRAYS.with(|trays| {
            let mut trays = trays.borrow_mut();
            let Some(tray) = trays.get_mut(&key).filter(|tray| tray.wanted) else { return false; };
            tray.registered = add_icon(hwnd).is_ok();
            !tray.registered
        });
        if lost_access { unsafe { let _ = ShowWindowAsync(hwnd, SW_SHOW); } }
    }
    false
}
