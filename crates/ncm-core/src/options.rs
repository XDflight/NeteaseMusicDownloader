use std::path::PathBuf;

use ncm_api::Level;
use serde::{Deserialize, Serialize};

use crate::lyrics::{LyricsExtra, LyricsFile};
use crate::naming::{DEFAULT_TEMPLATE, Layout};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExistsPolicy {
    /// Leave an existing file alone.
    #[default]
    Skip,
    Overwrite,
    /// Download under a new name (`name (2).mp3`).
    KeepBoth,
}

/// Requested edge length of cover art.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverSize {
    #[default]
    Px1400,
    Px800,
    Px500,
    /// The image exactly as uploaded (can be very large).
    Original,
}

impl CoverSize {
    pub fn label(self) -> &'static str {
        match self {
            CoverSize::Px1400 => "1400 px",
            CoverSize::Px800 => "800 px",
            CoverSize::Px500 => "500 px",
            CoverSize::Original => "原图",
        }
    }

    /// Append the image-server resize parameter to a cover URL.
    pub fn apply(self, url: &str) -> String {
        let base = url.split('?').next().unwrap_or(url);
        match self {
            CoverSize::Original => base.to_owned(),
            CoverSize::Px1400 => format!("{base}?param=1400y1400"),
            CoverSize::Px800 => format!("{base}?param=800y800"),
            CoverSize::Px500 => format!("{base}?param=500y500"),
        }
    }
}

/// Where to save cover art as a separate image file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverFile {
    #[default]
    Off,
    /// `<name>.jpg` beside each track.
    PerTrack,
    /// `cover.jpg` in the folder (the playlist cover, or the album cover in artist/album layout).
    Folder,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadOptions {
    pub output_dir: PathBuf,
    pub level: Level,
    /// Accept a lower tier when the requested one is not available for a track.
    pub allow_lower_quality: bool,
    pub layout: Layout,
    pub filename_template: String,
    pub on_exists: ExistsPolicy,

    /// Write title / artist / album / track number / year into the file.
    pub embed_tags: bool,
    pub embed_cover: bool,
    pub cover_size: CoverSize,
    pub cover_file: CoverFile,
    pub embed_lyrics: bool,
    /// The separate lyrics file (`.lrc` / `.txt`) to save next to the audio, if any.
    pub lyrics_file: LyricsFile,
    pub lyrics_extra: LyricsExtra,
    /// Keep the `[mm:ss.xx]` tags inside the embedded lyrics (otherwise plain text).
    pub embed_timeline: bool,
    /// Store the song's web address in the comment tag.
    pub source_comment: bool,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            output_dir: default_output_dir(),
            level: Level::Exhigh,
            allow_lower_quality: true,
            layout: Layout::Collection,
            filename_template: DEFAULT_TEMPLATE.to_owned(),
            on_exists: ExistsPolicy::Skip,
            embed_tags: true,
            embed_cover: true,
            cover_size: CoverSize::Px1400,
            cover_file: CoverFile::Off,
            embed_lyrics: true,
            lyrics_file: LyricsFile::default(),
            lyrics_extra: LyricsExtra::None,
            embed_timeline: true,
            source_comment: false,
        }
    }
}

pub fn default_output_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|d| d.audio_dir().map(|p| p.to_path_buf()))
        .or_else(|| directories::UserDirs::new().map(|d| d.home_dir().join("Music")))
        .unwrap_or_else(|| PathBuf::from("Music"))
        .join("NeteaseMusicDownloader")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_folder_is_named_after_the_app() {
        assert_eq!(default_output_dir().file_name().and_then(|n| n.to_str()), Some("NeteaseMusicDownloader"));
    }

    #[test]
    fn cover_size_replaces_existing_param() {
        let u = "http://p1.music.126.net/abc.jpg?param=130y130";
        assert_eq!(CoverSize::Px800.apply(u), "http://p1.music.126.net/abc.jpg?param=800y800");
        assert_eq!(CoverSize::Original.apply(u), "http://p1.music.126.net/abc.jpg");
    }

    #[test]
    fn options_roundtrip_and_tolerate_missing_fields() {
        let o = DownloadOptions::default();
        let json = serde_json::to_string(&o).unwrap();
        let back: DownloadOptions = serde_json::from_str(&json).unwrap();
        assert_eq!(back.level, o.level);
        let partial: DownloadOptions = serde_json::from_str(r#"{"level":"lossless"}"#).unwrap();
        assert_eq!(partial.level, Level::Lossless);
        assert!(partial.embed_lyrics);
        assert_eq!(partial.lyrics_file, LyricsFile::Lrc, "the default is the classic .lrc file");
        let txt: DownloadOptions = serde_json::from_str(r#"{"lyrics_file":"lrc_or_txt"}"#).unwrap();
        assert_eq!(txt.lyrics_file, LyricsFile::LrcOrTxt);
    }
}
