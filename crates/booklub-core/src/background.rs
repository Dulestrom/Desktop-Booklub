//! Book backgrounds (Section 3.11): validation, the private copy, and thumbnails.

use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};

use crate::model::new_id;

/// REQ-35
pub const MAX_BYTES: u64 = 20 * 1024 * 1024;
/// REQ-35
pub const MAX_MEGAPIXELS: u64 = 100;
/// REQ-35q: below this a tile looks blurry on a high-resolution screen.
pub const SMALL_WIDTH: u32 = 296;
pub const SMALL_HEIGHT: u32 = 444;
/// Thumbnails are stored at twice the tile size. M0 measured 480 x 720 as too heavy to cache.
pub const THUMB_WIDTH: u32 = 296;
pub const THUMB_HEIGHT: u32 = 444;

/// Category tiles are 216 x 122, so their thumbnails and small-image warning use twice that.
pub const WIDE_WIDTH: u32 = 432;
pub const WIDE_HEIGHT: u32 = 244;

/// REQ-45: the built-in artworks, as (ID, name). Files are `assets/category-art/<id>.jpg`.
pub const PRESETS: [(&str, &str); 8] = [
    ("dusk-dunes", "Dusk dunes"),
    ("teal-geometry", "Teal geometry"),
    ("violet-bloom", "Violet bloom"),
    ("very-bright", "Bright sky"),
    ("very-busy", "Paper cuts"),
    ("forest-mist", "Forest mist"),
    ("ink-wash", "Ink wash"),
    ("ochre-fields", "Ochre fields"),
];

pub fn is_preset(id: &str) -> bool {
    PRESETS.iter().any(|(preset, _)| *preset == id)
}

pub const MSG_TOO_LARGE: &str = "That image is larger than 20 MB. Choose a smaller one.";
pub const MSG_TOO_MANY_PIXELS: &str = "That image is larger than 100 megapixels. Choose a smaller one.";
pub const MSG_WRONG_TYPE: &str = "That file is not a PNG, JPEG, or WebP image.";
pub const MSG_NO_CLIPBOARD_IMAGE: &str = "The clipboard has no image. In your browser, right-click the picture and choose Copy Image.";
pub const MSG_SMALL_IMAGE: &str = "This image is small and may look blurry on the tile.";

/// The clipboard types REQ-35k accepts, in the order Appendix A requests them.
pub const CLIPBOARD_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ImageError {
    #[error("{MSG_TOO_LARGE}")]
    TooLarge,
    #[error("{MSG_TOO_MANY_PIXELS}")]
    TooManyPixels,
    #[error("{MSG_WRONG_TYPE}")]
    WrongType,
    #[error("That image could not be read.")]
    Unreadable,
    #[error("That image could not be saved. Your existing data has not been changed.")]
    Unsaved,
    #[error("{MSG_NO_CLIPBOARD_IMAGE}")]
    NoClipboardImage,
}

/// A checked image, ready to preview and, on Save, to store (REQ-35r).
#[derive(Debug, Clone)]
pub struct CheckedImage {
    pub bytes: Vec<u8>,
    pub extension: &'static str,
    /// True when REQ-35q's warning applies.
    pub small: bool,
    pub width: u32,
    pub height: u32,
}

/// QA-37: checks the size and the header before any pixels are decoded.
pub fn check(bytes: Vec<u8>) -> Result<CheckedImage, ImageError> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(ImageError::TooLarge);
    }
    let format = image::guess_format(&bytes).map_err(|_| ImageError::WrongType)?;
    let extension = match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::WebP => "webp",
        _ => return Err(ImageError::WrongType),
    };
    // The library's own limits would refuse a huge header as a generic failure. Only the
    // header is read here, so lifting them allocates nothing and lets the Reader be told why.
    let mut reader = ImageReader::with_format(Cursor::new(&bytes), format);
    reader.no_limits();
    let (width, height) = reader.into_dimensions().map_err(|_| ImageError::Unreadable)?;
    if u64::from(width) * u64::from(height) > MAX_MEGAPIXELS * 1_000_000 {
        return Err(ImageError::TooManyPixels);
    }
    let small = width < SMALL_WIDTH || height < SMALL_HEIGHT;
    Ok(CheckedImage { bytes, extension, small, width, height })
}

/// Reads a file with the limit enforced before the read, then checks it (REQ-35).
pub fn check_file(path: &Path) -> Result<CheckedImage, ImageError> {
    let size = fs::metadata(path).map_err(|_| ImageError::Unreadable)?.len();
    if size > MAX_BYTES {
        return Err(ImageError::TooLarge);
    }
    check(fs::read(path).map_err(|_| ImageError::Unreadable)?)
}

