//! Carga de miniaturas y contenidos de vista previa.
//! Todo el trabajo pesado se ejecuta fuera del hilo de la interfaz.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{BufWriter, Read, Write};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use gpui::RenderImage;
use image::ImageDecoder;

use crate::fs_model::{self, FileKind};
use crate::platform;

pub enum PreviewContent {
    Image(Arc<RenderImage>),
    Text(String),
    Folder {
        count: usize,
        size: u64,
        names: Vec<String>,
    },
    Media {
        thumb: Option<Arc<RenderImage>>,
        kind: FileKind,
    },
    Message(String),
}

// ---------------------------------------------------------------------------
// Caché de miniaturas
//
// Las miniaturas se guardan en memoria y en disco para que volver a una
// carpeta (o reabrir Xool Files) no obligue a decodificar otra vez las imágenes.
// La clave incluye ruta, tamaño, fecha de modificación y tamaño solicitado,
// así que un archivo editado se vuelve a generar automáticamente.
// ---------------------------------------------------------------------------

const CACHE_MAGIC: &[u8; 4] = b"ZTH1";
const MEMORY_MAX_BYTES: usize = 8 * 1024 * 1024;
const MEMORY_MAX_ENTRIES: usize = 128;
const DISK_MAX_BYTES: u64 = 300 * 1024 * 1024;
const DISK_TARGET_BYTES: u64 = 240 * 1024 * 1024;

struct MemoryCache {
    map: HashMap<u64, (Arc<RenderImage>, usize)>,
    order: VecDeque<u64>,
    bytes: usize,
}

impl MemoryCache {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
        }
    }
}

fn memory_cache() -> &'static Mutex<MemoryCache> {
    static CACHE: OnceLock<Mutex<MemoryCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(MemoryCache::new()))
}

fn cache_write_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn cache_key(path: &Path, size: u32, tag: &str) -> Option<u64> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.to_string_lossy().to_lowercase().hash(&mut hasher);
    if !metadata.is_dir() {
        metadata.len().hash(&mut hasher);
    }
    modified.hash(&mut hasher);
    size.hash(&mut hasher);
    tag.hash(&mut hasher);
    Some(hasher.finish())
}

fn cache_file(key: u64) -> Option<PathBuf> {
    Some(
        crate::settings::cache_dir()?
            .join(format!("thumb-{key:016x}.bin")),
    )
}

fn memory_get(key: u64) -> Option<Arc<RenderImage>> {
    let mut cache = memory_cache().lock().ok()?;
    let image = cache.map.get(&key)?.0.clone();
    if let Some(position) = cache.order.iter().position(|item| *item == key) {
        cache.order.remove(position);
    }
    cache.order.push_back(key);
    Some(image)
}

fn memory_put(key: u64, image: &Arc<RenderImage>) {
    let dimensions = image.size(0);
    let bytes = dimensions.width.0.max(0) as usize
        * dimensions.height.0.max(0) as usize
        * 4;
    let Ok(mut cache) = memory_cache().lock() else {
        return;
    };
    if let Some((_, old_bytes)) = cache.map.remove(&key) {
        cache.bytes = cache.bytes.saturating_sub(old_bytes);
    }
    if let Some(position) = cache.order.iter().position(|item| *item == key) {
        cache.order.remove(position);
    }
    cache.map.insert(key, (image.clone(), bytes));
    cache.order.push_back(key);
    cache.bytes += bytes;
    while cache.bytes > MEMORY_MAX_BYTES || cache.map.len() > MEMORY_MAX_ENTRIES {
        let Some(old_key) = cache.order.pop_front() else {
            break;
        };
        if let Some((_, old_bytes)) = cache.map.remove(&old_key) {
            cache.bytes = cache.bytes.saturating_sub(old_bytes);
        }
    }
}

fn disk_get(key: u64) -> Option<Arc<RenderImage>> {
    let path = cache_file(key)?;
    let mut file = fs::File::open(path).ok()?;
    let mut header = [0u8; 12];
    file.read_exact(&mut header).ok()?;
    if &header[..4] != CACHE_MAGIC {
        return None;
    }
    let width = u32::from_le_bytes(header[4..8].try_into().ok()?);
    let height = u32::from_le_bytes(header[8..12].try_into().ok()?);
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return None;
    }
    let expected = width as usize * height as usize * 4;
    if file.metadata().ok()?.len() != (12 + expected) as u64 {
        return None;
    }
    let mut pixels = vec![0; expected];
    file.read_exact(&mut pixels).ok()?;
    let buffer = image::RgbaImage::from_raw(width, height, pixels)?;
    Some(Arc::new(RenderImage::new(smallvec::smallvec![
        image::Frame::new(buffer)
    ])))
}

