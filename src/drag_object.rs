//! Objeto de datos que Xool Files entrega a OLE al arrastrar archivos fuera.
//!
//! El objeto del Shell (`BHID_DataObject`) implementa `IAsyncOperation`, y el
//! bucle de `DoDragDrop` lo pone en modo asíncrono. En ese modo `GetData`
//! puede responder `E_PENDING` mientras prepara el contenido, y los programas
//! que no lo contemplan interpretan la respuesta como una lista vacía. Por eso
//! el arrastre se entrega envuelto en un `IDataObject` propio que solo expone
//! esa interfaz: sin `IAsyncOperation` no hay modo asíncrono y los datos se
//! entregan completos en la primera consulta.
//!
//! Con `XOOLFILES_DRAG_PROBE` definido, además anota en `drag-formats.log` qué
//! formatos consulta el destino, para diagnosticar arrastres concretos.

use std::collections::HashMap;
use std::sync::OnceLock;

use windows::core::{implement, PCWSTR, Ref, Result as WinResult, BOOL, HRESULT};
use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::System::Com::{
    IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, FORMATETC, STGMEDIUM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::UI::Shell::DROPFILES;

#[implement(IDataObject)]
struct SynchronousDataObject {
    inner: IDataObject,
    log: bool,
}

/// Envuelve el objeto del Shell para que OLE no pueda activar el modo
/// asíncrono, que deja a algunos destinos sin datos al soltar.
pub fn synchronous(inner: IDataObject) -> IDataObject {
    let log = std::env::var_os("XOOLFILES_DRAG_PROBE").is_some();
    SynchronousDataObject { inner, log }.into()
}

impl SynchronousDataObject {
    fn note(&self, message: String) {
        if self.log {
            crate::platform::drag_diagnostic_append(&message);
        }
    }
}

fn names() -> &'static HashMap<u16, &'static str> {
    static NAMES: OnceLock<HashMap<u16, &'static str>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut map = HashMap::new();
        for (id, name) in [
            (1u16, "CF_TEXT"),
            (2, "CF_BITMAP"),
            (3, "CF_METAFILEPICT"),
            (8, "CF_DIB"),
            (13, "CF_UNICODETEXT"),
            (14, "CF_ENHMETAFILE"),
            (15, "CF_HDROP"),
            (16, "CF_LOCALE"),
            (17, "CF_DIBV5"),
        ] {
            map.insert(id, name);
        }
        for name in [
            "Shell IDList Array",
            "Preferred DropEffect",
            "Performed DropEffect",
            "Paste Succeeded",
            "TargetCLSID",
            "FileName",
            "FileNameW",
            "FileContents",
            "FileGroupDescriptor",
            "FileGroupDescriptorW",
            "Shell Object Offsets",
            "DragImageBits",
            "DragContext",
            "UsingDefaultDragImage",
            "InShellDragLoop",
            "DragSourceHelperFlags",
            "ZoneIdentifier",
            "Logical Filename",
        ] {
            let wide = crate::platform::wide(name);
            let id = unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) };
            if let Ok(id) = u16::try_from(id) {
                map.insert(id, name);
            }
        }
        map
    })
}

fn describe(format: *const FORMATETC) -> String {
    let Some(format) = (unsafe { format.as_ref() }) else {
        return "<null>".into();
    };
    let id = format.cfFormat;
    let name = names().get(&id).copied().unwrap_or("?");
    format!(
        "cf={id}({name}) tymed={} aspect={} lindex={}",
        format.tymed, format.dwAspect, format.lindex
    )
}

fn outcome<T>(result: &WinResult<T>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(_) => "error",
    }
}

impl IDataObject_Impl for SynchronousDataObject_Impl {
    fn GetData(&self, format: *const FORMATETC) -> WinResult<STGMEDIUM> {
        let result = unsafe { self.inner.GetData(format) };
        if self.log {
            let mut detail = String::new();
            if let (Ok(medium), Some(info)) = (&result, unsafe { format.as_ref() }) {
                if info.cfFormat == 15 {
                    detail = format!(" files={:?}", hdrop_paths(medium));
                }
            }
            self.note(format!(
                "GetData {} -> {}{detail}",
                describe(format),
                outcome(&result)
            ));
        }
        result
    }

