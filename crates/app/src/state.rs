//! Plain data shared by the UI pages.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use eframe::egui::TextureHandle;
use ncm_api::{Account, Collection, PlaylistSummary, ResourceKind, Track};
use ncm_core::{BatchId, BatchSummary, FailKind, Stage};

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
}

pub struct BatchInfo {
    pub summary: Option<BatchSummary>,
}

#[derive(Default)]
pub struct Queue {
    pub items: Vec<QItem>,
    pub lookup: HashMap<(BatchId, u64), usize>,
    pub batches: HashMap<BatchId, BatchInfo>,
}

impl Queue {
    pub fn active(&self) -> usize {
        self.items.iter().filter(|i| !i.state.is_finished()).count()
    }

    pub fn count(&self, f: impl Fn(&QState) -> bool) -> usize {
        self.items.iter().filter(|i| f(&i.state)).count()
    }

    pub fn clear_finished(&mut self) {
        self.items.retain(|i| !i.state.is_finished());
        self.lookup = self.items.iter().enumerate().map(|(n, i)| ((i.batch, i.track.id), n)).collect();
        let live: std::collections::HashSet<BatchId> = self.items.iter().map(|i| i.batch).collect();
        self.batches.retain(|id, _| live.contains(id));
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