fn disk_put(key: u64, image: &Arc<RenderImage>) {
    let Some(path) = cache_file(key) else {
        return;
    };
    if path.exists() {
        return;
    }
    let Some(bytes) = image.as_bytes(0) else {
        return;
    };
    let dimensions = image.size(0);
    let width = dimensions.width.0.max(0) as u32;
    let height = dimensions.height.0.max(0) as u32;
    if width == 0 || height == 0 {
        return;
    }
    let Some(directory) = path.parent() else {
        return;
    };
    let _guard = cache_write_lock().lock().ok();
    let _ = fs::create_dir_all(directory);
    let temporary = path.with_extension("tmp");
    let written = (|| -> std::io::Result<()> {
        let mut file = BufWriter::new(fs::File::create(&temporary)?);
        file.write_all(CACHE_MAGIC)?;
        file.write_all(&width.to_le_bytes())?;
        file.write_all(&height.to_le_bytes())?;
        file.write_all(bytes)?;
        file.flush()
    })();
    if written.is_ok() {
        let _ = fs::remove_file(&path);
        let _ = fs::rename(&temporary, &path);
        note_cache_write();
    }
}

fn cached(key: u64) -> Option<Arc<RenderImage>> {
    if let Some(image) = memory_get(key) {
        return Some(image);
    }
    let image = disk_get(key)?;
    memory_put(key, &image);
    Some(image)
}

fn cache_store(key: u64, image: &Arc<RenderImage>) {
    memory_put(key, image);
    disk_put(key, image);
}

fn note_cache_write() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static WRITES: AtomicUsize = AtomicUsize::new(0);
    let writes = WRITES.fetch_add(1, Ordering::Relaxed) + 1;
    if writes % 64 == 0 {
        prune_disk_cache();
    }
}

fn prune_disk_cache() {
    let Some(directory) = crate::settings::cache_dir() else {
        return;
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    let mut total = 0u64;
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        total += metadata.len();
        files.push((
            entry.path(),
            metadata.len(),
            metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
        ));
    }
    if total <= DISK_MAX_BYTES {
        return;
    }
    files.sort_by_key(|(_, _, modified)| *modified);
    for (path, length, _) in files {
        if total <= DISK_TARGET_BYTES {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(length);
        }
    }
}

/// Tamaño y número de archivos de la caché de miniaturas.
pub fn cache_stats() -> (u64, usize) {
    let Some(directory) = crate::settings::cache_dir() else {
        return (0, 0);
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return (0, 0);
    };
    let mut size = 0u64;
    let mut files = 0usize;
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_file() {
            size += metadata.len();
            files += 1;
        }
    }
    (size, files)
}

/// Vacía la caché en memoria y en disco.
pub fn clear_cache() -> Result<(), String> {
    if let Ok(mut cache) = memory_cache().lock() {
        cache.map.clear();
        cache.order.clear();
        cache.bytes = 0;
    }
    let Some(directory) = crate::settings::cache_dir() else {
        return Ok(());
    };
    let Ok(entries) = fs::read_dir(&directory) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let result = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        if let Err(error) = result {
            return Err(crate::localized!(format, "No se pudo borrar {}: {error}", "Could not delete {}: {error}", path.display()));
        }
    }
    Ok(())
}

