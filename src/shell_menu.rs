//! Context menus contributed by Windows and installed Shell extensions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use futures::channel::oneshot;
use gpui::RenderImage;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize, IBindCtx,
};
use windows::Win32::UI::Shell::{
    CMF_NORMAL, CMF_SYNCCASCADEMENU, CMINVOKECOMMANDINFO, Common::ITEMIDLIST, ECF_HASSUBCOMMANDS,
    ECF_ISSEPARATOR, ECS_CHECKED, ECS_DISABLED, ECS_HIDDEN, FOLDERID_RecycleBinFolder, GCS_VERBW,
    IContextMenu, IContextMenu2, IContextMenu3, IExplorerCommand, IShellFolder, IShellItemArray,
    IShellView, SHBindToParent, SHCreateShellItemArrayFromIDLists, SHGetDesktopFolder,
    SHGetKnownFolderIDList, SHParseDisplayName, SVGIO_BACKGROUND,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyMenu, GetMenuItemCount, GetMenuItemInfoW, HMENU, MENUITEMINFOW,
    MFS_CHECKED, MFS_DISABLED, MFT_SEPARATOR, MIIM_BITMAP, MIIM_FTYPE, MIIM_ID, MIIM_STATE,
    MIIM_STRING, MIIM_SUBMENU, SW_SHOWNORMAL, WM_INITMENUPOPUP,
};
use windows::core::{GUID, IUnknown, Interface, PCSTR, PCWSTR, PSTR, PWSTR};

use crate::{fs_model, platform};

const FIRST: u32 = 1;
const LAST: u32 = 0x7fff;
const MODERN_COMMAND: u32 = 0x10000;
// Class published by WinRAR's FileExplorerContextMenus manifest.
const WINRAR_COMMAND: GUID = GUID::from_u128(0xb41db860_64e4_11d2_9906_e49fadc173ca);

#[derive(Clone, PartialEq, Eq)]
pub enum ShellTarget {
    Selection(Vec<PathBuf>),
    SidebarLocation(PathBuf),
    Background(PathBuf),
}

#[derive(Clone, Copy)]
pub enum ShellAction {
    Invoke(u32),
    Expand(u32),
}

#[derive(Clone)]
pub enum ShellRow {
    Separator,
    Item {
        label: String,
        icon: Option<Arc<RenderImage>>,
        action: ShellAction,
        enabled: bool,
        checked: bool,
    },
}

pub type ShellRows = Arc<[ShellRow]>;

#[derive(Clone)]
pub struct ShellClient(mpsc::Sender<Request>);

enum Request {
    Expand(u32, oneshot::Sender<ShellRows>),
    Invoke(u32, bool, oneshot::Sender<Result<(), String>>),
}

impl ShellClient {
    pub fn start(owner: isize, target: ShellTarget) -> (Self, oneshot::Receiver<ShellRows>) {
        let (requests, worker) = mpsc::channel();
        let (ready, result) = oneshot::channel();
        std::thread::spawn(move || {
            if unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_err() {
                return;
            }
            {
                if let Some(mut menu) = ShellMenu::open(HWND(owner as *mut _), target)
                    && ready.send(menu.root_rows()).is_ok()
                {
                    while let Ok(request) = worker.recv() {
                        match request {
                            Request::Expand(key, result) => {
                                let _ = result.send(menu.expand(key));
                            }
                            Request::Invoke(offset, synchronous, result) => {
                                let _ = result.send(menu.invoke(offset, synchronous));
                                break;
                            }
                        }
                    }
                }
            }
            unsafe { CoUninitialize() };
        });
        (Self(requests), result)
    }

    pub fn expand(&self, key: u32) -> oneshot::Receiver<ShellRows> {
        let (result, receiver) = oneshot::channel();
        let _ = self.0.send(Request::Expand(key, result));
        receiver
    }

    pub fn invoke(&self, offset: u32, synchronous: bool) -> oneshot::Receiver<Result<(), String>> {
        let (result, receiver) = oneshot::channel();
        let _ = self.0.send(Request::Invoke(offset, synchronous, result));
        receiver
    }
}

struct Pidl(*mut ITEMIDLIST);

