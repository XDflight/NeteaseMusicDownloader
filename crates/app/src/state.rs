//! Plain data shared by the UI pages.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use eframe::egui::TextureHandle;
use ncm_api::{Account, Collection, PlaylistSummary, ResourceKind, Track};
use ncm_core::{BatchId, BatchRequest, BatchSummary, DownloadOptions, FailKind, SavedBatch, SavedQueue, Stage, TrackJob};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Browse,
    Queue,
    Settings,
    About,
}

pub enum Loadable<T> {
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

// ---------------------------------------------------------------------------- browse

pub struct CollectionView {
    pub col: Collection,
    pub selected: Vec<bool>,
    pub filter: String,
    /// Indices into `col.tracks` that pass the filter.
    pub visible: Vec<usize>,
    pub cover: Option<TextureHandle>,
    /// The exact URL the cover was requested with.
    pub cover_url: Option<String>,
}

impl CollectionView {
    pub fn new(col: Collection) -> Self {
        let n = col.tracks.len();
        Self { col, selected: vec![false; n], filter: String::new(), visible: (0..n).collect(), cover: None, cover_url: None }
    }

    pub fn refilter(&mut self) {
        let f = self.filter.trim().to_lowercase();
        self.visible = (0..self.col.tracks.len())
            .filter(|&i| {
                if f.is_empty() {
                    return true;
                }
                let t = &self.col.tracks[i];
                t.name.to_lowercase().contains(&f)
                    || t.album.name.to_lowercase().contains(&f)
                    || t.artists.iter().any(|a| a.name.to_lowercase().contains(&f))
            })
            .collect();
    }

    pub fn selected_count(&self) -> usize {
        self.selected.iter().filter(|s| **s).count()
    }
}

pub struct Browse {
    pub input: String,
    pub bare_kind: ResourceKind,
    pub loading: bool,
    pub error: Option<String>,
    pub view: Option<CollectionView>,
    pub my: Loadable<Vec<PlaylistSummary>>,
    /// Guards against a slow response overwriting a newer one.
    pub request_seq: u64,
}

impl Default for Browse {
    fn default() -> Self {
        Self {
            input: String::new(),
            bare_kind: ResourceKind::Playlist,
            loading: false,
            error: None,
            view: None,
            my: Loadable::Idle,
            request_seq: 0,
        }
    }
}

// ----------------------------------------------------------------------------- queue

/// Batches restored from the previous run have ids from here on until they are handed to the
/// engine (which counts up from 1, so the two ranges never meet).
pub const RESTORED_BASE: BatchId = 1 << 62;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QState {
    Queued,
    Working(Stage),
    Done,
    Skipped,
    Failed(FailKind),
    Cancelled,
}

impl QState {
    pub fn is_finished(&self) -> bool {
        matches!(self, QState::Done | QState::Skipped | QState::Failed(_) | QState::Cancelled)
    }
}

pub struct QItem {
    pub batch: BatchId,
    pub track: Track,
    pub index: usize,
    pub state: QState,
    pub done: u64,
    pub total: u64,
    /// Delivered quality label, e.g. `极高 · MP3`.
    pub quality: Option<String>,
    pub path: Option<PathBuf>,
    pub message: Option<String>,
    /// The partial file of a download that did not finish. It stays on disk when the download is
    /// interrupted, so that it can be continued.
    pub part: Option<PathBuf>,
    /// Position in the order in which items finished (the finished list shows the newest first).
    pub finished_seq: u64,
}

impl QItem {
    pub fn new(batch: BatchId, track: Track, index: usize) -> Self {
        Self {
            batch,
            track,
            index,
            state: QState::Queued,
            done: 0,
            total: 0,
            quality: None,
            path: None,
            message: None,
            part: None,
            finished_seq: 0,
        }
    }

    /// Restored from the previous run and not handed to the engine yet.
    pub fn is_restored(&self) -> bool {
        self.batch >= RESTORED_BASE
    }

