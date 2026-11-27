use std::{
    fs::File,
    io::{BufReader, ErrorKind},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits, RgbImage,
    codecs::{jpeg::JpegEncoder, png::PngDecoder, webp::WebPDecoder}, imageops::FilterType};

#[derive(Clone, Copy)]
pub(crate) enum Edit {
    Convert(ImageFormat),
    Resize(u32, u32),
    FlipHorizontal,
    FlipVertical,
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(24_000);
    limits.max_image_height = Some(24_000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits
}

pub(crate) fn format_for(path: &Path) -> Option<ImageFormat> {
    ImageFormat::from_path(path).ok().filter(|format| {
        matches!(format, ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP)
    })
}

pub(crate) fn resize_allowed(original_width: u32, width: u32, height: u32) -> bool {
    (1..=24_000).contains(&width) && (1..=24_000).contains(&height)
        && u64::from(width) * u64::from(height) <= 24_000_000
        && u64::from(original_width) * u64::from(height) <= 16_000_000
}

#[derive(Clone, Debug)]
pub struct ResizeControls {
    pub original: [u32; 2],
    pub dimensions: [u32; 2],
    pub locked: bool,
    pub drag: Option<(usize, f32, u32)>,
}

impl ResizeControls {
    pub(crate) fn new(original: [u32; 2]) -> Self {
        Self { original, dimensions: original, locked: true, drag: None }
    }