impl Pidl {
    fn parse(path: &Path) -> windows::core::Result<Self> {
        unsafe {
            if fs_model::is_recycle_bin(path) {
                return SHGetKnownFolderIDList(&FOLDERID_RecycleBinFolder, 0, None).map(Self);
            }
            let name = platform::wide(path);
            let mut pidl = std::ptr::null_mut();
            SHParseDisplayName(PCWSTR(name.as_ptr()), None::<&IBindCtx>, &mut pidl, 0, None)?;
            Ok(Self(pidl))
        }
    }
}

impl Drop for Pidl {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.cast())) };
    }
}

struct MenuHandle(HMENU);

impl Drop for MenuHandle {
    fn drop(&mut self) {
        let _ = unsafe { DestroyMenu(self.0) };
    }
}

struct ShellMenu {
    context: IContextMenu,
    root: MenuHandle,
    submenus: HashMap<u32, (HMENU, u32)>,
    owner: HWND,
    background: bool,
    sidebar_location: bool,
    winrar: Option<ExplorerCommands>,
    _view: Option<IShellView>,
}

impl ShellMenu {
    fn open(owner: HWND, target: ShellTarget) -> Option<Self> {
        let background = matches!(target, ShellTarget::Background(_));
        let sidebar_location = matches!(target, ShellTarget::SidebarLocation(_));
        let mut view = None;
        let mut winrar = None;
        let context = unsafe {
            match &target {
                ShellTarget::Selection(_) | ShellTarget::SidebarLocation(_) => {
                    let paths = match &target {
                        ShellTarget::Selection(paths) => paths.as_slice(),
                        ShellTarget::SidebarLocation(path) => std::slice::from_ref(path),
                        ShellTarget::Background(_) => unreachable!(),
                    };
                    let first = paths.first()?;
                    if paths.iter().any(|path| path.parent() != first.parent()) {
                        return None;
                    }
                    let pidls: Vec<_> = paths
                        .iter()
                        .map(|path| Pidl::parse(path))
                        .collect::<windows::core::Result<_>>()
                        .ok()?;
                    if first.parent().is_some() && !fs_model::is_recycle_bin(first) {
                        winrar = ExplorerCommands::winrar(&pidls);
                    }
                    let mut children = Vec::with_capacity(pidls.len());
                    let mut parent = None;
                    for pidl in &pidls {
                        let mut child = std::ptr::null_mut();
                        let folder: IShellFolder = SHBindToParent(pidl.0, Some(&mut child)).ok()?;
                        parent.get_or_insert(folder);
                        children.push(child.cast_const());
                    }
                    parent?.GetUIObjectOf(owner, &children, None).ok()?
                }
                ShellTarget::Background(path) => {
                    let pidl = Pidl::parse(path).ok()?;
                    let desktop = SHGetDesktopFolder().ok()?;
                    let folder: IShellFolder =
                        desktop.BindToObject(pidl.0, None::<&IBindCtx>).ok()?;
                    if let Ok(context) = folder.CreateViewObject::<IContextMenu>(owner) {
                        context
                    } else {
                        let shell_view = folder.CreateViewObject::<IShellView>(owner).ok()?;
                        let context = shell_view.GetItemObject(SVGIO_BACKGROUND).ok()?;
                        view = Some(shell_view);
                        context
                    }
                }
            }
        };
        let root = MenuHandle(unsafe { CreatePopupMenu() }.ok()?);
        unsafe {
            context.QueryContextMenu(root.0, 0, FIRST, LAST, CMF_NORMAL | CMF_SYNCCASCADEMENU)
        }
        .ok()
        .ok()?;
        Some(Self {
            context,
            root,
            submenus: HashMap::new(),
            owner,
            background,
            sidebar_location,
            winrar,
            _view: view,
        })
    }

    fn root_rows(&mut self) -> ShellRows {
        self.init_popup(self.root.0, 0);
        let mut rows = self.rows(self.root.0, true);
        let has_winrar = rows.iter().any(|row| match row {
            ShellRow::Item { label, .. } => label.to_ascii_lowercase().contains("winrar"),
            ShellRow::Separator => false,
        });
        if !has_winrar
            && let Some(winrar) = self.winrar.as_mut()
            && let Some(row) = winrar.root_row()
        {
            rows.insert(0, row);
            if rows.len() > 1 {
                rows.insert(1, ShellRow::Separator);
            }
        }
        rows.into()
    }

