use crate::i18n::tr;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local};
use gpui::RenderImage;

#[cfg(windows)]
use windows::Win32::Storage::FileSystem::GetLogicalDrives;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Folder,
    Image,
    Video,
    Audio,
    Document,
    Code,
    Archive,
    Executable,
    Font,
    Other,
}

impl FileKind {
    pub fn from_path(path: &Path, is_dir: bool) -> Self {
        if is_dir {
            return Self::Folder;
        }
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "ico" | "svg"
            | "avif" | "heic" | "heif" => Self::Image,
            "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "m4v" | "flv" | "mpg" | "mpeg"
            | "m2ts" => Self::Video,
            "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "wma" | "opus" | "mid" | "midi" => {
                Self::Audio
            }
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "txt" | "md" | "rtf"
            | "odt" | "ods" | "odp" | "csv" | "epub" => Self::Document,
            "rs" | "js" | "ts" | "tsx" | "jsx" | "py" | "c" | "cpp" | "h" | "hpp" | "cs"
            | "java" | "go" | "rb" | "php" | "html" | "css" | "scss" | "json" | "yaml" | "yml"
            | "toml" | "xml" | "sh" | "bat" | "ps1" | "sql" | "vue" | "svelte" | "lua" => {
                Self::Code
            }
            "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "iso" | "cab" | "zst" => {
                Self::Archive
            }
            "exe" | "msi" | "com" | "lnk" | "appx" | "msix" => Self::Executable,
            "ttf" | "otf" | "woff" | "woff2" => Self::Font,
            _ => Self::Other,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Folder => tr("Carpeta de archivos", "File folder"),
            Self::Image => tr("Imagen", "Image"),
            Self::Video => tr("Vídeo", "Video"),
            Self::Audio => "Audio",
            Self::Document => tr("Documento", "Document"),
            Self::Code => tr("Código", "Code"),
            Self::Archive => tr("Archivo comprimido", "Archive"),
            Self::Executable => tr("Aplicación", "Application"),
            Self::Font => tr("Fuente", "Font"),
            Self::Other => tr("Archivo", "File"),
        }
    }

    pub fn badge(self, ext: &str) -> String {
        let value = match self {
            Self::Folder => "DIR".to_string(),
            Self::Image => "IMG".to_string(),
            Self::Video => "VID".to_string(),
            Self::Audio => "AUD".to_string(),
            Self::Document => "DOC".to_string(),
            Self::Code => "DEV".to_string(),
            Self::Archive => "ZIP".to_string(),
            Self::Executable => "EXE".to_string(),
            Self::Font => "FNT".to_string(),
            Self::Other => "FILE".to_string(),
        };
        if ext.is_empty() {
            value
        } else {
            let ext = ext.to_uppercase();
            // `len()` cuenta bytes: cortar por índice podría partir un carácter
            // UTF-8 (por ejemplo una extensión con emoji) y provocar un pánico.
            if ext.chars().count() <= 4 {
                ext
            } else {
                ext.chars().take(4).collect()
            }
        }
    }

    pub fn color(self) -> u32 {
        match self {
            Self::Folder => 0xf5b84b,
            Self::Image => 0x5bd6c8,
            Self::Video => 0xff7a66,
            Self::Audio => 0xb18cff,
            Self::Document => 0x6ea8fe,
            Self::Code => 0xb8e986,
            Self::Archive => 0xffa94d,
            Self::Executable => 0xff6b8a,
            Self::Font => 0xf28db2,
            Self::Other => 0x9aa7b8,
        }
    }

    pub fn is_previewable_text(self, path: &Path) -> bool {
        if self == Self::Document || self == Self::Code {
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            return matches!(
                ext.as_str(),
                "txt"
                    | "md"
                    | "rtf"
                    | "csv"
                    | "json"
                    | "yaml"
                    | "yml"
                    | "toml"
                    | "xml"
                    | "rs"
                    | "js"
                    | "ts"
                    | "tsx"
                    | "jsx"
                    | "py"
                    | "c"
                    | "cpp"
                    | "h"
                    | "hpp"
                    | "cs"
                    | "java"
                    | "go"
                    | "rb"
                    | "php"
                    | "html"
                    | "css"
                    | "scss"
                    | "sh"
                    | "bat"
                    | "ps1"
                    | "sql"
                    | "vue"
                    | "svelte"
                    | "lua"
                    | "ini"
                    | "cfg"
                    | "log"
            );
        }
        false
    }
}