    fn GetDataHere(&self, format: *const FORMATETC, medium: *mut STGMEDIUM) -> WinResult<()> {
        let result = unsafe { self.inner.GetDataHere(format, medium) };
        if self.log {
            self.note(format!("GetDataHere {} -> {}", describe(format), outcome(&result)));
        }
        result
    }

    fn QueryGetData(&self, format: *const FORMATETC) -> HRESULT {
        let result = unsafe { self.inner.QueryGetData(format) };
        if self.log {
            self.note(format!("QueryGetData {} -> {result:?}", describe(format)));
        }
        result
    }

    fn GetCanonicalFormatEtc(&self, input: *const FORMATETC, output: *mut FORMATETC) -> HRESULT {
        let result = unsafe { self.inner.GetCanonicalFormatEtc(input, output) };
        if self.log {
            self.note(format!("GetCanonicalFormatEtc {} -> {result:?}", describe(input)));
        }
        result
    }

    fn SetData(
        &self,
        format: *const FORMATETC,
        medium: *const STGMEDIUM,
        release: BOOL,
    ) -> WinResult<()> {
        let result = unsafe { self.inner.SetData(format, medium, release.as_bool()) };
        if self.log {
            self.note(format!("SetData {} -> {}", describe(format), outcome(&result)));
        }
        result
    }

    fn EnumFormatEtc(&self, direction: u32) -> WinResult<IEnumFORMATETC> {
        let result = unsafe { self.inner.EnumFormatEtc(direction) };
        if self.log {
            let mut list = Vec::new();
            if let Ok(enumerator) = unsafe { self.inner.EnumFormatEtc(direction) } {
                loop {
                    let mut item = FORMATETC::default();
                    let mut fetched = 0u32;
                    let more = unsafe {
                        enumerator.Next(std::slice::from_mut(&mut item), Some(&mut fetched))
                    };
                    if more.is_err() || fetched == 0 {
                        break;
                    }
                    list.push(describe(&item));
                }
            }
            self.note(format!(
                "EnumFormatEtc dir={direction} -> {} [{}]",
                outcome(&result),
                list.join("; ")
            ));
        }
        result
    }

    fn DAdvise(
        &self,
        format: *const FORMATETC,
        flags: u32,
        sink: Ref<'_, IAdviseSink>,
    ) -> WinResult<u32> {
        let result = unsafe { self.inner.DAdvise(format, flags, sink.as_ref()) };
        if self.log {
            self.note(format!("DAdvise {} -> {}", describe(format), outcome(&result)));
        }
        result
    }

    fn DUnadvise(&self, connection: u32) -> WinResult<()> {
        let result = unsafe { self.inner.DUnadvise(connection) };
        if self.log {
            self.note(format!("DUnadvise {connection} -> {}", outcome(&result)));
        }
        result
    }

    fn EnumDAdvise(&self) -> WinResult<IEnumSTATDATA> {
        let result = unsafe { self.inner.EnumDAdvise() };
        if self.log {
            self.note(format!("EnumDAdvise -> {}", outcome(&result)));
        }
        result
    }
}

/// Lee los nombres que contiene un `CF_HDROP` para comprobar la ruta entregada.
fn hdrop_paths(medium: &STGMEDIUM) -> Vec<String> {
    let mut paths = Vec::new();
    unsafe {
        let handle: HGLOBAL = medium.u.hGlobal;
        if handle.is_invalid() {
            return paths;
        }
        let base = GlobalLock(handle) as *const u8;
        if base.is_null() {
            return paths;
        }
        let header = &*(base as *const DROPFILES);
        let start = base.add(header.pFiles as usize);
        if header.fWide.as_bool() {
            let mut cursor = start as *const u16;
            while *cursor != 0 {
                let mut len = 0usize;
                while *cursor.add(len) != 0 {
                    len += 1;
                }
                let slice = std::slice::from_raw_parts(cursor, len);
                paths.push(String::from_utf16_lossy(slice));
                cursor = cursor.add(len + 1);
            }
        } else {
            let mut cursor = start;
            while *cursor != 0 {
                let mut len = 0usize;
                while *cursor.add(len) != 0 {
                    len += 1;
                }
                let slice = std::slice::from_raw_parts(cursor, len);
                paths.push(String::from_utf8_lossy(slice).into_owned());
                cursor = cursor.add(len + 1);
            }
        }
        let _ = GlobalUnlock(handle);
    }
    paths
}