    fn expand(&mut self, key: u32) -> ShellRows {
        if key >= MODERN_COMMAND {
            return self
                .winrar
                .as_mut()
                .map(|commands| commands.expand(key - MODERN_COMMAND))
                .unwrap_or_else(|| Arc::from([]));
        }
        let Some(&(menu, index)) = self.submenus.get(&key) else {
            return Arc::from([]);
        };
        self.init_popup(menu, index);
        self.rows(menu, false).into()
    }

    fn init_popup(&self, menu: HMENU, index: u32) {
        unsafe {
            let wparam = WPARAM(menu.0 as usize);
            let lparam = LPARAM(index as isize);
            if let Ok(handler) = self.context.cast::<IContextMenu3>() {
                let _ = handler.HandleMenuMsg2(WM_INITMENUPOPUP, wparam, lparam, None);
            } else if let Ok(handler) = self.context.cast::<IContextMenu2>() {
                let _ = handler.HandleMenuMsg(WM_INITMENUPOPUP, wparam, lparam);
            }
        }
    }

    fn rows(&mut self, menu: HMENU, root: bool) -> Vec<ShellRow> {
        let mut rows = Vec::new();
        for position in 0..unsafe { GetMenuItemCount(Some(menu)) }.max(0) as u32 {
            let mut label = [0u16; 512];
            let mut info = MENUITEMINFOW {
                cbSize: size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_ID | MIIM_STRING | MIIM_FTYPE | MIIM_STATE | MIIM_SUBMENU | MIIM_BITMAP,
                dwTypeData: PWSTR(label.as_mut_ptr()),
                cch: label.len() as u32,
                ..Default::default()
            };
            if unsafe { GetMenuItemInfoW(menu, position, true, &mut info) }.is_err() {
                continue;
            }
            if info.fType.contains(MFT_SEPARATOR) {
                if matches!(rows.last(), Some(ShellRow::Item { .. })) {
                    rows.push(ShellRow::Separator);
                }
                continue;
            }
            let label = menu_label(&String::from_utf16_lossy(
                &label[..(info.cch as usize).min(label.len())],
            ));
            if label.is_empty() {
                continue;
            }
            let offset = (FIRST..=LAST).contains(&info.wID).then(|| info.wID - FIRST);
            if root && self.replaced_by_xool(offset, &label) {
                continue;
            }
            let action = if !info.hSubMenu.is_invalid() {
                let key = self.submenus.len() as u32;
                self.submenus.insert(key, (info.hSubMenu, position));
                ShellAction::Expand(key)
            } else if let Some(offset) = offset {
                ShellAction::Invoke(offset)
            } else {
                continue;
            };
            let icon = platform::windows_menu_icon(&label)
                .or_else(|| {
                    (info.hbmpItem.0 as isize > 12)
                        .then(|| platform::menu_bitmap_to_image(info.hbmpItem))
                        .flatten()
                })
                .or_else(|| platform::menu_application_icon(&label));
            rows.push(ShellRow::Item {
                label,
                icon,
                action,
                enabled: !info.fState.contains(MFS_DISABLED),
                checked: info.fState.contains(MFS_CHECKED),
            });
        }
        if matches!(rows.last(), Some(ShellRow::Separator)) {
            rows.pop();
        }
        rows
    }

    fn replaced_by_xool(&self, offset: Option<u32>, label: &str) -> bool {
        let mut verb = [0u16; 128];
        if let Some(offset) = offset {
            let _ = unsafe {
                self.context.GetCommandString(
                    offset as usize,
                    GCS_VERBW,
                    None,
                    PSTR(verb.as_mut_ptr().cast()),
                    verb.len() as u32,
                )
            };
        }
        let len = verb
            .iter()
            .position(|&letter| letter == 0)
            .unwrap_or(verb.len());
        let verb = String::from_utf16_lossy(&verb[..len]).to_ascii_lowercase();
        let label = label.trim_end_matches(['.', '…']).to_lowercase();
        if self.sidebar_location
            && (matches!(verb.as_str(), "copy" | "paste")
                || matches!(label.as_str(), "copiar" | "copy" | "pegar" | "paste"))
        {
            return false;
        }
        let common = matches!(verb.as_str(), "open" | "properties" | "refresh" | "paste")
            || matches!(
                label.as_str(),
                "abrir"
                    | "open"
                    | "propiedades"
                    | "properties"
                    | "actualizar"
                    | "refresh"
                    | "pegar"
                    | "paste"
            );
        if self.background {
            common || matches!(label.as_str(), "ver" | "view" | "ordenar por" | "sort by")
        } else {
            common
                || matches!(
                    verb.as_str(),
                    "cut" | "copy" | "delete" | "rename" | "copyaspath"
                )
                || matches!(
                    label.as_str(),
                    "cortar"
                        | "cut"
                        | "copiar"
                        | "copy"
                        | "eliminar"
                        | "delete"
                        | "cambiar nombre"
                        | "rename"
                        | "copiar como ruta de acceso"
                        | "copy as path"
                )
        }
    }