/// Decodes with the REQ-35 pixel limit and applies EXIF orientation (REQ-35i).
pub fn decode(bytes: &[u8]) -> Result<DynamicImage, ImageError> {
    let format = image::guess_format(bytes).map_err(|_| ImageError::WrongType)?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_MEGAPIXELS * 1_000_000 * 4);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|_| ImageError::Unreadable)?;
    let orientation = decoder.orientation().map_err(|_| ImageError::Unreadable)?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|_| ImageError::Unreadable)?;
    image.apply_orientation(orientation);
    Ok(image)
}

/// Pixels ready for a texture, with no toolkit types, so a worker thread can make them.
#[derive(Debug, Clone)]
pub struct DecodedRgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Decodes for display, scaled to fit `max_width` by `max_height` so a 100 megapixel source
/// cannot become a 100 megapixel texture.
pub fn decode_for_display(bytes: &[u8], max_width: u32, max_height: u32) -> Result<DecodedRgba, ImageError> {
    let image = decode(bytes)?;
    let image = if image.width() > max_width || image.height() > max_height {
        image.resize(max_width, max_height, FilterType::Triangle)
    } else {
        image
    };
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(DecodedRgba { width, height, pixels: rgba.into_raw() })
}

/// REQ-35c: scales to cover, cropping around the centre.
pub fn make_thumbnail(image: &DynamicImage) -> DynamicImage {
    image.resize_to_fill(THUMB_WIDTH, THUMB_HEIGHT, FilterType::Triangle)
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("part");
    let mut file = fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)
}

/// REQ-45g: the landscape crop a Category tile uses. A separate file, because the book thumbnail
/// is already cropped to 2:3 and has thrown away most of a wide image.
pub fn wide_thumbnail_path(thumbnails: &Path, file_name: &str) -> PathBuf {
    let stem = Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or(file_name);
    thumbnails.join(format!("{stem}-wide.jpg"))
}

fn write_wide_thumbnail(image: &DynamicImage, path: &Path) -> Result<(), ImageError> {
    let mut encoded = Vec::new();
    image
        .resize_to_fill(WIDE_WIDTH, WIDE_HEIGHT, FilterType::Triangle)
        .to_rgb8()
        .write_to(&mut Cursor::new(&mut encoded), ImageFormat::Jpeg)
        .map_err(|_| ImageError::Unsaved)?;
    write_atomically(path, &encoded).map_err(|_| ImageError::Unsaved)
}

/// REQ-45a: stores a Category's private copy and landscape thumbnail, returning the file name.
pub fn store_for_category(image: &CheckedImage, backgrounds: &Path, thumbnails: &Path) -> Result<String, ImageError> {
    let file_name = format!("{}.{}", new_id(), image.extension);
    let decoded = decode(&image.bytes)?;
    fs::create_dir_all(backgrounds).map_err(|_| ImageError::Unsaved)?;
    fs::create_dir_all(thumbnails).map_err(|_| ImageError::Unsaved)?;
    write_atomically(&backgrounds.join(&file_name), &image.bytes).map_err(|_| ImageError::Unsaved)?;
    write_wide_thumbnail(&decoded, &wide_thumbnail_path(thumbnails, &file_name))?;
    Ok(file_name)
}

/// A cache like the book thumbnail, rebuilt from the private copy when absent.
pub fn ensure_wide_thumbnail(file_name: &str, backgrounds: &Path, thumbnails: &Path) -> Result<PathBuf, ImageError> {
    let path = wide_thumbnail_path(thumbnails, file_name);
    if path.exists() {
        return Ok(path);
    }
    let bytes = fs::read(backgrounds.join(file_name)).map_err(|_| ImageError::Unreadable)?;
    fs::create_dir_all(thumbnails).map_err(|_| ImageError::Unsaved)?;
    write_wide_thumbnail(&decode(&bytes)?, &path)?;
    Ok(path)
}

/// REQ-45h
pub fn is_small_for_category(image: &CheckedImage) -> bool {
    image.width < WIDE_WIDTH || image.height < WIDE_HEIGHT
}

pub fn thumbnail_path(thumbnails: &Path, file_name: &str) -> PathBuf {
    let stem = Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or(file_name);
    thumbnails.join(format!("{stem}.jpg"))
}

fn write_thumbnail(image: &DynamicImage, path: &Path) -> Result<(), ImageError> {
    let mut encoded = Vec::new();
    make_thumbnail(image)
        .to_rgb8()
        .write_to(&mut Cursor::new(&mut encoded), ImageFormat::Jpeg)
        .map_err(|_| ImageError::Unsaved)?;
    write_atomically(path, &encoded).map_err(|_| ImageError::Unsaved)
}

