//! Non-secret application settings and the locations they live in.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fsutil::write_atomic;
use crate::options::DownloadOptions;

const SETTINGS_FILE: &str = "settings.json";
const PORTABLE_MARKER: &str = "portable.flag";

#[derive(Debug, Clone)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl AppPaths {
    /// Per-user directories, or `<exe dir>/data` when a `portable.flag` file sits next to the
    /// executable (portable installs).
    pub fn discover() -> Self {
        if let Some(dir) = portable_dir() {
            return Self { config_dir: dir.clone(), data_dir: dir };
        }
        match directories::ProjectDirs::from("io.github", "XDflight", "NeteaseMusicDownloader") {
            Some(d) => Self { config_dir: d.config_dir().to_path_buf(), data_dir: d.data_local_dir().to_path_buf() },
            None => {
                let fallback = PathBuf::from(".ncm-downloader");
                Self { config_dir: fallback.clone(), data_dir: fallback }
            }
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join(SETTINGS_FILE)
    }
}

fn portable_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    dir.join(PORTABLE_MARKER).exists().then(|| dir.join("data"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
    pub auto_check: bool,
    pub include_prerelease: bool,
    /// Unix seconds of the last successful check.
    pub last_check: u64,
    /// A version the user chose to ignore.
    pub skipped_version: Option<String>,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self { auto_check: true, include_prerelease: false, last_check: 0, skipped_version: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub download: DownloadOptions,
    /// `http://`, `https://` or `socks5://` proxy; empty means direct (system proxy is not used).
    pub proxy: String,
    pub theme: Theme,
    pub update: UpdateSettings,
    /// Ask before quitting while downloads are running.
    pub confirm_quit_while_downloading: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download: DownloadOptions::default(),
            proxy: String::new(),
            theme: Theme::default(),
            update: UpdateSettings::default(),
            confirm_quit_while_downloading: true,
        }
    }
}

impl Settings {
    pub fn load(paths: &AppPaths) -> Self {
        let file = paths.settings_file();
        match fs::read(&file) {
            // Editors such as Notepad add a UTF-8 byte-order mark, which JSON parsers reject.
            Ok(bytes) => serde_json::from_slice(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes)).unwrap_or_else(|e| {
                tracing::warn!("settings unreadable ({e}); starting from defaults");
                let _ = fs::rename(&file, file.with_extension("json.bak"));
                Settings::default()
            }),
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&self, paths: &AppPaths) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        write_atomic(&paths.settings_file(), &json)
    }
}

pub fn dir_is_writable(dir: &Path) -> bool {
    fs::create_dir_all(dir).is_ok() && {
        let probe = dir.join(".ncm-write-test");
        let ok = fs::write(&probe, b"").is_ok();
        let _ = fs::remove_file(&probe);
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_forward_compat() {
        let dir = std::env::temp_dir().join(format!("ncm-settings-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let paths = AppPaths { config_dir: dir.clone(), data_dir: dir.clone() };
        let s = Settings { proxy: "socks5://127.0.0.1:1080".into(), theme: Theme::Dark, ..Settings::default() };
        s.save(&paths).unwrap();
        let back = Settings::load(&paths);
        assert_eq!(back.proxy, "socks5://127.0.0.1:1080");
        assert_eq!(back.theme, Theme::Dark);

        // Unknown and missing fields must not break loading.
        fs::write(paths.settings_file(), br#"{"theme":"light","some_future_field":1}"#).unwrap();
        let back = Settings::load(&paths);
        assert_eq!(back.theme, Theme::Light);
        assert!(back.update.auto_check);
        assert!(back.confirm_quit_while_downloading, "a file without the field gets the default (on)");

        fs::write(paths.settings_file(), br#"{"confirm_quit_while_downloading":false}"#).unwrap();
        assert!(!Settings::load(&paths).confirm_quit_while_downloading, "an explicit choice is kept");

        // A byte-order mark (Notepad, Windows PowerShell) must not make the file unreadable.
        let mut with_bom = b"\xEF\xBB\xBF".to_vec();
        with_bom.extend_from_slice(br#"{"theme":"dark"}"#);
        fs::write(paths.settings_file(), &with_bom).unwrap();
        assert_eq!(Settings::load(&paths).theme, Theme::Dark);

        // Corrupt file: back it up and fall back to defaults.
        fs::write(paths.settings_file(), b"{ not json").unwrap();
        let back = Settings::load(&paths);
        assert_eq!(back.theme, Theme::System);
        assert!(dir.join("settings.json.bak").exists());
        fs::remove_dir_all(&dir).ok();
    }
}
