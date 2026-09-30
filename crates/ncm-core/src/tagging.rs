//! Writing metadata, cover art and lyrics into the downloaded audio file.

use std::path::Path;

use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::probe::Probe;
use lofty::tag::{Accessor, ItemKey, Tag, TagType};
use ncm_api::Track;

#[derive(Debug, thiserror::Error)]
pub enum TagError {
    #[error("cannot read the audio file: {0}")]
    Read(String),
    #[error("cannot write tags: {0}")]
    Write(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageKind {
    Jpeg,
    Png,
}

impl ImageKind {
    /// Sniff the format from magic bytes.
    pub fn detect(data: &[u8]) -> Option<Self> {
        if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Self::Jpeg)
        } else if data.starts_with(&[0x89, b'P', b'N', b'G']) {
            Some(Self::Png)
        } else {
            None
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
        }
    }
}

pub struct TagData<'a> {
    pub track: &'a Track,
    pub album_artist: Option<&'a str>,
    /// Text for the lyrics frame (LRC or plain text).
    pub lyrics: Option<&'a str>,
    pub cover: Option<&'a [u8]>,
    pub comment: Option<&'a str>,
}

/// Overwrite the tags of `path` with `data`. Existing embedded cover art is replaced.
pub fn write_tags(path: &Path, data: &TagData<'_>) -> Result<(), TagError> {
    // Content-based detection: the file usually still carries a `.part` extension here.
    let probe = Probe::open(path).map_err(|e| TagError::Read(e.to_string()))?;
    let probe = probe.guess_file_type().map_err(|e| TagError::Read(e.to_string()))?;
    let mut file = probe.read().map_err(|e| TagError::Read(e.to_string()))?;
    let tag_type = file.primary_tag_type();
    if file.primary_tag().is_none() {
        file.insert_tag(Tag::new(tag_type));
    }
    let tag = file.primary_tag_mut().ok_or_else(|| TagError::Write("format has no writable tag".into()))?;

    let t = data.track;
    tag.set_title(t.name.clone());
    tag.set_artist(t.artist_names("/"));
    tag.set_album(t.album.name.clone());
    if t.track_no > 0 {
        tag.set_track(t.track_no);
    }
    if let Some(disc) = t.disc.trim().parse::<u32>().ok().filter(|d| *d > 0) {
        tag.set_disk(disc);
    }
    if let Some(year) = t.year() {
        tag.insert_text(ItemKey::Year, year.to_string());
    }
    if let Some(aa) = data.album_artist.filter(|s| !s.is_empty()) {
        tag.insert_text(ItemKey::AlbumArtist, aa.to_owned());
    }
    if let Some(c) = data.comment {
        tag.set_comment(c.to_owned());
    }
    if let Some(lyrics) = data.lyrics.filter(|s| !s.trim().is_empty()) {
        insert_lyrics(tag, lyrics);
    }
    if let Some(bytes) = data.cover
        && let Some(kind) = ImageKind::detect(bytes)
    {
        tag.remove_picture_type(PictureType::CoverFront);
        tag.push_picture(
            Picture::unchecked(bytes.to_vec())
                .pic_type(PictureType::CoverFront)
                .mime_type(match kind {
                    ImageKind::Jpeg => MimeType::Jpeg,
                    ImageKind::Png => MimeType::Png,
                })
                .build(),
        );
    }

    file.save_to_path(path, WriteOptions::default()).map_err(|e| TagError::Write(e.to_string()))
}

/// ID3v2 (`USLT`) and MP4 (`©lyr`) hold lyrics under `UnsyncLyrics`; Vorbis comments use the
/// widely supported `LYRICS` field, which lofty exposes as `Lyrics`.
fn insert_lyrics(tag: &mut Tag, lyrics: &str) {
    let (preferred, fallback) = match tag.tag_type() {
        TagType::VorbisComments => (ItemKey::Lyrics, ItemKey::UnsyncLyrics),
        _ => (ItemKey::UnsyncLyrics, ItemKey::Lyrics),
    };
    if !tag.insert_text(preferred, lyrics.to_owned()) {
        tag.insert_text(fallback, lyrics.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_images() {
        assert_eq!(ImageKind::detect(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(ImageKind::Jpeg));
        assert_eq!(ImageKind::detect(&[0x89, b'P', b'N', b'G', 0x0D]), Some(ImageKind::Png));
        assert_eq!(ImageKind::detect(b"GIF89a"), None);
    }
}