    fn invoke(&self, offset: u32, synchronous: bool) -> Result<(), String> {
        if offset >= MODERN_COMMAND {
            return self
                .winrar
                .as_ref()
                .ok_or_else(|| crate::i18n::tr("La acción de WinRAR ya no está disponible.", "The WinRAR action is no longer available.").to_owned())?
                .invoke(offset - MODERN_COMMAND);
        }
        let command = CMINVOKECOMMANDINFO {
            cbSize: size_of::<CMINVOKECOMMANDINFO>() as u32,
            fMask: if synchronous {
                windows::Win32::UI::Shell::SEE_MASK_NOASYNC
            } else {
                0
            },
            hwnd: self.owner,
            lpVerb: PCSTR(offset as usize as *const u8),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        unsafe { self.context.InvokeCommand(&command) }
            .map_err(|error| crate::localized!(format, "Windows no pudo ejecutar la acción: {error}", "Windows could not run the action: {error}"))
    }
}

struct ExplorerCommands {
    items: IShellItemArray,
    commands: Vec<IExplorerCommand>,
}

impl ExplorerCommands {
    fn winrar(pidls: &[Pidl]) -> Option<Self> {
        unsafe {
            let command: IExplorerCommand =
                CoCreateInstance(&WINRAR_COMMAND, None::<&IUnknown>, CLSCTX_INPROC_SERVER).ok()?;
            let absolute: Vec<_> = pidls.iter().map(|pidl| pidl.0.cast_const()).collect();
            let items = SHCreateShellItemArrayFromIDLists(&absolute).ok()?;
            Some(Self {
                items,
                commands: vec![command],
            })
        }
    }

    fn root_row(&self) -> Option<ShellRow> {
        let ShellRow::Item { enabled, .. } = self.row(0)? else {
            return None;
        };
        Some(ShellRow::Item {
            label: "WinRAR".to_owned(),
            icon: platform::menu_application_icon("WinRAR"),
            action: ShellAction::Expand(MODERN_COMMAND),
            enabled,
            checked: false,
        })
    }

    fn row(&self, index: u32) -> Option<ShellRow> {
        let command = self.commands.get(index as usize)?;
        unsafe {
            let flags = command.GetFlags().unwrap_or(0);
            if flags & ECF_ISSEPARATOR.0 as u32 != 0 {
                return Some(ShellRow::Separator);
            }
            let state = command.GetState(&self.items, true).ok()?;
            if state & ECS_HIDDEN.0 as u32 != 0 {
                return None;
            }
            let title = command.GetTitle(&self.items).ok()?;
            let label = menu_label(&title.to_string().unwrap_or_default());
            CoTaskMemFree(Some(title.0.cast()));
            if label.is_empty() {
                return None;
            }
            let flags = command.GetFlags().unwrap_or(flags);
            let id = MODERN_COMMAND + index;
            let action = if flags & ECF_HASSUBCOMMANDS.0 as u32 != 0 {
                ShellAction::Expand(id)
            } else {
                ShellAction::Invoke(id)
            };
            Some(ShellRow::Item {
                icon: None,
                label,
                action,
                enabled: state & ECS_DISABLED.0 as u32 == 0,
                checked: state & ECS_CHECKED.0 as u32 != 0,
            })
        }
    }