#[derive(Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub folder_shortcut: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    pub kind: FileKind,
    pub hidden: bool,
    pub thumb: Option<Arc<RenderImage>>,
    pub icon: Option<Arc<RenderImage>>,
    /// Ruta original de un elemento de la Papelera. `None` en el resto.
    pub origin: Option<PathBuf>,
    /// Archivo `$I` que describe el elemento dentro de la Papelera.
    pub recycle_meta: Option<PathBuf>,
}

impl Entry {
    pub fn extension(&self) -> String {
        if self.is_dir {
            return String::new();
        }
        self.path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    }

    pub fn display_name(&self, show_extensions: bool) -> String {
        if show_extensions || self.is_dir {
            return self.name.clone();
        }
        let Some(ext) = self.path.extension() else {
            return self.name.clone();
        };
        let suffix = format!(".{}", ext.to_string_lossy());
        self.name
            .strip_suffix(&suffix)
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.name.clone())
    }

    pub fn size_label(&self) -> String {
        if self.is_dir {
            "—".to_string()
        } else {
            format_size(self.size)
        }
    }

    pub fn type_label(&self) -> String {
        if self.is_dir {
            return tr("Carpeta de archivos", "File folder").to_string();
        }
        let ext = self.extension();
        if ext.is_empty() {
            self.kind.label().to_string()
        } else {
            format!("{} · {}", ext.to_uppercase(), self.kind.label())
        }
    }

    pub fn modified_label(&self) -> String {
        self.modified
            .map(format_date)
            .unwrap_or_else(|| "—".to_string())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Kind,
    Modified,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => tr("Nombre", "Name"),
            Self::Size => tr("Tamaño", "Size"),
            Self::Kind => tr("Tipo", "Type"),
            Self::Modified => tr("Modificado", "Modified"),
        }
    }
}

pub struct ScanResult {
    pub entries: Vec<Entry>,
    pub error: Option<String>,
    pub truncated: bool,
}

pub fn scan(path: &Path, show_hidden: bool, recursive: bool, query: &str, ignore_accents: bool) -> ScanResult {
    if !path.is_dir() {
        return ScanResult {
            entries: Vec::new(),
            error: Some(tr("La carpeta no existe o no está disponible.", "The folder does not exist or is unavailable.").to_string()),
            truncated: false,
        };
    }

    let query = query.trim();
    let recursive = recursive && !query.is_empty();
    let mut entries = Vec::new();
    let mut error = None;
    let mut truncated = false;

    let query = search_text(query.trim(), ignore_accents);
    if recursive {
        let mut stack = vec![(path.to_path_buf(), 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            if entries.len() >= 4000 {
                truncated = true;
                break;
            }
            if depth > 12 {
                truncated = true;
                continue;
            }
            let Ok(read) = fs::read_dir(&dir) else {
                continue;
            };
            for item in read.flatten() {
                let Ok(meta) = item.metadata() else {
                    continue;
                };
                let entry = make_entry(item.path(), &meta);
                if entry.hidden && !show_hidden {
                    continue;
                }
                if entry.is_dir && !is_reparse(&meta) {
                    stack.push((entry.path.clone(), depth + 1));
                }
                if matches_query(&entry, &query, ignore_accents) {
                    entries.push(entry);
                }
                if entries.len() >= 4000 {
                    truncated = true;
                    break;
                }
            }
        }
    } else {
        match fs::read_dir(path) {
            Ok(read) => {
                for item in read.flatten() {
                    let Ok(meta) = item.metadata() else {
                        continue;
                    };
                    let entry = make_entry(item.path(), &meta);
                    if entry.hidden && !show_hidden {
                        continue;
                    }
                    if !query.is_empty() && !matches_query(&entry, &query, ignore_accents) {
                        continue;
                    }
                    entries.push(entry);
                }
            }
            Err(err) => error = Some(crate::localized!(format, "No se pudo leer la carpeta: {err}", "Could not read the folder: {err}")),
        }
    }

    sort_entries(&mut entries, SortKey::Name, false);
    ScanResult {
        entries,
        error,
        truncated,
    }
}

fn make_entry(path: PathBuf, meta: &fs::Metadata) -> Entry {
    let is_dir = meta.is_dir();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let hidden = is_hidden(meta, &name);
    let folder_shortcut = !is_dir && shortcut_targets_directory(&path);
    Entry {
        kind: FileKind::from_path(&path, is_dir),
        path,
        name,
        is_dir,
        folder_shortcut,
        size: meta.len(),
        modified: meta.modified().ok(),
        created: meta.created().ok(),
        accessed: meta.accessed().ok(),
        hidden,
        thumb: None,
        icon: None,
        origin: None,
        recycle_meta: None,
    }
}

// El encabezado de un .lnk incluye los atributos del destino. Leerlo evita
// resolver el acceso directo y tocar su destino, que puede estar desconectado.
pub(crate) fn shortcut_targets_directory(path: &Path) -> bool {
    if !path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("lnk")) {
        return false;
    }
    use std::io::Read;
    let mut header = [0; 28];
    fs::File::open(path).and_then(|mut file| file.read_exact(&mut header)).is_ok()
        && shortcut_header_has_directory_attribute(&header)
}

