//! Turning track metadata into safe file and folder names.

use std::path::{Path, PathBuf};

use ncm_api::Track;
use serde::{Deserialize, Serialize};

pub const AUDIO_EXTENSIONS: [&str; 6] = ["mp3", "flac", "m4a", "wav", "ogg", "ape"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// Everything directly in the output folder.
    Flat,
    /// `<out>/<playlist or album name>/`
    #[default]
    Collection,
    /// `<out>/<artist>/<album>/`
    ArtistAlbum,
}

pub struct NameContext<'a> {
    pub track: &'a Track,
    /// 1-based position in the collection, when meaningful.
    pub index: Option<usize>,
    pub collection: &'a str,
    /// Quality level actually delivered, e.g. `exhigh`.
    pub quality: &'a str,
}

pub const DEFAULT_TEMPLATE: &str = "{artist} - {title}";

/// Expand `{artist} {title} {album} {track} {disc} {year} {id} {index} {quality} {playlist}`.
/// Numeric placeholders accept a zero-pad width, e.g. `{track:02}` or `{index:03}`.
pub fn render_template(template: &str, ctx: &NameContext<'_>) -> String {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '{' {
            out.push(c);
            continue;
        }
        let mut key = String::new();
        let mut closed = false;
        for n in chars.by_ref() {
            if n == '}' {
                closed = true;
                break;
            }
            key.push(n);
        }
        if !closed {
            out.push('{');
            out.push_str(&key);
            break;
        }
        let (name, width) = match key.split_once(':') {
            Some((n, w)) => (n, w.parse::<usize>().ok()),
            None => (key.as_str(), None),
        };
        let pad = |n: u64| match width {
            Some(w) => format!("{n:0w$}"),
            None => n.to_string(),
        };
        let t = ctx.track;
        match name {
            "artist" | "artists" => out.push_str(&t.artist_names(", ")),
            "first_artist" => out.push_str(t.artists.first().map(|a| a.name.as_str()).unwrap_or("")),
            "title" | "name" => out.push_str(&t.name),
            "album" => out.push_str(&t.album.name),
            "track" => out.push_str(&pad(u64::from(t.track_no))),
            "disc" => out.push_str(&t.disc),
            "year" => {
                if let Some(y) = t.year() {
                    out.push_str(&y.to_string());
                }
            }
            "id" => out.push_str(&t.id.to_string()),
            "index" => out.push_str(&pad(ctx.index.unwrap_or(0) as u64)),
            "quality" => out.push_str(ctx.quality),
            "playlist" | "collection" => out.push_str(ctx.collection),
            other => {
                out.push('{');
                out.push_str(other);
                out.push('}');
            }
        }
    }
    let cleaned = sanitize_component(&out);
    if cleaned.is_empty() { sanitize_component(&format!("{}", ctx.track.id)) } else { cleaned }
}

/// Make `s` safe as a single path component on Windows, macOS and Linux.
pub fn sanitize_component(s: &str) -> String {
    const MAX_CHARS: usize = 100;
    let mut out = String::with_capacity(s.len());
    let mut last_space = false;
    for c in s.chars() {
        let mapped = match c {
            '\\' => '＼',
            '/' => '／',
            ':' => '：',
            '*' => '＊',
            '?' => '？',
            '"' => '＂',
            '<' => '＜',
            '>' => '＞',
            '|' => '｜',
            c if c.is_control() => ' ',
            c => c,
        };
        if mapped.is_whitespace() {
            if !last_space && !out.is_empty() {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(mapped);
            last_space = false;
        }
    }
    let mut out: String = out.trim_matches([' ', '.']).chars().take(MAX_CHARS).collect();
    out = out.trim_end_matches([' ', '.']).to_owned();
    if is_reserved_windows_name(&out) {
        out.push('_');
    }
    out
}

fn is_reserved_windows_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0')
}

/// Folder that holds the audio file for `layout`.
pub fn target_dir(layout: Layout, out_dir: &Path, ctx: &NameContext<'_>) -> PathBuf {
    match layout {
        Layout::Flat => out_dir.to_path_buf(),
        Layout::Collection => {
            let name = sanitize_component(ctx.collection);
            if name.is_empty() { out_dir.to_path_buf() } else { out_dir.join(name) }
        }
        Layout::ArtistAlbum => {
            let artist = sanitize_component(ctx.track.artists.first().map(|a| a.name.as_str()).unwrap_or("Unknown Artist"));
            let album = sanitize_component(&ctx.track.album.name);
            out_dir.join(if artist.is_empty() { "Unknown Artist".into() } else { artist }).join(if album.is_empty() {
                "Unknown Album".into()
            } else {
                album
            })
        }
    }
}