    /// Worth keeping across a restart: not finished, or stopped with a partial file that can be continued.
    fn is_resumable(&self) -> bool {
        !self.state.is_finished() || (self.part.is_some() && matches!(self.state, QState::Failed(_) | QState::Cancelled))
    }
}

/// What a batch was queued with; needed to save it and to start it again.
pub struct BatchMeta {
    pub collection: String,
    pub collection_cover: Option<String>,
    pub options: DownloadOptions,
}

pub struct BatchInfo {
    pub summary: Option<BatchSummary>,
    pub meta: Option<BatchMeta>,
}

#[derive(Default)]
pub struct Queue {
    pub items: Vec<QItem>,
    pub lookup: HashMap<(BatchId, u64), usize>,
    pub batches: HashMap<BatchId, BatchInfo>,
    /// Downloading is paused (mirrors the engine).
    pub paused: bool,
    /// How many items have finished so far; orders the finished list.
    pub finished_count: u64,
    /// The queue changed in a way that has to be saved.
    pub dirty: bool,
}

impl Queue {
    /// Items the engine is working on or has queued.
    pub fn active(&self) -> usize {
        self.items.iter().filter(|i| !i.state.is_finished() && !i.is_restored()).count()
    }

    /// Items restored from the previous run that wait for the user to continue them.
    pub fn restored(&self) -> usize {
        self.items.iter().filter(|i| i.is_restored()).count()
    }

    pub fn count(&self, f: impl Fn(&QState) -> bool) -> usize {
        self.items.iter().filter(|i| f(&i.state)).count()
    }

    /// Record how item `i` ended.
    pub fn finish(&mut self, i: usize, state: QState) {
        self.finished_count += 1;
        let it = &mut self.items[i];
        it.state = state;
        it.finished_seq = self.finished_count;
        self.dirty = true;
    }

    /// Indices of the items still to do (running first, then in queue order, restored ones last)
    /// and of the finished ones (newest first).
    pub fn lists(&self) -> (Vec<usize>, Vec<usize>) {
        let mut pending: Vec<usize> = (0..self.items.len()).filter(|&i| !self.items[i].state.is_finished()).collect();
        pending.sort_by_key(|&i| {
            let it = &self.items[i];
            let rank = if it.is_restored() {
                2
            } else if matches!(it.state, QState::Working(_)) {
                0
            } else {
                1
            };
            (rank, i)
        });
        let mut finished: Vec<usize> = (0..self.items.len()).filter(|&i| self.items[i].state.is_finished()).collect();
        finished.sort_by_key(|&i| std::cmp::Reverse(self.items[i].finished_seq));
        (pending, finished)
    }

    fn reindex(&mut self) {
        self.lookup = self.items.iter().enumerate().map(|(n, i)| ((i.batch, i.track.id), n)).collect();
        let live: std::collections::HashSet<BatchId> = self.items.iter().map(|i| i.batch).collect();
        self.batches.retain(|id, _| live.contains(id));
    }

    /// Remove the finished items. A failed one may still own a partial file, which goes with it.
    pub fn clear_finished(&mut self) {
        for it in self.items.iter().filter(|i| matches!(i.state, QState::Failed(_))) {
            if let Some(part) = &it.part {
                ncm_core::fetch::discard_partial(part);
            }
        }
        self.items.retain(|i| !i.state.is_finished());
        self.reindex();
        self.dirty = true;
    }

    /// The part of the queue that is worth saving, grouped by batch in queue order.
    pub fn to_saved(&self) -> SavedQueue {
        let mut order: Vec<BatchId> = Vec::new();
        for it in self.items.iter().filter(|i| i.is_resumable()) {
            if !order.contains(&it.batch) {
                order.push(it.batch);
            }
        }
        let batches = order
            .into_iter()
            .filter_map(|id| {
                let meta = self.batches.get(&id)?.meta.as_ref()?;
                let mut tracks = Vec::new();
                let mut parts = HashMap::new();
                for it in self.items.iter().filter(|i| i.batch == id && i.is_resumable()) {
                    tracks.push(TrackJob { track: it.track.clone(), index: it.index });
                    if let Some(part) = &it.part {
                        parts.insert(it.track.id, part.clone());
                    }
                }
                let request = BatchRequest {
                    collection: meta.collection.clone(),
                    collection_cover: meta.collection_cover.clone(),
                    tracks,
                    options: meta.options.clone(),
                };
                Some(SavedBatch { request, parts })
            })
            .collect();
        SavedQueue { batches, ..SavedQueue::default() }
    }

    /// Add the saved queue as items that wait to be continued.
    pub fn restore(&mut self, saved: SavedQueue) {
        for (n, batch) in saved.batches.into_iter().enumerate() {
            let id = RESTORED_BASE + n as u64;
            let SavedBatch { request, parts } = batch;
            self.batches.insert(
                id,
                BatchInfo {
                    summary: None,
                    meta: Some(BatchMeta {
                        collection: request.collection,
                        collection_cover: request.collection_cover,
                        options: request.options,
                    }),
                },
            );
            for job in request.tracks {
                let mut item = QItem::new(id, job.track, job.index);
                item.part = parts.get(&item.track.id).cloned();
                self.items.push(item);
            }
        }
        self.reindex();
        self.lookup = self.items.iter().enumerate().map(|(n, i)| ((i.batch, i.track.id), n)).collect();
    }