fn shortcut_header_has_directory_attribute(header: &[u8; 28]) -> bool {
    const LINK_HEADER: [u8; 20] = [
        0x4c, 0, 0, 0, 0x01, 0x14, 0x02, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46,
    ];
    header.starts_with(&LINK_HEADER) && header[24] & 0x10 != 0
}

pub fn is_hidden(meta: &fs::Metadata, name: &str) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x2 != 0 {
            return true;
        }
    }
    false
}

pub fn search_text(text: &str, ignore_accents: bool) -> String {
    let lowered = text.to_lowercase();
    if !ignore_accents || lowered.is_ascii() {
        return lowered;
    }
    lowered.chars().filter_map(|ch| match ch {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => Some('a'),
        'é' | 'è' | 'ê' | 'ë' => Some('e'),
        'í' | 'ì' | 'î' | 'ï' => Some('i'),
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' => Some('o'),
        'ú' | 'ù' | 'û' | 'ü' => Some('u'),
        '\u{0300}' | '\u{0301}' | '\u{0302}' | '\u{0308}' | '\u{030a}' => None,
        other => Some(other),
    }).collect()
}

pub fn matches_query(entry: &Entry, query: &str, ignore_accents: bool) -> bool {
    if query.is_empty() {
        return true;
    }
    let name = search_text(&entry.name, ignore_accents);
    for term in query.split_whitespace() {
        if let Some(ext) = term.strip_prefix("ext:") {
            if !ext.split(',').any(|e| {
                entry
                    .extension()
                    .eq_ignore_ascii_case(e.trim_start_matches('.'))
            }) {
                return false;
            }
        } else if let Some(kind) = term.strip_prefix("tipo:") {
            if !search_text(entry.kind.label(), ignore_accents).contains(kind) {
                return false;
            }
        } else if let Some(excluded) = term.strip_prefix('-') {
            if !excluded.is_empty() && name.contains(excluded) {
                return false;
            }
        } else if let Some(fuzzy) = term.strip_prefix('~') {
            if !subsequence(&name, fuzzy) {
                return false;
            }
        } else if !name.contains(term) {
            return false;
        }
    }
    true
}

fn subsequence(haystack: &str, needle: &str) -> bool {
    let mut chars = haystack.chars();
    for wanted in needle.chars() {
        loop {
            match chars.next() {
                Some(ch) if ch == wanted => break,
                Some(_) => continue,
                None => return false,
            }
        }
    }
    true
}