    fn expand(&mut self, index: u32) -> ShellRows {
        let Some(command) = self.commands.get(index as usize) else {
            return Arc::from([]);
        };
        let Ok(enumerator) = (unsafe { command.EnumSubCommands() }) else {
            return self.leaf_rows(index);
        };
        let mut rows = Vec::new();
        for _ in 0..100 {
            let mut next = [None];
            let mut fetched = 0;
            if unsafe { enumerator.Next(&mut next, Some(&mut fetched)) }.is_err() || fetched == 0 {
                break;
            }
            let [Some(next)] = next else { break };
            let id = self.commands.len() as u32;
            self.commands.push(next);
            if let Some(row) = self.row(id) {
                if matches!(row, ShellRow::Separator)
                    && !matches!(rows.last(), Some(ShellRow::Item { .. }))
                {
                    continue;
                }
                rows.push(row);
            }
        }
        if matches!(rows.last(), Some(ShellRow::Separator)) {
            rows.pop();
        }
        if rows.is_empty() {
            self.leaf_rows(index)
        } else {
            rows.into()
        }
    }

    fn leaf_rows(&self, index: u32) -> ShellRows {
        self.row(index)
            .filter(|row| {
                matches!(
                    row,
                    ShellRow::Item {
                        action: ShellAction::Invoke(_),
                        ..
                    }
                )
            })
            .map_or_else(|| Arc::from([]), |row| Arc::from([row]))
    }

    fn invoke(&self, index: u32) -> Result<(), String> {
        let command = self
            .commands
            .get(index as usize)
            .ok_or_else(|| crate::i18n::tr("La acción de WinRAR ya no está disponible.", "The WinRAR action is no longer available.").to_owned())?;
        unsafe { command.Invoke(&self.items, None::<&IBindCtx>) }
            .map_err(|error| crate::localized!(format, "WinRAR no pudo ejecutar la acción: {error}", "WinRAR could not run the action: {error}"))
    }
}

fn menu_label(label: &str) -> String {
    let mut parts = label
        .split('\t')
        .next()
        .unwrap_or_default()
        .chars()
        .peekable();
    let mut cleaned = String::new();
    while let Some(letter) = parts.next() {
        if letter != '&' || parts.next_if_eq(&'&').is_some() {
            cleaned.push(letter);
        }
    }
    cleaned.trim().to_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn strips_windows_accelerators_and_preserves_literal_ampersands() {
        assert_eq!(
            super::menu_label("  &Abrir && editar\tCtrl+A "),
            "Abrir & editar"
        );
    }

    #[test]
    #[ignore = "Requires the Windows Shell photo rotation handler"]
    fn windows_rotates_a_jpeg_right_and_left() {
        use super::{ShellAction, ShellClient, ShellRow, ShellTarget};
        use std::time::{Duration, Instant};

        fn receive<T>(mut reply: futures::channel::oneshot::Receiver<T>) -> T {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(result) = reply.try_recv().expect("Windows Shell reply") {
                    return result;
                }
                assert!(
                    Instant::now() < deadline,
                    "Windows Shell response timed out"
                );
                std::thread::sleep(Duration::from_millis(15));
            }
        }

        let path =
            std::env::temp_dir().join(format!("xool-shell-rotate-{}.jpg", std::process::id()));
        let fixture = image::RgbImage::from_fn(48, 32, |x, y| {
            image::Rgb([
                if x < 24 { 220 } else { 0 },
                if y < 16 { 120 } else { 0 },
                25,
            ])
        });
        fixture.save(&path).unwrap();

        for (names, expected) in [
            (["girar a la derecha", "rotate right"], (32, 48)),
            (["girar a la izquierda", "rotate left"], (48, 32)),
        ] {
            let (client, rows) = ShellClient::start(0, ShellTarget::Selection(vec![path.clone()]));
            let rows = receive(rows);
            let offset = rows
                .iter()
                .find_map(|row| match row {
                    ShellRow::Item {
                        label,
                        action: ShellAction::Invoke(offset),
                        enabled: true,
                        ..
                    } if names.contains(&label.to_lowercase().as_str()) => Some(*offset),
                    _ => None,
                })
                .expect("Windows photo rotation action");
            receive(client.invoke(offset, true)).expect("Windows rotated the image");
            let deadline = Instant::now() + Duration::from_secs(4);
            loop {
                if let Some(image) = crate::thumbs::image_thumb(&path, 64)
                    && (image.size(0).width.0, image.size(0).height.0) == expected
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "The image dimensions did not change"
                );
                std::thread::sleep(Duration::from_millis(30));
            }
        }

        let _ = std::fs::remove_file(path);
    }
}