    /// The restored batches as requests the engine can start, in order.
    pub fn restored_requests(&self) -> Vec<(BatchId, BatchRequest)> {
        let mut ids: Vec<BatchId> = self.items.iter().filter(|i| i.is_restored()).map(|i| i.batch).collect();
        ids.sort();
        ids.dedup();
        ids.into_iter()
            .filter_map(|id| {
                let meta = self.batches.get(&id)?.meta.as_ref()?;
                let tracks = self
                    .items
                    .iter()
                    .filter(|i| i.batch == id)
                    .map(|i| TrackJob { track: i.track.clone(), index: i.index })
                    .collect();
                let request = BatchRequest {
                    collection: meta.collection.clone(),
                    collection_cover: meta.collection_cover.clone(),
                    tracks,
                    options: meta.options.clone(),
                };
                Some((id, request))
            })
            .collect()
    }

    /// A restored batch has been given to the engine under a new id.
    pub fn rebatch(&mut self, old: BatchId, new: BatchId) {
        for it in self.items.iter_mut().filter(|i| i.batch == old) {
            it.batch = new;
        }
        if let Some(info) = self.batches.remove(&old) {
            self.batches.insert(new, info);
        }
        self.lookup = self.items.iter().enumerate().map(|(n, i)| ((i.batch, i.track.id), n)).collect();
        self.dirty = true;
    }

    /// Drop the restored items together with their partial files.
    pub fn discard_restored(&mut self) {
        for it in self.items.iter().filter(|i| i.is_restored()) {
            if let Some(part) = &it.part {
                ncm_core::fetch::discard_partial(part);
            }
        }
        self.items.retain(|i| !i.is_restored());
        self.reindex();
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn track(id: u64) -> Track {
        Track { id, name: format!("track {id}"), ..Track::default() }
    }

    fn queue_with(batch: BatchId, ids: &[u64]) -> Queue {
        let mut q = Queue::default();
        q.batches.insert(
            batch,
            BatchInfo {
                summary: None,
                meta: Some(BatchMeta { collection: "list".into(), collection_cover: None, options: DownloadOptions::default() }),
            },
        );
        for &id in ids {
            q.items.push(QItem::new(batch, track(id), id as usize));
        }
        q.reindex();
        q
    }

    #[test]
    fn finished_items_move_to_their_own_list_newest_first() {
        let mut q = queue_with(1, &[1, 2, 3, 4, 5]);
        q.items[2].state = QState::Working(Stage::Downloading);
        q.items[3].state = QState::Working(Stage::Resolving);
        q.finish(4, QState::Done);
        q.finish(0, QState::Failed(FailKind::Network));
        q.finish(1, QState::Skipped);

        let (pending, finished) = q.lists();
        // Running items first (in queue order), then the ones that wait.
        assert_eq!(pending, [2, 3]);
        // Items 4, 0 and 1 finished in that order; the newest is shown first.
        assert_eq!(finished, [1, 0, 4]);
        assert_eq!(q.active(), 2);
    }

    #[test]
    fn restored_items_wait_after_the_live_ones_and_do_not_count_as_active() {
        let mut q = queue_with(1, &[1]);
        q.restore(SavedQueue {
            batches: vec![SavedBatch {
                request: BatchRequest {
                    collection: "old".into(),
                    collection_cover: None,
                    tracks: vec![TrackJob { track: track(7), index: 7 }],
                    options: DownloadOptions::default(),
                },
                parts: HashMap::from([(7, PathBuf::from("a.mp3.part"))]),
            }],
            ..SavedQueue::default()
        });
        assert_eq!((q.active(), q.restored()), (1, 1));
        let (pending, _) = q.lists();
        assert_eq!(pending, [0, 1]);
        assert_eq!(q.items[1].part.as_deref(), Some(Path::new("a.mp3.part")));
        assert!(q.lookup.contains_key(&(RESTORED_BASE, 7)));

        let requests = q.restored_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].1.collection, "old");

        q.rebatch(RESTORED_BASE, 5);
        assert_eq!((q.active(), q.restored()), (2, 0));
        assert!(q.lookup.contains_key(&(5, 7)));
        assert!(q.batches.contains_key(&5) && !q.batches.contains_key(&RESTORED_BASE));
    }