pub fn thumbnail(path: &Path, kind: FileKind, size: u32) -> Option<Arc<RenderImage>> {
    let tag = match kind {
        FileKind::Folder => "dir",
        FileKind::Image => "img-exif",
        FileKind::Video => "vid",
        FileKind::Document => "doc",
        FileKind::Archive => "zip",
        _ => return None,
    };
    let key = cache_key(path, size, tag);
    if let Some(key) = key {
        if let Some(image) = cached(key) {
            return Some(image);
        }
    }
    let image = match kind {
        FileKind::Folder => platform::shell_thumbnail(path, size),
        FileKind::Image => image_thumb(path, size).or_else(|| platform::shell_thumbnail(path, size)),
        FileKind::Video => platform::shell_thumbnail(path, size),
        FileKind::Document => {
            let ext = path
                .extension()
                .map(|ext| ext.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if matches!(ext.as_str(), "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx") {
                platform::shell_thumbnail(path, size)
            } else {
                None
            }
        }
        FileKind::Archive => platform::shell_thumbnail(path, size),
        _ => None,
    };
    if let (Some(key), Some(image)) = (key, image.as_ref()) {
        cache_store(key, image);
    }
    image
}

/// Devuelve una miniatura de contenido o el icono de Windows.
pub fn entry_visual(
    path: &Path,
    is_dir: bool,
    kind: FileKind,
    size: u32,
) -> (Option<Arc<RenderImage>>, Option<Arc<RenderImage>>) {
    let thumb = thumbnail(path, kind, size);
    if thumb.is_some() {
        return (thumb, None);
    }
    (thumb, platform::shell_icon(path, is_dir, size))
}

pub fn image_thumb(path: &Path, max: u32) -> Option<Arc<RenderImage>> {
    decode_image(path, max, 0.)
}

pub(crate) fn background_image(path: &Path, blur: u8) -> Option<Arc<RenderImage>> {
    decode_image(path, 1280, f32::from(blur))
}

fn decode_image(path: &Path, max: u32, blur: f32) -> Option<Arc<RenderImage>> {
    // Rapid navigation can overlap preview and thumbnail jobs. Bound decoding
    // memory even while old jobs finish, especially for large TIFF/PNG images.
    static DECODER: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _decoder = DECODER.lock().ok()?;
    let mut reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(24_000);
    limits.max_image_height = Some(24_000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder().ok()?;
    limits.reserve(decoder.total_bytes()).ok()?;
    decoder.set_limits(limits).ok()?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let decoded = image::DynamicImage::from_decoder(decoder).ok()?;
    // `resize` con Lanczos3 conserva la proporción y mantiene nitidez; solo
    // se reduce cuando la imagen supera el tamaño pedido.
    let limit = max.max(32);
    let mut resized = if decoded.width() > limit || decoded.height() > limit {
        decoded.resize(limit, limit, image::imageops::FilterType::Lanczos3)
    } else {
        decoded
    };
    resized.apply_orientation(orientation);
    let thumb = resized.into_rgba8();
    let thumb = if blur > 0. { image::imageops::blur(&thumb, blur) } else { thumb };
    let (width, height) = thumb.dimensions();
    let mut pixels = thumb.into_raw();
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width, height, pixels)?;
    Some(Arc::new(RenderImage::new(smallvec::smallvec![
        image::Frame::new(buffer)
    ])))
}

pub fn preview(path: &Path, is_dir: bool, kind: FileKind, max: u32) -> PreviewContent {
    if is_dir {
        let (count, size, names) = fs_model::folder_summary(path, 12);
        return PreviewContent::Folder {
            count,
            size,
            names,
        };
    }

    if kind == FileKind::Image {
        if let Some(image) = thumbnail(path, FileKind::Image, max) {
            return PreviewContent::Image(image);
        }
    }

    if kind.is_previewable_text(path) {
        match fs_model::read_text(path, 64 * 1024) {
            Ok(text) => return PreviewContent::Text(text),
            Err(err) => return PreviewContent::Message(err),
        }
    }

    if matches!(
        kind,
        FileKind::Video | FileKind::Audio | FileKind::Document | FileKind::Archive
    ) {
        let thumb = platform::shell_thumbnail(path, max);
        return PreviewContent::Media { thumb, kind };
    }

    PreviewContent::Media { thumb: None, kind }
}

#[cfg(test)]
mod orientation_tests {
    use super::*;
    use image::{ExtendedColorType, ImageEncoder};

    #[test]
    fn thumbnail_applies_jpeg_orientation_in_both_directions() {
        let path = std::env::temp_dir().join(format!("xool-exif-{}.jpg", std::process::id()));
        for (orientation, red_corner) in [(6, 9usize), (8, 29usize * 10)] {
            let pixels: Vec<u8> = (0..300).flat_map(|index| {
                if index % 30 < 15 { [240, 0, 0] } else { [0, 0, 240] }
            }).collect();
            let file = fs::File::create(&path).unwrap();
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(file, 98);
            // Little-endian TIFF header, followed by one EXIF orientation entry.
            encoder.set_exif_metadata(vec![
                b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 1, 3, 0, 1, 0,
                0, 0, orientation, 0, 0, 0, 0, 0, 0, 0,
            ]).unwrap();
            encoder.write_image(&pixels, 30, 10, ExtendedColorType::Rgb8).unwrap();
            let thumbnail = image_thumb(&path, 32).unwrap();
            assert_eq!((thumbnail.size(0).width.0, thumbnail.size(0).height.0), (10, 30));
            let bytes = thumbnail.as_bytes(0).unwrap();
            assert!(bytes[4 * red_corner + 2] > 200);
        }
        let _ = fs::remove_file(path);
    }
}