pub fn sort_entries(entries: &mut [Entry], key: SortKey, descending: bool) {
    entries.sort_by(|a, b| {
        if a.is_dir != b.is_dir {
            return b.is_dir.cmp(&a.is_dir);
        }
        let order = match key {
            SortKey::Name => natural_cmp(&a.name, &b.name),
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Kind => a.kind.label().cmp(b.kind.label()),
            SortKey::Modified => a.modified.cmp(&b.modified),
        };
        if descending { order.reverse() } else { order }
    });
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ac), Some(bc)) => {
                if ac.is_ascii_digit() && bc.is_ascii_digit() {
                    let mut an = String::new();
                    while let Some(c) = ai.peek().copied().filter(|c| c.is_ascii_digit()) {
                        an.push(c);
                        ai.next();
                    }
                    let mut bn = String::new();
                    while let Some(c) = bi.peek().copied().filter(|c| c.is_ascii_digit()) {
                        bn.push(c);
                        bi.next();
                    }
                    let av = an.trim_start_matches('0');
                    let bv = bn.trim_start_matches('0');
                    let ord = av.len().cmp(&bv.len()).then_with(|| av.cmp(bv));
                    if ord != Ordering::Equal {
                        return ord;
                    }
                } else {
                    ai.next();
                    bi.next();
                    let ord = ac.cmp(&bc);
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
            }
        }
    }
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        let text = format!("{value:.1} {}", UNITS[unit]);
        if crate::i18n::is_english() { text } else { text.replace('.', ",") }
    }
}

pub fn format_date(time: SystemTime) -> String {
    let date: DateTime<Local> = time.into();
    date.format(tr("%d/%m/%Y  %H:%M", "%Y-%m-%d  %H:%M")).to_string()
}

pub fn folder_summary(path: &Path, limit: usize) -> (usize, u64, Vec<String>) {
    let mut count = 0usize;
    let mut size = 0u64;
    let mut sample = Vec::new();
    if let Ok(read) = fs::read_dir(path) {
        for item in read.flatten() {
            let Ok(meta) = item.metadata() else {
                continue;
            };
            count += 1;
            if !meta.is_dir() {
                size = size.saturating_add(meta.len());
            }
            if sample.len() < limit {
                sample.push(item.file_name().to_string_lossy().into_owned());
            }
        }
    }
    (count, size, sample)
}

pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string());
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for index in 2..10000 {
        let candidate = dir.join(format!("{stem} ({index}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(crate::localized!(format, "{stem} (copia){ext}", "{stem} (copy){ext}"))
}

// ---------------------------------------------------------------------------
// Papelera de reciclaje
// ---------------------------------------------------------------------------

/// Ruta centinela con la que la Papelera aparece como una vista más dentro de
/// Xool Files. No es una carpeta real del disco.
pub const RECYCLE_BIN_PATH: &str = "shell:RecycleBinFolder";

pub fn is_recycle_bin(path: &Path) -> bool {
    path.to_string_lossy().eq_ignore_ascii_case(RECYCLE_BIN_PATH)
}

/// Un elemento real de la Papelera: `$R…` es el contenido y `$I…` sus datos.
#[derive(Clone, Debug)]
pub struct RecycleItem {
    pub recycled: PathBuf,
    pub meta: PathBuf,
    pub origin: PathBuf,
}

struct RecycleInfo {
    size: u64,
    deleted: Option<SystemTime>,
    origin: Option<PathBuf>,
}

/// Recorre todas las unidades y arma una lista con los elementos de la
/// Papelera. Se ejecuta en segundo plano, igual que el escaneo de carpetas.
pub fn scan_recycle_bin() -> ScanResult {
    let mut entries = Vec::new();
    let mut found_bin = false;

    for drive in logical_drives() {
        let bin = drive.join("$Recycle.Bin");
        let Ok(sids) = fs::read_dir(&bin) else {
            continue;
        };
        found_bin = true;
        for sid in sids.flatten() {
            let sid_path = sid.path();
            if !sid.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let Ok(items) = fs::read_dir(&sid_path) else {
                continue;
            };
            let mut metadata_files = Vec::new();
            let mut payloads = HashMap::new();
            for item in items.flatten() {
                let name = item.file_name().to_string_lossy().into_owned();
                if name.starts_with("$I") || name.starts_with("$i") {
                    metadata_files.push((name, item.path()));
                } else if name.starts_with("$R") || name.starts_with("$r") {
                    payloads.insert(name[2..].to_ascii_lowercase(), item);
                }
            }

            for (name, meta_path) in metadata_files {
                let Some(rest) = name
                    .strip_prefix("$I")
                    .or_else(|| name.strip_prefix("$i"))
                else {
                    continue;
                };
                let recycled_name = format!("$R{rest}");
                let Some(payload) = payloads.remove(&rest.to_ascii_lowercase()) else {
                    continue;
                };
                let Ok(file_type) = payload.file_type() else {
                    continue;
                };
                let recycled = payload.path();
                let bytes = fs::read(&meta_path).unwrap_or_default();
                let info = parse_recycle_info(&bytes);
                let metadata = if info.as_ref().is_some_and(|info| info.deleted.is_some()) {
                    None
                } else {
                    payload.metadata().ok()
                };
                let origin = info.as_ref().and_then(|info| info.origin.clone());
                let original_name = origin
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| recycled_name.clone());
                let is_dir = file_type.is_dir();
                let kind = origin
                    .as_ref()
                    .map(|path| FileKind::from_path(path, is_dir))
                    .unwrap_or_else(|| FileKind::from_path(&recycled, is_dir));
                let folder_shortcut = !is_dir && shortcut_targets_directory(&recycled);
                let entry = Entry {
                    path: recycled,
                    name: original_name,
                    is_dir,
                    folder_shortcut,
                    size: if is_dir {
                        0
                    } else {
                        info.as_ref()
                            .map(|info| info.size)
                            .or_else(|| metadata.as_ref().map(fs::Metadata::len))
                            .unwrap_or_default()
                    },
                    modified: info
                        .as_ref()
                        .and_then(|info| info.deleted)
                        .or_else(|| metadata.as_ref().and_then(|metadata| metadata.modified().ok())),
                    created: metadata.as_ref().and_then(|meta| meta.created().ok()),
                    accessed: metadata.as_ref().and_then(|meta| meta.accessed().ok()),
                    kind,
                    hidden: false,
                    thumb: None,
                    icon: None,
                    origin,
                    recycle_meta: Some(meta_path),
                };
                entries.push(entry);
            }
        }
    }

    ScanResult {
        entries,
        error: if found_bin {
            None
        } else {
            Some(tr("No se encontró una Papelera accesible en este equipo.", "No accessible Recycle Bin was found on this computer.").to_string())
        },
        truncated: false,
    }
}

fn logical_drives() -> Vec<PathBuf> {
    #[cfg(windows)]
    unsafe {
        let mask = GetLogicalDrives();
        let mut drives = Vec::new();
        for index in 0..26u32 {
            if mask & (1 << index) != 0 {
                let letter = (b'A' + index as u8) as char;
                drives.push(PathBuf::from(format!("{letter}:\\")));
            }
        }
        drives
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

fn parse_recycle_info(bytes: &[u8]) -> Option<RecycleInfo> {
    if bytes.len() < 24 {
        return None;
    }
    let version = u64::from_le_bytes(bytes.get(0..8)?.try_into().ok()?);
    let size = u64::from_le_bytes(bytes.get(8..16)?.try_into().ok()?);
    let filetime = u64::from_le_bytes(bytes.get(16..24)?.try_into().ok()?);
    let origin = if version >= 2 {
        let name_len = u32::from_le_bytes(bytes.get(24..28)?.try_into().ok()?) as usize;
        let end = 28usize.checked_add(name_len.checked_mul(2)?)?;
        decode_utf16(bytes.get(28..end)?)
    } else {
        // Formato antiguo: una ruta fija de 260 caracteres UTF-16.
        decode_utf16(bytes.get(24..).unwrap_or(&[]))
    };
    Some(RecycleInfo {
        size,
        deleted: filetime_to_system_time(filetime),
        origin,
    })
}

fn decode_utf16(bytes: &[u8]) -> Option<PathBuf> {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let text = String::from_utf16_lossy(&units);
    let text = text.split('\0').next().unwrap_or("").trim();
    if text.is_empty() {
        None
    } else {
        Some(PathBuf::from(text))
    }
}

fn filetime_to_system_time(value: u64) -> Option<SystemTime> {
    // FILETIME cuenta intervalos de 100 ns desde 1601-01-01.
    const EPOCH_DIFFERENCE: u64 = 116_444_736_000_000_000;
    let ticks = value.checked_sub(EPOCH_DIFFERENCE)?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_nanos(ticks.checked_mul(100)?))
}

/// Restaura los elementos a su ruta original. Si ya existe algo con ese
/// nombre, se usa un nombre libre para no sobrescribir nada.
pub fn restore_recycle_items(items: &[RecycleItem]) -> Result<usize, String> {
    let mut restored = 0usize;
    let mut errors = Vec::new();
    for item in items {
        let Some(parent) = item.origin.parent() else {
            errors.push(crate::localized!(format, "{}: ruta original no válida", "{}: invalid original path", item.origin.display()));
            continue;
        };
        if !parent.as_os_str().is_empty() && !parent.exists() {
            let _ = fs::create_dir_all(parent);
        }
        let destination = if item.origin.exists() {
            let name = item
                .origin
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| tr("elemento", "item").to_string());
            unique_path(parent, &name)
        } else {
            item.origin.clone()
        };
        match move_item(&item.recycled, &destination) {
            Ok(()) => {
                let _ = fs::remove_file(&item.meta);
                restored += 1;
            }
            Err(error) => errors.push(format!("{}: {error}", item.origin.display())),
        }
    }
    if errors.is_empty() {
        Ok(restored)
    } else {
        Err(crate::localized!(format, "Se restauraron {restored} de {} elemento(s). {}", "Restored {restored} of {} item(s). {}",
            items.len(),
            errors.join(" · ")
        ))
    }
}

/// Borra definitivamente los elementos seleccionados de la Papelera.
pub fn delete_recycle_items(items: &[RecycleItem]) -> (HashSet<PathBuf>, Option<String>) {
    let mut deleted = HashSet::new();
    let mut errors = Vec::new();
    for item in items {
        let result = fs::symlink_metadata(&item.recycled).and_then(|metadata| {
            if metadata.is_dir() {
                fs::remove_dir_all(&item.recycled)
            } else {
                fs::remove_file(&item.recycled)
            }
        });
        match result {
            Ok(()) => {
                deleted.insert(item.recycled.clone());
                if let Err(error) = fs::remove_file(&item.meta)
                    && error.kind() != io::ErrorKind::NotFound
                {
                    errors.push(crate::localized!(format, "No se retiraron los metadatos de {}: {error}", "Metadata for {} could not be removed: {error}", item.origin.display()));
                }
            }
            Err(error) => errors.push(format!("{}: {error}", item.origin.display())),
        }
    }
    let error = (!errors.is_empty()).then(|| crate::localized!(format, "Se eliminaron {} de {} elementos. {}", "Deleted {} of {} items. {}",
        deleted.len(), items.len(), errors.join(" · ")
    ));
    (deleted, error)
}

fn move_item(from: &Path, to: &Path) -> io::Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    if from.is_dir() {
        copy_dir(from, to)?;
        fs::remove_dir_all(from)
    } else {
        fs::copy(from, to)?;
        fs::remove_file(from)
    }
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

pub fn read_text(path: &Path, limit: usize) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    let truncated = bytes.len() > limit;
    let slice = &bytes[..bytes.len().min(limit)];
    if slice.contains(&0) {
        return Err(tr("Este archivo contiene datos binarios.", "This file contains binary data.").to_string());
    }
    let text = if slice.starts_with(&[0xff, 0xfe]) {
        let utf16: Vec<u16> = slice[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&utf16)
    } else {
        String::from_utf8_lossy(slice).into_owned()
    };
    if truncated {
        Ok(crate::localized!(format, "{text}\n\n— Vista previa recortada —", "{text}\n\n— Preview truncated —"))
    } else {
        Ok(text)
    }
}

