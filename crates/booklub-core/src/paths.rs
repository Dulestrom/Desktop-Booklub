//! Data locations from Appendix A, honouring the XDG variables when they are set. Windows has
//! neither those nor a dependable HOME, so community builds there use %LOCALAPPDATA%.

use std::env;
use std::path::PathBuf;

const APP_DIR: &str = "booklub";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub database: PathBuf,
    pub backgrounds: PathBuf,
    pub thumbnails: PathBuf,
    pub backups: PathBuf,
    pub preferences: PathBuf,
    /// This device's sync key and certificate, which never leave it.
    pub device: PathBuf,
    /// One thumbnail per background, sent to phones unchanged for the image's life (Android
    /// SRS REQ-33e). Not in the cache, which may be cleared.
    pub sync_thumbnails: PathBuf,
}

impl Paths {
    /// The locations for the current user.
    pub fn for_user() -> Self {
        let (data, cache) = base_dirs(cfg!(windows), |name| env::var_os(name).map(PathBuf::from));
        Self::under(data.join(APP_DIR), cache.join(APP_DIR))
    }

    pub fn under(data: PathBuf, cache: PathBuf) -> Self {
        Self {
            database: data.join("library.sqlite3"),
            backgrounds: data.join("backgrounds"),
            thumbnails: cache.join("thumbnails"),
            backups: data.join("backups"),
            preferences: data.join("preferences.json"),
            device: data.join("device"),
            sync_thumbnails: data.join("sync-thumbnails"),
        }
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        if let Some(parent) = self.database.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(&self.backgrounds)?;
        std::fs::create_dir_all(&self.thumbnails)?;
        std::fs::create_dir_all(&self.backups)
    }
}

/// The folders that hold the application's data and cache folders. Takes its inputs as arguments
/// so that the Windows branch is compiled and tested on every platform.
fn base_dirs(windows: bool, var: impl Fn(&str) -> Option<PathBuf>) -> (PathBuf, PathBuf) {
    let data = var("XDG_DATA_HOME");
    let cache = var("XDG_CACHE_HOME");
    if windows {
        let local = var("LOCALAPPDATA").or_else(|| var("USERPROFILE").map(|home| home.join("AppData").join("Local"))).unwrap_or_else(|| PathBuf::from("."));
        return (data.unwrap_or_else(|| local.clone()), cache.unwrap_or_else(|| local.join("Cache")));
    }
    let home = var("HOME").unwrap_or_else(|| PathBuf::from("."));
    (data.unwrap_or_else(|| home.join(".local/share")), cache.unwrap_or_else(|| home.join(".cache")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<PathBuf> + 'a {
        move |name| pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| PathBuf::from(value))
    }

    #[test]
    fn linux_uses_the_appendix_a_folders() {
        let (data, cache) = base_dirs(false, lookup(&[("HOME", "/home/r")]));
        assert_eq!(data, PathBuf::from("/home/r/.local/share"));
        assert_eq!(cache, PathBuf::from("/home/r/.cache"));
    }

    #[test]
    fn xdg_overrides_win_everywhere() {
        for windows in [false, true] {
            let (data, cache) = base_dirs(windows, lookup(&[("HOME", "/h"), ("LOCALAPPDATA", "/l"), ("XDG_DATA_HOME", "/d"), ("XDG_CACHE_HOME", "/c")]));
            assert_eq!((data, cache), (PathBuf::from("/d"), PathBuf::from("/c")));
        }
    }

    #[test]
    fn windows_uses_local_app_data_and_never_the_working_folder() {
        let (data, cache) = base_dirs(true, lookup(&[("LOCALAPPDATA", "/l")]));
        assert_eq!(data, PathBuf::from("/l"));
        assert_eq!(cache, PathBuf::from("/l/Cache"));
        let (data, _) = base_dirs(true, lookup(&[("USERPROFILE", "/u")]));
        assert_eq!(data, PathBuf::from("/u/AppData/Local"));
    }
}
