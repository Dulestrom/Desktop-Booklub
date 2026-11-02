//! The `.booklub` backup archive (REQ-37, Appendix B): a ZIP holding `library.json` and
//! `backgrounds/<uuid>.<png|jpg|webp>`. A restore treats the file as hostile input.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::Path;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::background::MAX_BYTES;
use crate::model::{CategoryBackground, Library};
use crate::portability::{export_json, parse_import, ImportError, ParsedImport};

const LIBRARY_ENTRY: &str = "library.json";
const BACKGROUND_PREFIX: &str = "backgrounds/";
/// Appendix B.
const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// More than any library could need, and it keeps a crafted file from filling memory.
const MAX_LIBRARY_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArchiveError {
    #[error("That file could not be read as a Booklub archive.")]
    Unreadable,
    #[error("That archive contains \"{0}\", which does not belong in a Booklub archive.")]
    UnexpectedEntry(String),
    #[error("That archive is larger than Booklub allows.")]
    TooLarge,
    #[error("That archive has no library.json inside it.")]
    MissingLibrary,
    #[error(transparent)]
    Import(#[from] ImportError),
    #[error("The archive could not be written.")]
    Unwritable,
}

/// True for a bare `<uuid>.<png|jpg|webp>` name: no directories, no parent segments.
fn is_background_name(name: &str) -> bool {
    let Some((stem, extension)) = name.rsplit_once('.') else { return false };
    matches!(extension, "png" | "jpg" | "webp")
        && !stem.is_empty()
        && stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// REQ-37: exports the library with every background it references.
pub fn export(library: &Library, backgrounds: &Path, target: &Path) -> Result<(), ArchiveError> {
    let file = File::create(target).map_err(|_| ArchiveError::Unwritable)?;
    let mut writer = ZipWriter::new(file);
    let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    // Images are already compressed, so deflating them again only costs time.
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    writer.start_file(LIBRARY_ENTRY, deflated).map_err(|_| ArchiveError::Unwritable)?;
    writer.write_all(export_json(library).as_bytes()).map_err(|_| ArchiveError::Unwritable)?;

    for name in library.background_files() {
        if !is_background_name(&name) {
            continue;
        }
        let Ok(bytes) = std::fs::read(backgrounds.join(&name)) else { continue };
        writer.start_file(format!("{BACKGROUND_PREFIX}{name}"), stored).map_err(|_| ArchiveError::Unwritable)?;
        writer.write_all(&bytes).map_err(|_| ArchiveError::Unwritable)?;
    }
    writer.finish().map_err(|_| ArchiveError::Unwritable)?;
    Ok(())
}

pub struct RestoredArchive {
    pub import: ParsedImport,
    /// File name to image bytes, for the images the archive really holds.
    pub images: HashMap<String, Vec<u8>>,
}

/// REQ-37a, REQ-37b, QA-37: validates the entry names and declared sizes before reading
/// anything, then reads each entry with its limit enforced on the actual bytes too.
pub fn read(path: &Path) -> Result<RestoredArchive, ArchiveError> {
    read_from(File::open(path).map_err(|_| ArchiveError::Unreadable)?)
}

pub fn read_from<R: Read + Seek>(reader: R) -> Result<RestoredArchive, ArchiveError> {
    let mut archive = ZipArchive::new(reader).map_err(|_| ArchiveError::Unreadable)?;

    let mut declared = 0u64;
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).map_err(|_| ArchiveError::Unreadable)?;
        let name = entry.name().map_err(|_| ArchiveError::Unreadable)?.to_string();
        declared = declared.saturating_add(entry.size());
        if declared > MAX_TOTAL_BYTES {
            return Err(ArchiveError::TooLarge);
        }
        if name == LIBRARY_ENTRY {
            if entry.size() > MAX_LIBRARY_BYTES {
                return Err(ArchiveError::TooLarge);
            }
            continue;
        }
        match name.strip_prefix(BACKGROUND_PREFIX) {
            Some(image) if is_background_name(image) => {
                if entry.size() > MAX_BYTES {
                    return Err(ArchiveError::TooLarge);
                }
            }
            // A directory entry for the images folder is harmless.
            Some("") => {}
            _ => return Err(ArchiveError::UnexpectedEntry(name)),
        }
    }

    let mut json = None;
    let mut images = HashMap::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(|_| ArchiveError::Unreadable)?;
        let name = entry.name().map_err(|_| ArchiveError::Unreadable)?.to_string();
        if name == BACKGROUND_PREFIX {
            continue;
        }
        let limit = if name == LIBRARY_ENTRY { MAX_LIBRARY_BYTES } else { MAX_BYTES };
        let mut bytes = Vec::new();
        // Declared sizes can lie, so the limit also applies to what actually comes out.
        entry.take(limit + 1).read_to_end(&mut bytes).map_err(|_| ArchiveError::Unreadable)?;
        if bytes.len() as u64 > limit {
            return Err(ArchiveError::TooLarge);
        }
        if name == LIBRARY_ENTRY {
            json = Some(String::from_utf8(bytes).map_err(|_| ArchiveError::Unreadable)?);
        } else if let Some(image) = name.strip_prefix(BACKGROUND_PREFIX) {
            images.insert(image.to_string(), bytes);
        }
    }

    let json = json.ok_or(ArchiveError::MissingLibrary)?;
    let mut import = parse_import(&json)?;

    // REQ-37d
    for book in &mut import.library.books {
        if book.background_file.as_deref().is_some_and(|name| !images.contains_key(name)) {
            book.background_file = None;
            import.corrections.cleared_backgrounds += 1;
        }
    }
    clear_categories_missing(&mut import, |name| images.contains_key(name));
    Ok(RestoredArchive { import, images })
}