pub fn normalize_path(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    #[cfg(windows)]
    {
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return rest.to_string();
        }
    }
    text
}

fn is_reparse(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}
#[cfg(test)]
mod tests {

    #[test]
    fn folder_shortcut_requires_link_signature_and_directory_attribute() {
        let mut header = [0; 28];
        header[..20].copy_from_slice(&[
            0x4c, 0, 0, 0, 0x01, 0x14, 0x02, 0, 0, 0, 0, 0, 0xc0, 0, 0, 0, 0, 0, 0, 0x46,
        ]);
        assert!(!super::shortcut_header_has_directory_attribute(&header));
        header[24] = 0x10;
        assert!(super::shortcut_header_has_directory_attribute(&header));
        header[4] = 0;
        assert!(!super::shortcut_header_has_directory_attribute(&header));
    }

    #[test]
    fn permanent_delete_preserves_metadata_for_failed_items() {
        let root = std::env::temp_dir().join(format!("xool-delete-test-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let deleted = super::RecycleItem {
            recycled: root.join("$R-moved.txt"),
            meta: root.join("$I-moved.txt"),
            origin: root.join("moved.txt"),
        };
        let missing = super::RecycleItem {
            recycled: root.join("$R-missing.txt"),
            meta: root.join("$I-missing.txt"),
            origin: root.join("missing.txt"),
        };
        std::fs::write(&deleted.recycled, b"data").unwrap();
        std::fs::write(&deleted.meta, b"metadata").unwrap();
        std::fs::write(&missing.meta, b"metadata").unwrap();
        let result = super::delete_recycle_items(&[deleted.clone(), missing.clone()]);
        let state = (deleted.recycled.exists(), deleted.meta.exists(), missing.meta.exists());
        std::fs::remove_file(&missing.meta).unwrap();
        std::fs::remove_dir(root).unwrap();
        assert!(result.0.contains(&deleted.recycled));
        assert!(result.1.is_some());
        assert_eq!(state, (false, false, true));
    }

    use super::*;
    #[test]
    fn accent_preference_applies_to_recursive_folder_and_global_search() {
        let root = std::env::current_dir().unwrap().join("target")
            .join(format!("accent-search-test-{}", std::process::id()));
        let sub = root.join("sub");
        fs::create_dir_all(&sub).unwrap();
        let song = sub.join("Música.txt");
        fs::write(&song, "test").unwrap();

        let entry = make_entry(song.clone(), &fs::metadata(&song).unwrap());
        assert!(!matches_query(&entry, "musica", false));
        assert!(matches_query(&entry, "musica ext:txt", true));
        assert_eq!(scan(&root, false, true, "musica", false).entries.len(), 0);
        assert_eq!(scan(&root, false, true, "musica", true).entries.len(), 1);
        let generation = std::sync::atomic::AtomicU64::new(1);
        assert!(crate::integration::quick_matches(&root, "musica", false, &generation, 1).is_empty());
        assert_eq!(crate::integration::quick_matches(&root, "musica", true, &generation, 1), vec![song]);
        assert_eq!(search_text("Mu\u{301}sica", true), "musica");
        assert_eq!(search_text("Año", true), "año");

        fs::remove_file(sub.join("Música.txt")).unwrap();
        fs::remove_dir(sub).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn recursive_with_hidden_and_combined_filters() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "search-test-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub\\Informe.PDF"), "test").unwrap();
        fs::write(root.join("sub\\Informe-borrador.txt"), "test").unwrap();
        assert_eq!(
            scan(&root, true, true, "informe ext:pdf,txt -borrador", false)
                .entries
                .len(),
            1
        );
        assert_eq!(scan(&root, false, true, "~ifme", false).entries.len(), 2);
        assert_eq!(scan(&root, true, false, "informe", false).entries.len(), 0);
        fs::remove_dir_all(root).unwrap();
    }
}
