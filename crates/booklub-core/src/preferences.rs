//! This device's display choices (REQ-47). They aren't part of the library, so they stay out of
//! the database, exports, and backups. A missing or damaged file only means the defaults.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub const MIN_TILE_SCALE: f64 = 0.75;
pub const MAX_TILE_SCALE: f64 = 1.6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BooksView {
    #[default]
    Pictures,
    List,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Preferences {
    pub appearance: Appearance,
    pub books_view: BooksView,
    pub tile_scale: f64,
}

impl Default for Preferences {
    fn default() -> Self {
        Self { appearance: Appearance::default(), books_view: BooksView::default(), tile_scale: 1.0 }
    }
}

impl Preferences {
    pub fn load(path: &Path) -> Self {
        let mut loaded: Self = std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        loaded.tile_scale = clamp_scale(loaded.tile_scale);
        loaded
    }

    /// Writes beside the target and then renames, so a crash can't leave half a file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&temporary, path)
    }
}

pub fn clamp_scale(scale: f64) -> f64 {
    if scale.is_finite() { scale.clamp(MIN_TILE_SCALE, MAX_TILE_SCALE) } else { 1.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("booklub-preferences-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn choices_survive_a_restart() {
        let path = folder("round-trip").join("preferences.json");
        let chosen = Preferences { appearance: Appearance::Dark, books_view: BooksView::List, tile_scale: 1.25 };
        chosen.save(&path).unwrap();
        assert_eq!(Preferences::load(&path), chosen);
    }

    #[test]
    fn a_missing_or_damaged_file_means_the_defaults() {
        let dir = folder("damaged");
        assert_eq!(Preferences::load(&dir.join("absent.json")), Preferences::default());
        let path = dir.join("preferences.json");
        std::fs::write(&path, b"{ not json").unwrap();
        assert_eq!(Preferences::load(&path), Preferences::default());
    }

    #[test]
    fn a_missing_or_unknown_value_does_not_lose_the_others() {
        let dir = folder("partial");
        let path = dir.join("preferences.json");
        std::fs::write(&path, br#"{"booksView":"list","futureSetting":1}"#).unwrap();
        let loaded = Preferences::load(&path);
        assert_eq!(loaded.books_view, BooksView::List);
        assert_eq!(loaded.appearance, Appearance::System);
    }

    #[test]
    fn an_out_of_range_size_is_brought_back_into_range() {
        let dir = folder("range");
        let path = dir.join("preferences.json");
        std::fs::write(&path, br#"{"tileScale":40.0}"#).unwrap();
        assert_eq!(Preferences::load(&path).tile_scale, MAX_TILE_SCALE);
        assert_eq!(clamp_scale(f64::NAN), 1.0);
    }
}