/// REQ-37d for a plain JSON import, which carries no images: a reference survives only
/// when the file already exists in the store.
pub fn clear_absent_backgrounds(import: &mut ParsedImport, backgrounds: &Path) {
    for book in &mut import.library.books {
        if let Some(name) = book.background_file.as_deref() {
            if !backgrounds.join(name).is_file() {
                book.background_file = None;
                import.corrections.cleared_backgrounds += 1;
            }
        }
    }
    clear_categories_missing(import, |name| backgrounds.join(name).is_file());
}

/// REQ-37d for Categories. Built-in artwork is always present, so only files can go missing.
fn clear_categories_missing(import: &mut ParsedImport, exists: impl Fn(&str) -> bool) {
    for category in &mut import.library.categories {
        let missing = category.background.as_ref().and_then(CategoryBackground::file_name).is_some_and(|name| !exists(name));
        if missing {
            category.background = None;
            import.corrections.cleared_backgrounds += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{default_goals, Book, Category};
    use std::io::Cursor;
    use tempfile::TempDir;

    const NAME: &str = "0f8a3c1e-1111-4222-8333-444455556666.jpg";

    fn library(background: Option<&str>) -> Library {
        let mut book = Book { title: "Ѓердан".into(), ..Book::new(0) };
        book.background_file = background.map(str::to_string);
        Library {
            books: vec![book],
            categories: vec![Category { id: "c".into(), name: "Design".into(), sub_categories: vec![], background: None }],
            goals: default_goals(),
        }
    }

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buffer = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(&mut buffer);
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, bytes) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
        buffer.into_inner()
    }

    fn json() -> Vec<u8> {
        export_json(&library(None)).into_bytes()
    }

    #[test]
    fn export_then_read_keeps_the_library_and_its_images() {
        let dir = TempDir::new().unwrap();
        let backgrounds = dir.path().join("bg");
        std::fs::create_dir_all(&backgrounds).unwrap();
        std::fs::write(backgrounds.join(NAME), b"jpeg bytes").unwrap();
        let target = dir.path().join("backup.booklub");

        export(&library(Some(NAME)), &backgrounds, &target).unwrap();
        let restored = read(&target).unwrap();

        assert_eq!(restored.import.library.books[0].title, "Ѓердан");
        assert_eq!(restored.import.library.books[0].background_file.as_deref(), Some(NAME));
        assert_eq!(restored.images[NAME], b"jpeg bytes");
        assert_eq!(restored.import.corrections.cleared_backgrounds, 0);
    }

    #[test]
    fn category_images_travel_in_the_archive_and_clear_when_missing() {
        let dir = TempDir::new().unwrap();
        let backgrounds = dir.path().join("bg");
        std::fs::create_dir_all(&backgrounds).unwrap();
        std::fs::write(backgrounds.join(NAME), b"category art").unwrap();
        let target = dir.path().join("backup.booklub");
        let mut lib = library(None);
        lib.categories[0].background = Some(CategoryBackground::File(NAME.into()));
        export(&lib, &backgrounds, &target).unwrap();

        let restored = read(&target).unwrap();
        assert_eq!(restored.images[NAME], b"category art", "REQ-37");
        assert_eq!(restored.import.library.categories[0].background, lib.categories[0].background);

        let bytes = zip_with(&[(LIBRARY_ENTRY, export_json(&lib).as_bytes())]);
        let restored = read_from(Cursor::new(bytes)).unwrap();
        assert_eq!(restored.import.library.categories[0].background, None);
        assert_eq!(restored.import.corrections.cleared_backgrounds, 1, "REQ-37d");
    }

    #[test]
    fn a_background_missing_from_the_archive_is_cleared_and_counted() {
        let mut lib = library(Some(NAME));
        lib.books[0].title = "T".into();
        let bytes = zip_with(&[(LIBRARY_ENTRY, export_json(&lib).as_bytes())]);
        let restored = read_from(Cursor::new(bytes)).unwrap();
        assert_eq!(restored.import.library.books[0].background_file, None);
        assert_eq!(restored.import.corrections.cleared_backgrounds, 1, "REQ-37d");
    }

    #[test]
    fn path_traversal_and_foreign_entries_are_rejected() {
        for bad in [
            "../evil.jpg",
            "backgrounds/../../evil.jpg",
            "/etc/passwd",
            "backgrounds/sub/dir.jpg",
            "backgrounds/evil.exe",
            "backgrounds/.hidden.jpg",
            "notes.txt",
        ] {
            let bytes = zip_with(&[(LIBRARY_ENTRY, &json()), (bad, b"x")]);
            let result = read_from(Cursor::new(bytes));
            assert!(matches!(result, Err(ArchiveError::UnexpectedEntry(_))), "{bad} must be refused (REQ-37b)");
        }
    }

    #[test]
    fn an_oversized_image_is_rejected() {
        let huge = vec![0u8; MAX_BYTES as usize + 1];
        let bytes = zip_with(&[(LIBRARY_ENTRY, &json()), (&format!("backgrounds/{NAME}"), &huge)]);
        assert_eq!(read_from(Cursor::new(bytes)).err(), Some(ArchiveError::TooLarge), "QA-37");
    }

    #[test]
    fn an_archive_with_no_library_is_rejected() {
        let bytes = zip_with(&[(&format!("backgrounds/{NAME}"), b"x")]);
        assert_eq!(read_from(Cursor::new(bytes)).err(), Some(ArchiveError::MissingLibrary));
    }

    #[test]
    fn a_non_zip_is_rejected() {
        assert_eq!(read_from(Cursor::new(b"nope".to_vec())).err(), Some(ArchiveError::Unreadable));
    }

    #[test]
    fn plain_json_imports_keep_only_backgrounds_already_stored() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(NAME), b"x").unwrap();
        let mut import = parse_import(&export_json(&library(Some("absent.jpg")))).unwrap();
        clear_absent_backgrounds(&mut import, dir.path());
        assert_eq!(import.library.books[0].background_file, None);
        assert_eq!(import.corrections.cleared_backgrounds, 1);

        let mut import = parse_import(&export_json(&library(Some(NAME)))).unwrap();
        clear_absent_backgrounds(&mut import, dir.path());
        assert_eq!(import.library.books[0].background_file.as_deref(), Some(NAME));
    }
}