    #[test]
    fn only_unfinished_work_and_resumable_failures_are_saved() {
        let mut q = queue_with(1, &[1, 2, 3, 4, 5]);
        q.items[0].state = QState::Working(Stage::Downloading);
        q.items[0].part = Some(PathBuf::from("one.part"));
        q.finish(1, QState::Done);
        q.finish(2, QState::Failed(FailKind::Network));
        q.items[2].part = Some(PathBuf::from("three.part"));
        q.finish(3, QState::Failed(FailKind::VipRequired));
        // Item 5 is still queued.

        let saved = q.to_saved();
        assert_eq!(saved.batches.len(), 1);
        let ids: Vec<u64> = saved.batches[0].request.tracks.iter().map(|j| j.track.id).collect();
        assert_eq!(ids, [1, 3, 5], "unfinished, and failed with a partial file; not done, not failed for good");
        assert_eq!(saved.batches[0].parts.len(), 2);
        assert_eq!(saved.batches[0].request.collection, "list");
    }

    #[test]
    fn saving_and_restoring_round_trips() {
        let mut q = queue_with(3, &[10, 11]);
        q.items[1].part = Some(PathBuf::from("x.part"));
        let mut again = Queue::default();
        again.restore(q.to_saved());
        assert_eq!(again.restored(), 2);
        assert_eq!(again.items[1].track.id, 11);
        assert_eq!(again.items[1].part.as_deref(), Some(Path::new("x.part")));
    }

    #[test]
    fn clearing_keeps_unfinished_items_and_reindexes() {
        let mut q = queue_with(1, &[1, 2, 3]);
        q.finish(0, QState::Done);
        q.finish(2, QState::Cancelled);
        q.clear_finished();
        assert_eq!(q.items.len(), 1);
        assert_eq!(q.lookup.get(&(1, 2)), Some(&0));
        assert!(q.dirty);
    }
}

// ----------------------------------------------------------------------------- login

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LoginTab {
    Qr,
    Phone,
    Cookie,
}

#[derive(Clone, PartialEq)]
pub enum QrPhase {
    Starting,
    Waiting,
    Scanned(String),
    Expired,
    Failed(String),
}

pub struct QrUi {
    pub phase: QrPhase,
    /// QR modules, `size * size`, row-major.
    pub modules: Vec<bool>,
    pub size: usize,
    /// Stops the polling task when set.
    pub cancel: Arc<AtomicBool>,
    pub generation: u64,
}

impl QrUi {
    pub fn new() -> Self {
        Self { phase: QrPhase::Starting, modules: Vec::new(), size: 0, cancel: Arc::new(AtomicBool::new(false)), generation: 0 }
    }
}

pub struct LoginUi {
    pub open: bool,
    pub tab: LoginTab,
    pub qr: QrUi,
    pub phone: String,
    pub country: String,
    pub secret: String,
    pub use_captcha: bool,
    pub sending: bool,
    pub busy: bool,
    pub error: Option<String>,
    pub cookie: String,
}

impl Default for LoginUi {
    fn default() -> Self {
        Self {
            open: false,
            tab: LoginTab::Qr,
            qr: QrUi::new(),
            phone: String::new(),
            country: "86".into(),
            secret: String::new(),
            use_captcha: false,
            sending: false,
            busy: false,
            error: None,
            cookie: String::new(),
        }
    }
}

// ------------------------------------------------------------------------ misc widgets

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub born: Instant,
}

#[derive(Default)]
pub struct Toasts(pub VecDeque<Toast>);

impl Toasts {
    pub fn push(&mut self, kind: ToastKind, text: impl Into<String>) {
        if self.0.len() >= 5 {
            self.0.pop_front();
        }
        self.0.push_back(Toast { text: text.into(), kind, born: Instant::now() });
    }
}

#[derive(Default)]
pub struct PassphraseUi {
    /// Unlock dialog at start-up.
    pub unlock_open: bool,
    pub unlock_input: String,
    pub unlock_error: Option<String>,
    /// Set / change dialog in the settings page.
    pub set_open: bool,
    pub set_input: String,
    pub set_confirm: String,
    pub set_error: Option<String>,
}

pub enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available(Box<ncm_update::UpdateInfo>),
    Downloading { done: u64, total: u64 },
    Failed(String),
}

pub fn account_label(a: &Account) -> &'static str {
    match a.vip_type {
        11 => "SVIP",
        10 => "VIP",
        t if t > 0 => "VIP",
        _ => "普通用户",
    }
}

/// Track state after a login change: keeps the UI honest about VIP status.
pub fn is_vip(a: &Account) -> bool {
    a.vip_type > 0
}