/// REQ-35a: stores the private copy and its thumbnail, returning the new file name.
pub fn store(image: &CheckedImage, backgrounds: &Path, thumbnails: &Path) -> Result<String, ImageError> {
    let file_name = format!("{}.{}", new_id(), image.extension);
    let decoded = decode(&image.bytes)?;
    fs::create_dir_all(backgrounds).map_err(|_| ImageError::Unsaved)?;
    fs::create_dir_all(thumbnails).map_err(|_| ImageError::Unsaved)?;
    write_atomically(&backgrounds.join(&file_name), &image.bytes).map_err(|_| ImageError::Unsaved)?;
    write_thumbnail(&decoded, &thumbnail_path(thumbnails, &file_name))?;
    Ok(file_name)
}

/// Appendix A: a thumbnail is a cache, rebuilt from the private copy when absent.
pub fn ensure_thumbnail(file_name: &str, backgrounds: &Path, thumbnails: &Path) -> Result<PathBuf, ImageError> {
    let path = thumbnail_path(thumbnails, file_name);
    if path.exists() {
        return Ok(path);
    }
    let bytes = fs::read(backgrounds.join(file_name)).map_err(|_| ImageError::Unreadable)?;
    fs::create_dir_all(thumbnails).map_err(|_| ImageError::Unsaved)?;
    write_thumbnail(&decode(&bytes)?, &path)?;
    Ok(path)
}

/// REQ-35h: removing a background deletes the stored files. A file already gone is fine.
pub fn remove(file_name: &str, backgrounds: &Path, thumbnails: &Path) {
    let _ = fs::remove_file(backgrounds.join(file_name));
    let _ = fs::remove_file(thumbnail_path(thumbnails, file_name));
    let _ = fs::remove_file(wide_thumbnail_path(thumbnails, file_name));
}