    pub(crate) fn set_dimension(&mut self, axis: usize, requested: i32) {
        let other = 1 - axis;
        let maximum = if self.locked {
            (24_000. * f64::from(self.original[axis]) / f64::from(self.original[other]))
                .floor().clamp(1., 24_000.) as i32
        } else { 24_000 };
        self.dimensions[axis] = requested.clamp(1, maximum) as u32;
        if self.locked {
            let ratio = f64::from(self.original[other]) / f64::from(self.original[axis]);
            self.dimensions[other] = (f64::from(self.dimensions[axis]) * ratio).round().clamp(1., 24_000.) as u32;
        }
    }
}

fn open_decoder(path: &Path) -> Result<(impl ImageDecoder, ImageFormat)> {
    let mut reader = ImageReader::open(path)?.with_guessed_format()?;
    let format = reader.format().context("Unknown image format")?;
    match format {
        ImageFormat::Png => {
            let decoder = PngDecoder::with_limits(BufReader::new(File::open(path)?), limits())?;
            if decoder.is_apng()? { bail!("Animated PNG images are unsupported"); }
        }
        ImageFormat::WebP => {
            let decoder = WebPDecoder::new(BufReader::new(File::open(path)?))?;
            if decoder.has_animation() { bail!("Animated WebP images are unsupported"); }
        }
        ImageFormat::Jpeg => {}
        _ => bail!("Unsupported image format"),
    }
    let mut limits = limits();
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder()?;
    limits.reserve(decoder.total_bytes())?;
    decoder.set_limits(limits)?;
    Ok((decoder, format))
}

pub(crate) fn oriented_dimensions(path: &Path) -> Result<[u32; 2]> {
    use image::metadata::Orientation::*;
    let (mut decoder, _) = open_decoder(path)?;
    let (width, height) = decoder.dimensions();
    let orientation = decoder.orientation()?;
    if width == 0 || height == 0 { bail!("Empty image"); }
    Ok(match orientation {
        Rotate90 | Rotate270 | Rotate90FlipH | Rotate270FlipH => [height, width],
        _ => [width, height],
    })
}

fn load(path: &Path) -> Result<(DynamicImage, ImageFormat)> {
    let (mut decoder, format) = open_decoder(path)?;
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok((image, format))
}

fn jpeg_pixels(image: DynamicImage) -> RgbImage {
    let image = image.into_rgba8();
    RgbImage::from_fn(image.width(), image.height(), |x, y| {
        let [r, g, b, alpha] = image.get_pixel(x, y).0;
        let alpha = u16::from(alpha);
        image::Rgb([r, g, b].map(|channel| {
            ((u16::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8
        }))
    })
}

pub(crate) fn save_copy(path: &Path, edit: Edit) -> Result<PathBuf> {
    let (image, source_format) = load(path)?;
    let (image, format, suffix) = match edit {
        Edit::Convert(format) => (image, format, ""),
        Edit::Resize(w, h) => {
            // Lanczos uses a floating-point vertical pass at the original width.
            if !resize_allowed(image.width(), w, h) {
                bail!("Resize exceeds the image processing memory limit");
            }
            (image.resize_exact(w, h, FilterType::Lanczos3), source_format, " - resized")
        }
        Edit::FlipHorizontal => (image.fliph(), source_format, " - flip H"),
        Edit::FlipVertical => (image.flipv(), source_format, " - flip V"),
    };
    let extension = match format {
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Png => "png",
        ImageFormat::WebP => "webp",
        _ => bail!("Unsupported output format"),
    };
    let dir = path.parent().context("Image has no parent directory")?;
    let stem = path.file_stem().context("Image has no name")?.to_string_lossy();
    let name = format!("{stem}{suffix}.{extension}");
    let mut output = tempfile::Builder::new().prefix(".xool-image-").tempfile_in(dir)?;
    match format {
        ImageFormat::Jpeg => JpegEncoder::new_with_quality(output.as_file_mut(), 90)
            .encode_image(&jpeg_pixels(image))?,
        ImageFormat::WebP => DynamicImage::ImageRgba8(image.into_rgba8()).write_to(output.as_file_mut(), format)?,
        _ => image.write_to(output.as_file_mut(), format)?,
    }
    output.as_file().sync_all()?;
    for _ in 0..32 {
        let destination = crate::fs_model::unique_path(dir, &name);
        match output.persist_noclobber(&destination) {
            Ok(_) => return Ok(destination),
            Err(error) if error.error.kind() == ErrorKind::AlreadyExists => output = error.file,
            Err(error) => return Err(error.error.into()),
        }
    }
    bail!("Could not reserve a name for the image copy")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    #[test]
    fn resize_updates_the_other_axis_only_when_locked() {
        let mut state = ResizeControls::new([1920, 1080]);
        state.set_dimension(0, 960);
        assert_eq!(state.dimensions, [960, 540]);
        state.set_dimension(1, 720);
        assert_eq!(state.dimensions, [1280, 720]);
        state.locked = false;
        state.set_dimension(1, 600);
        assert_eq!(state.dimensions, [1280, 600]);
    }

    #[test]
    fn conversions_and_transforms_write_copies() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("sample.png");
        let image = RgbaImage::from_fn(2, 2, |x, y| Rgba([x as u8 * 120, y as u8 * 120, 0, 255]));
        image.save(&path)?;
        let original = std::fs::read(&path)?;
        let horizontal = image::open(save_copy(&path, Edit::FlipHorizontal)?)?.into_rgba8();
        let vertical = image::open(save_copy(&path, Edit::FlipVertical)?)?.into_rgba8();
        assert_eq!(horizontal.get_pixel(0, 0), image.get_pixel(1, 0));
        assert_eq!(vertical.get_pixel(0, 0), image.get_pixel(0, 1));
        let jpeg = save_copy(&path, Edit::Convert(ImageFormat::Jpeg))?;
        assert_eq!(image::open(&jpeg)?.width(), 2);
        assert_ne!(save_copy(&jpeg, Edit::Convert(ImageFormat::Png))?, path);
        let webp = save_copy(&path, Edit::Convert(ImageFormat::WebP))?;
        assert_eq!(image::open(webp)?.into_rgba8(), image);
        assert_eq!(image::image_dimensions(save_copy(&path, Edit::Resize(6, 4))?)?, (6, 4));
        assert_eq!(std::fs::read(path)?, original);
        Ok(())
    }
}