/// An existing audio file `dir/base.<ext>` for any known extension.
pub fn find_existing(dir: &Path, base: &str) -> Option<PathBuf> {
    AUDIO_EXTENSIONS
        .iter()
        .map(|ext| dir.join(format!("{base}.{ext}")))
        .find(|p| p.metadata().map(|m| m.len() > 0).unwrap_or(false))
}

/// `base`, `base (2)`, `base (3)`, ... — the first name with no audio file yet.
pub fn unique_base(dir: &Path, base: &str) -> String {
    if find_existing(dir, base).is_none() {
        return base.to_owned();
    }
    (2..).map(|n| format!("{base} ({n})")).find(|candidate| find_existing(dir, candidate).is_none()).expect("unbounded iterator")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ncm_api::{AlbumRef, Artist};

    fn track() -> Track {
        Track {
            id: 347230,
            name: "海阔天空".into(),
            artists: vec![Artist { id: 1, name: "Beyond".into() }, Artist { id: 2, name: "黄家驹".into() }],
            album: AlbumRef { id: 3, name: "乐与怒: 精选?".into(), pic_url: None },
            track_no: 3,
            disc: "1".into(),
            publish_time_ms: Some(1_577_836_800_000),
            ..Track::default()
        }
    }

    fn ctx(t: &Track) -> NameContext<'_> {
        NameContext { track: t, index: Some(7), collection: "我的歌单/2024", quality: "exhigh" }
    }

    #[test]
    fn default_template() {
        let t = track();
        assert_eq!(render_template(DEFAULT_TEMPLATE, &ctx(&t)), "Beyond, 黄家驹 - 海阔天空");
    }

    #[test]
    fn numeric_padding_and_fields() {
        let t = track();
        assert_eq!(render_template("{index:03}. {title} [{quality}] {year}", &ctx(&t)), "007. 海阔天空 [exhigh] 2020");
        assert_eq!(render_template("{track:02}-{title}", &ctx(&t)), "03-海阔天空");
    }

    #[test]
    fn unsafe_characters_are_replaced() {
        assert_eq!(sanitize_component("a/b:c*d?e\"f<g>h|i"), "a／b：c＊d？e＂f＜g＞h｜i");
        assert_eq!(sanitize_component("  trailing dots... "), "trailing dots");
        assert_eq!(sanitize_component("CON"), "CON_");
        assert_eq!(sanitize_component("lpt3.txt"), "lpt3.txt_");
        assert_eq!(sanitize_component("tab\tand\nnewline"), "tab and newline");
    }

    #[test]
    fn long_names_are_truncated() {
        let long = "长".repeat(300);
        assert_eq!(sanitize_component(&long).chars().count(), 100);
    }

    #[test]
    fn layouts() {
        let t = track();
        let out = Path::new("out");
        assert_eq!(target_dir(Layout::Flat, out, &ctx(&t)), PathBuf::from("out"));
        assert_eq!(target_dir(Layout::Collection, out, &ctx(&t)), PathBuf::from("out").join("我的歌单／2024"));
        assert_eq!(target_dir(Layout::ArtistAlbum, out, &ctx(&t)), PathBuf::from("out").join("Beyond").join("乐与怒： 精选？"));
    }

    #[test]
    fn unknown_placeholder_is_preserved() {
        let t = track();
        assert_eq!(render_template("{title}{nope}", &ctx(&t)), "海阔天空{nope}");
    }

    #[test]
    fn keep_both_picks_a_free_name() {
        let dir = std::env::temp_dir().join(format!("ncm-naming-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("song.mp3"), b"x").unwrap();
        std::fs::write(dir.join("song (2).flac"), b"x").unwrap();
        assert_eq!(unique_base(&dir, "song"), "song (3)");
        assert_eq!(unique_base(&dir, "other"), "other");
        assert!(find_existing(&dir, "song").is_some());
        std::fs::remove_dir_all(&dir).ok();
    }
}
