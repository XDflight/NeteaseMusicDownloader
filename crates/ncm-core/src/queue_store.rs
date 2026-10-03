//! The unfinished download queue, kept on disk so that it survives a crash or a quit.
//!
//! Only what is needed to start the work again is stored: the batches that still have tracks
//! to download, with the options they were queued with, and the partial files that are
//! waiting to be continued. Progress is not stored; the resume journals next to the partial
//! files know what has been downloaded.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::engine::BatchRequest;
use crate::fsutil::write_atomic;
use crate::settings::AppPaths;

const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedBatch {
    /// The tracks that were not finished, with the collection and the options of the batch.
    pub request: BatchRequest,
    /// Partial files of tracks of this batch that were being downloaded, by track id.
    #[serde(default)]
    pub parts: HashMap<u64, PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedQueue {
    pub version: u32,
    pub batches: Vec<SavedBatch>,
}

impl Default for SavedQueue {
    fn default() -> Self {
        Self { version: VERSION, batches: Vec::new() }
    }
}

impl SavedQueue {
    pub fn track_count(&self) -> usize {
        self.batches.iter().map(|b| b.request.tracks.len()).sum()
    }

    /// The saved queue, or `None` when there is none, it is empty, or it cannot be read (an
    /// unreadable file is set aside as `queue.json.bak`).
    pub fn load(paths: &AppPaths) -> Option<SavedQueue> {
        let file = paths.queue_file();
        let bytes = fs::read(&file).ok()?;
        match serde_json::from_slice::<SavedQueue>(&bytes) {
            Ok(q) if q.version == VERSION && q.track_count() > 0 => Some(q),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!("saved queue unreadable ({e}); ignoring it");
                let _ = fs::rename(&file, file.with_extension("json.bak"));
                None
            }
        }
    }

    /// Write the queue; an empty queue removes the file instead.
    pub fn save(&self, paths: &AppPaths) -> io::Result<()> {
        if self.track_count() == 0 {
            return Self::forget(paths);
        }
        let json = serde_json::to_vec(self).map_err(io::Error::other)?;
        write_atomic(&paths.queue_file(), &json)
    }

    pub fn forget(paths: &AppPaths) -> io::Result<()> {
        match fs::remove_file(paths.queue_file()) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use ncm_api::Track;

    use super::*;
    use crate::engine::TrackJob;
    use crate::options::DownloadOptions;

    fn temp_paths(tag: &str) -> AppPaths {
        let dir = std::env::temp_dir().join(format!("ncm-queue-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        AppPaths { config_dir: dir.clone(), data_dir: dir }
    }

    fn batch(ids: &[u64]) -> SavedBatch {
        let tracks = ids
            .iter()
            .map(|&id| TrackJob { track: Track { id, name: format!("歌曲 {id}"), ..Track::default() }, index: id as usize })
            .collect();
        SavedBatch {
            request: BatchRequest {
                collection: "热歌榜".into(),
                collection_cover: Some("http://p1.music.126.net/c.jpg".into()),
                tracks,
                options: DownloadOptions::default(),
            },
            parts: HashMap::new(),
        }
    }

    #[test]
    fn roundtrip_keeps_tracks_options_and_partial_files() {
        let paths = temp_paths("roundtrip");
        let mut first = batch(&[1, 2, 3]);
        first.parts.insert(2, PathBuf::from("music/热歌榜/歌曲 2.mp3.part"));
        let queue = SavedQueue { batches: vec![first, batch(&[9])], ..SavedQueue::default() };
        queue.save(&paths).unwrap();

        let back = SavedQueue::load(&paths).unwrap();
        assert_eq!(back.track_count(), 4);
        assert_eq!(back.batches[0].request.collection, "热歌榜");
        assert_eq!(back.batches[0].request.tracks[1].track.name, "歌曲 2");
        assert_eq!(back.batches[0].request.options.level, queue.batches[0].request.options.level);
        assert_eq!(back.batches[0].parts[&2], PathBuf::from("music/热歌榜/歌曲 2.mp3.part"));
        fs::remove_dir_all(&paths.data_dir).ok();
    }

    #[test]
    fn saving_an_empty_queue_removes_the_file() {
        let paths = temp_paths("empty");
        SavedQueue { batches: vec![batch(&[1])], ..SavedQueue::default() }.save(&paths).unwrap();
        assert!(paths.queue_file().exists());
        SavedQueue::default().save(&paths).unwrap();
        assert!(!paths.queue_file().exists());
        assert!(SavedQueue::load(&paths).is_none());
        // Forgetting what is not there is fine.
        SavedQueue::forget(&paths).unwrap();
        fs::remove_dir_all(&paths.data_dir).ok();
    }

    #[test]
    fn an_unreadable_file_is_set_aside() {
        let paths = temp_paths("corrupt");
        fs::create_dir_all(&paths.data_dir).unwrap();
        fs::write(paths.queue_file(), b"{ not json").unwrap();
        assert!(SavedQueue::load(&paths).is_none());
        assert!(!paths.queue_file().exists());
        assert!(paths.data_dir.join("queue.json.bak").exists());
        fs::remove_dir_all(&paths.data_dir).ok();
    }

    #[test]
    fn a_queue_from_another_format_version_is_ignored() {
        let paths = temp_paths("version");
        let mut queue = SavedQueue { batches: vec![batch(&[1])], ..SavedQueue::default() };
        queue.version = 99;
        queue.save(&paths).unwrap();
        assert!(SavedQueue::load(&paths).is_none());
        fs::remove_dir_all(&paths.data_dir).ok();
    }
}