/// QA-33: deletes every background that no book references, returning how many went.
pub fn remove_unreferenced(referenced: &[String], backgrounds: &Path, thumbnails: &Path) -> usize {
    let Ok(entries) = fs::read_dir(backgrounds) else { return 0 };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !referenced.contains(&name) {
            remove(&name, backgrounds, thumbnails);
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};
    use tempfile::TempDir;

    fn encoded(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
        let image = RgbImage::from_pixel(width, height, Rgb([200, 100, 50]));
        let mut bytes = Vec::new();
        image.write_to(&mut Cursor::new(&mut bytes), format).unwrap();
        bytes
    }

    #[test]
    fn accepted_formats_report_their_extension() {
        assert_eq!(check(encoded(400, 600, ImageFormat::Png)).unwrap().extension, "png");
        assert_eq!(check(encoded(400, 600, ImageFormat::Jpeg)).unwrap().extension, "jpg");
        assert_eq!(check(encoded(400, 600, ImageFormat::WebP)).unwrap().extension, "webp");
    }

    #[test]
    fn other_types_and_text_are_rejected() {
        assert_eq!(check(b"GIF89a\x01\x00\x01\x00".to_vec()).unwrap_err(), ImageError::WrongType);
        assert_eq!(check(b"plain text".to_vec()).unwrap_err(), ImageError::WrongType);
        assert_eq!(ImageError::WrongType.to_string(), "That file is not a PNG, JPEG, or WebP image.");
    }

    #[test]
    fn oversized_bytes_are_rejected_before_any_decoding() {
        let bytes = vec![0u8; MAX_BYTES as usize + 1];
        assert_eq!(check(bytes).unwrap_err(), ImageError::TooLarge);
        assert_eq!(ImageError::TooLarge.to_string(), "That image is larger than 20 MB. Choose a smaller one.");
    }

    #[test]
    fn a_header_claiming_too_many_pixels_is_rejected_without_decoding() {
        // A valid PNG signature and IHDR declaring 20000 x 20000, with no pixel data at all.
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
        bytes.extend_from_slice(&20000u32.to_be_bytes());
        bytes.extend_from_slice(&20000u32.to_be_bytes());
        bytes.extend_from_slice(&[8, 2, 0, 0, 0]);
        let crc_input = bytes[12..].to_vec();
        bytes.extend_from_slice(&crc32(&crc_input).to_be_bytes());
        // An empty IDAT chunk, so the header parses and the only problem is the claimed size.
        bytes.extend_from_slice(&[0, 0, 0, 0, b'I', b'D', b'A', b'T']);
        bytes.extend_from_slice(&crc32(b"IDAT").to_be_bytes());
        assert_eq!(check(bytes).unwrap_err(), ImageError::TooManyPixels, "QA-37");
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in data {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }

    #[test]
    fn small_images_warn_and_still_pass() {
        assert!(check(encoded(200, 300, ImageFormat::Png)).unwrap().small, "REQ-35q");
        assert!(check(encoded(295, 1000, ImageFormat::Png)).unwrap().small);
        assert!(!check(encoded(1000, 1500, ImageFormat::Png)).unwrap().small);
        assert!(!check(encoded(296, 444, ImageFormat::Png)).unwrap().small);
    }

    #[test]
    fn thumbnails_cover_the_tile_at_two_to_three() {
        let wide = DynamicImage::ImageRgb8(RgbImage::new(2000, 500));
        let thumb = make_thumbnail(&wide);
        assert_eq!((thumb.width(), thumb.height()), (THUMB_WIDTH, THUMB_HEIGHT), "REQ-35c");
    }

    #[test]
    fn category_images_get_a_landscape_thumbnail_and_removing_deletes_it() {
        let dir = TempDir::new().unwrap();
        let (backgrounds, thumbnails) = (dir.path().join("bg"), dir.path().join("th"));
        let checked = check(encoded(1000, 600, ImageFormat::Png)).unwrap();
        let name = store_for_category(&checked, &backgrounds, &thumbnails).unwrap();
        let wide = wide_thumbnail_path(&thumbnails, &name);
        let thumb = image::open(&wide).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (WIDE_WIDTH, WIDE_HEIGHT), "REQ-45g");
        fs::remove_file(&wide).unwrap();
        ensure_wide_thumbnail(&name, &backgrounds, &thumbnails).unwrap();
        assert!(wide.exists(), "a missing thumbnail rebuilds");
        remove(&name, &backgrounds, &thumbnails);
        assert!(!wide.exists() && !backgrounds.join(&name).exists(), "REQ-35h");
    }

    #[test]
    fn the_category_small_image_rule_uses_landscape_sizes() {
        assert!(is_small_for_category(&check(encoded(400, 300, ImageFormat::Png)).unwrap()), "REQ-45h");
        assert!(!is_small_for_category(&check(encoded(432, 244, ImageFormat::Png)).unwrap()));
    }

    #[test]
    fn every_preset_has_its_artwork_on_disk() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/category-art");
        for (id, _) in PRESETS {
            let bytes = fs::read(root.join(format!("{id}.jpg"))).unwrap_or_else(|_| panic!("{id} is missing"));
            let image = decode(&bytes).unwrap();
            assert!(image.width() > image.height(), "{id} must be landscape");
        }
        assert!(is_preset("ink-wash") && !is_preset("../evil"));
    }

    #[test]
    fn storing_writes_both_files_and_removing_deletes_them() {
        let dir = TempDir::new().unwrap();
        let (backgrounds, thumbnails) = (dir.path().join("bg"), dir.path().join("th"));
        let checked = check(encoded(1000, 1500, ImageFormat::Jpeg)).unwrap();
        let name = store(&checked, &backgrounds, &thumbnails).unwrap();
        assert!(name.ends_with(".jpg"));
        assert!(backgrounds.join(&name).exists(), "REQ-35a");
        assert!(thumbnail_path(&thumbnails, &name).exists());
        remove(&name, &backgrounds, &thumbnails);
        assert!(!backgrounds.join(&name).exists(), "REQ-35h");
        assert!(!thumbnail_path(&thumbnails, &name).exists());
    }

    #[test]
    fn a_missing_thumbnail_rebuilds_from_the_private_copy() {
        let dir = TempDir::new().unwrap();
        let (backgrounds, thumbnails) = (dir.path().join("bg"), dir.path().join("th"));
        let checked = check(encoded(1000, 1500, ImageFormat::Png)).unwrap();
        let name = store(&checked, &backgrounds, &thumbnails).unwrap();
        fs::remove_file(thumbnail_path(&thumbnails, &name)).unwrap();
        let rebuilt = ensure_thumbnail(&name, &backgrounds, &thumbnails).unwrap();
        assert!(rebuilt.exists());
    }

    #[test]
    fn unreferenced_files_are_swept_and_referenced_ones_kept() {
        let dir = TempDir::new().unwrap();
        let (backgrounds, thumbnails) = (dir.path().join("bg"), dir.path().join("th"));
        let checked = check(encoded(400, 600, ImageFormat::Png)).unwrap();
        let keep = store(&checked, &backgrounds, &thumbnails).unwrap();
        let orphan = store(&checked, &backgrounds, &thumbnails).unwrap();
        assert_eq!(remove_unreferenced(std::slice::from_ref(&keep), &backgrounds, &thumbnails), 1, "QA-33");
        assert!(backgrounds.join(&keep).exists());
        assert!(!backgrounds.join(&orphan).exists());
    }
}
