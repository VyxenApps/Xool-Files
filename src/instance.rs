//! Routes new launches to the main window using the active settings profile.
use anyhow::bail;
use std::{
    ffi::OsString,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM},
        System::{
            DataExchange::COPYDATASTRUCT,
            Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject},
        },
        UI::WindowsAndMessaging::{
            AllowSetForegroundWindow, EnumWindows, GetPropW, GetWindowThreadProcessId,
            MB_ICONWARNING, MB_OK, MessageBoxW, RemovePropW, SMTO_ABORTIFHUNG, SMTO_BLOCK,
            SendMessageTimeoutW, SetPropW, WM_COPYDATA,
        },
    },
    core::{BOOL, PCWSTR, w},
};

const LAUNCH_REQUEST: usize = 0x5a46_0001;
const MAX_PATH_BYTES: usize = 65_536;

struct MutexHandle(HANDLE);

impl Drop for MutexHandle {
    fn drop(&mut self) {
        unsafe { let _ = CloseHandle(self.0); }
    }
}

pub(crate) struct Primary(MutexHandle);

impl Drop for Primary {
    fn drop(&mut self) {
        unsafe { let _ = ReleaseMutex(self.0.0); }
    }
}

fn profile_id() -> u64 {
    static ID: OnceLock<u64> = OnceLock::new();
    *ID.get_or_init(|| {
        let path = crate::settings::settings_path().unwrap_or_default();
        let key = path.to_string_lossy().replace('/', "\\").to_lowercase();
        key.bytes().fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        })
    })
}

fn window_property() -> PCWSTR {
    static NAME: OnceLock<Vec<u16>> = OnceLock::new();
    PCWSTR(NAME.get_or_init(|| crate::platform::wide(format!("XoolFiles.Launch.{:016x}", profile_id()))).as_ptr())
}

impl Primary {
    /// Keep ownership for the entire GUI run. Starting concurrently or while
    /// the primary window is being created must never create a second window.
    pub(crate) fn start(folder: Option<&Path>) -> anyhow::Result<Option<Self>> {
        let name = crate::platform::wide(format!("Local\\XoolFiles.Instance.{:016x}", profile_id()));
        let mutex = MutexHandle(unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr()))? });
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match unsafe { WaitForSingleObject(mutex.0, 0) } {
                WAIT_OBJECT_0 | WAIT_ABANDONED => return Ok(Some(Self(mutex))),
                WAIT_TIMEOUT => {}
                _ => return Err(windows::core::Error::from_win32().into()),
            }
            if let Some(hwnd) = primary_window() {
                if forward(hwnd, folder) {
                    return Ok(None);
                }
                if unsafe { !GetPropW(hwnd, window_property()).is_invalid() } {
                    crate::localized!(bail, "La ventana principal de Xool Files no respondió.", "The main Xool Files window did not respond.");
                }
            }
            if Instant::now() >= deadline {
                crate::localized!(bail, "La ventana principal de Xool Files no respondió.", "The main Xool Files window did not respond.");
            }
            thread::sleep(Duration::from_millis(40));
        }
    }
}

pub(crate) fn publish(window: &gpui::Window) -> windows::core::Result<()> {
    unsafe { SetPropW(crate::platform::hwnd(window), window_property(), Some(HANDLE(1 as *mut _))) }
}

pub(crate) fn unpublish(hwnd: HWND) {
    unsafe { let _ = RemovePropW(hwnd, window_property()); }
}

fn primary_window() -> Option<HWND> {
    unsafe extern "system" fn find(hwnd: HWND, target: LPARAM) -> BOOL {
        if unsafe { GetPropW(hwnd, window_property()).is_invalid() } {
            return BOOL(1);
        }
        unsafe { *(target.0 as *mut HWND) = hwnd; }
        BOOL(0)
    }

    let mut hwnd = HWND::default();
    unsafe { let _ = EnumWindows(Some(find), LPARAM((&mut hwnd as *mut HWND) as isize)); }
    (!hwnd.is_invalid()).then_some(hwnd)
}

fn forward(hwnd: HWND, folder: Option<&Path>) -> bool {
    let mut path: Vec<u16> = folder.map(|path| path.as_os_str().encode_wide().collect()).unwrap_or_default();
    if path.len() > MAX_PATH_BYTES / 2 { return false; }
    let payload = COPYDATASTRUCT {
        dwData: LAUNCH_REQUEST,
        cbData: (path.len() * 2) as u32,
        lpData: if path.is_empty() { std::ptr::null_mut() } else { path.as_mut_ptr().cast() },
    };
    let mut acknowledged = 0;
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != 0 { let _ = AllowSetForegroundWindow(pid); }
        let sent = SendMessageTimeoutW(hwnd, WM_COPYDATA, WPARAM(0),
            LPARAM((&payload as *const COPYDATASTRUCT) as isize),
            SMTO_ABORTIFHUNG | SMTO_BLOCK, 1500, Some(&mut acknowledged));
        sent.0 != 0 && acknowledged == 1
    }
}

/// Called only from the window's WM_COPYDATA handler, before Windows releases the payload.
pub(crate) unsafe fn receive(lparam: LPARAM) -> Option<Option<PathBuf>> {
    let data = unsafe { (lparam.0 as *const COPYDATASTRUCT).as_ref()? };
    if data.dwData != LAUNCH_REQUEST { return None; }
    let length = data.cbData as usize;
    if length == 0 { return Some(None); }
    if length > MAX_PATH_BYTES || length % 2 != 0 || data.lpData.is_null() { return None; }
    let bytes = unsafe { std::slice::from_raw_parts(data.lpData.cast::<u8>(), length) };
    let units: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
    if units.contains(&0) { return None; }
    let folder = PathBuf::from(OsString::from_wide(&units));
    folder.is_absolute().then_some(Some(folder))
}

pub(crate) fn report_start_error() {
    unsafe {
        let _ = MessageBoxW(None, windows::core::PCWSTR(windows::core::HSTRING::from(crate::i18n::tr("No se pudo recuperar la ventana de Xool Files. Comprueba si ya está abierta.", "Could not restore the Xool Files window. Check if it is already open.")).as_ptr()),
            w!("Xool Files"), MB_OK | MB_ICONWARNING);
    }
}

