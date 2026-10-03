//! The download engine: turns a batch of tracks into finished, tagged files.
//!
//! Per track: resolve URL → chunked download into `<name>.<ext>.part` → verify size/MD5 →
//! (meanwhile fetch lyrics + cover) → tag → atomic rename → sidecar `.lrc` / `.txt` / cover files.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use md5::{Digest, Md5};
use ncm_api::{Availability, Client, Level, SongUrl, Track};
use tokio::runtime::Handle;
use tokio::sync::{OnceCell, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::adaptive::{Gate, Limiter, NetSnapshot};
use crate::fetch::{self, FetchError, FetchRequest, Fetcher, Resume, build_http_client};
use crate::lyrics::{self, LyricsFile, PreparedLyrics};
use crate::naming::{self, NameContext};
use crate::options::{CoverFile, DownloadOptions, ExistsPolicy};
use crate::resolver::UrlResolver;
use crate::tagging::{self, ImageKind, TagData};
use crate::tuning as t;

pub type BatchId = u64;

// ------------------------------------------------------------------------------- public API

#[derive(Debug, Clone)]
pub struct TrackJob {
    pub track: Track,
    /// 1-based position in the source collection (for `{index}`).
    pub index: usize,
}

#[derive(Debug, Clone)]
pub struct BatchRequest {
    /// Name of the playlist / album (used for the folder and `{playlist}`).
    pub collection: String,
    pub collection_cover: Option<String>,
    pub tracks: Vec<TrackJob>,
    pub options: DownloadOptions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Resolving,
    Downloading,
    Verifying,
    Tagging,
    Finishing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailKind {
    /// The song needs a VIP subscription or a login.
    VipRequired,
    /// No copyright / taken down / no audio source.
    Unavailable,
    /// Only a lower quality is available and the options forbid it.
    Quality,
    Network,
    Disk,
    Other,
}

#[derive(Debug, Clone)]
pub enum TrackUpdate {
    Stage(Stage),
    /// What the server will deliver (may be lower than requested).
    Delivered {
        level: String,
        ext: String,
        size: u64,
    },
    /// The partial file the track is downloaded into. It stays behind when the download is
    /// interrupted, so that it can be continued later.
    PartFile(PathBuf),
    Progress {
        done: u64,
        total: u64,
    },
    Done {
        path: PathBuf,
        bytes: u64,
        level: String,
        warnings: Vec<String>,
    },
    Skipped {
        existing: PathBuf,
    },
    Failed {
        kind: FailKind,
        message: String,
    },
    Cancelled,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BatchSummary {
    pub total: usize,
    pub done: usize,
    pub skipped: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub enum Event {
    Track {
        batch: BatchId,
        id: u64,
        update: TrackUpdate,
    },
    Net(NetSnapshot),
    BatchFinished {
        batch: BatchId,
        summary: BatchSummary,
    },
    /// Downloading was paused (`true`) or continued (`false`).
    Paused(bool),
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("cannot set up networking: {0}")]
    Http(#[from] reqwest::Error),
}

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

struct Inner {
    api: Client,
    http: reqwest::Client,
    fetcher: Fetcher,
    limiter: Arc<Limiter>,
    resolver: UrlResolver,
    tx: mpsc::UnboundedSender<Event>,
    root: CancellationToken,
    batches: Mutex<HashMap<BatchId, CancellationToken>>,
    next_batch: AtomicU64,
    rt: Handle,
}

impl Engine {
    /// Must be called with a tokio runtime available through `rt`. Events (progress, results,
    /// network stats) arrive on the returned receiver.
    pub fn new(rt: &Handle, api: Client, proxy: Option<&str>) -> Result<(Self, mpsc::UnboundedReceiver<Event>), EngineError> {
        let _guard = rt.enter();
        let http = build_http_client(proxy)?;
        let root = CancellationToken::new();
        let limiter = Limiter::new();
        let fetcher = Fetcher::new(http.clone(), limiter.clone(), root.clone());
        let (tx, rx) = mpsc::unbounded_channel();

        rt.spawn(limiter.clone().run_controller(root.clone()));
        {
            let (limiter, tx, root) = (limiter.clone(), tx.clone(), root.clone());
            rt.spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_millis(500));
                loop {
                    tokio::select! {
                        _ = root.cancelled() => return,
                        _ = tick.tick() => {
                            if tx.send(Event::Net(limiter.snapshot())).is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }

        let inner = Inner {
            resolver: UrlResolver::new(api.clone()),
            api,
            http,
            fetcher,
            limiter,
            tx,
            root,
            batches: Mutex::new(HashMap::new()),
            next_batch: AtomicU64::new(1),
            rt: rt.clone(),
        };
        Ok((Self { inner: Arc::new(inner) }, rx))
    }

    pub fn api(&self) -> &Client {
        &self.inner.api
    }

    pub fn net_snapshot(&self) -> NetSnapshot {
        self.inner.limiter.snapshot()
    }

    /// Queue a batch; returns immediately.
    pub fn submit(&self, req: BatchRequest) -> BatchId {
        let id = self.inner.next_batch.fetch_add(1, Ordering::Relaxed);
        let token = self.inner.root.child_token();
        self.inner.batches.lock().unwrap().insert(id, token.clone());
        let inner = self.inner.clone();
        self.inner.rt.spawn(async move {
            inner.clone().run_batch(id, req, token).await;
            inner.batches.lock().unwrap().remove(&id);
        });
        id
    }

    /// Pause or continue all downloads. A paused engine starts no new request or track; tracks
    /// that are already being prepared finish what does not need the network.
    pub fn set_paused(&self, paused: bool) {
        self.inner.fetcher.set_paused(paused);
        let _ = self.inner.tx.send(Event::Paused(paused));
    }

    pub fn is_paused(&self) -> bool {
        self.inner.fetcher.is_paused()
    }

    /// Cancel a batch. What was downloaded of its unfinished tracks is discarded.
    pub fn cancel_batch(&self, id: BatchId) {
        if let Some(token) = self.inner.batches.lock().unwrap().get(&id) {
            token.cancel();
        }
    }

    /// Cancel every batch (see [`Engine::cancel_batch`]).
    pub fn cancel_all(&self) {
        for token in self.inner.batches.lock().unwrap().values() {
            token.cancel();
        }
    }

    /// Stop all background tasks; the engine cannot be used afterwards. Unlike a cancel, this
    /// keeps the partial files, so the downloads can be continued by a later engine.
    pub fn shutdown(&self) {
        self.inner.root.cancel();
    }
}

// ------------------------------------------------------------------------------- internals

/// Cover bytes for one URL, downloaded at most once per batch (`None` when the download failed).
type CoverCell = Arc<OnceCell<Option<Arc<Vec<u8>>>>>;

struct BatchCtx {
    id: BatchId,
    collection: String,
    collection_cover: Option<String>,
    options: DownloadOptions,
    cancel: CancellationToken,
    covers: Mutex<HashMap<String, CoverCell>>,
}

#[derive(Debug)]
enum TrackError {
    Skipped(PathBuf),
    Cancelled,
    Failed { kind: FailKind, message: String },
}

impl TrackError {
    fn failed(kind: FailKind, message: impl Into<String>) -> Self {
        TrackError::Failed { kind, message: message.into() }
    }
}

struct Finished {
    path: PathBuf,
    bytes: u64,
    level: String,
    warnings: Vec<String>,
}

impl Inner {
    fn emit(&self, batch: BatchId, id: u64, update: TrackUpdate) {
        let _ = self.tx.send(Event::Track { batch, id, update });
    }

    async fn run_batch(self: Arc<Self>, id: BatchId, req: BatchRequest, cancel: CancellationToken) {
        let ctx = Arc::new(BatchCtx {
            id,
            collection: req.collection,
            collection_cover: req.collection_cover,
            options: req.options,
            cancel: cancel.clone(),
            covers: Mutex::new(HashMap::new()),
        });

        let run: RunTrack = {
            let (this, ctx) = (self.clone(), ctx.clone());
            Arc::new(move |job, upcoming| {
                let (this, ctx) = (this.clone(), ctx.clone());
                async move { this.run_track(&ctx, &job, &upcoming).await }.boxed()
            })
        };
        let emit: Emit = {
            let this = self.clone();
            Arc::new(move |track, update| this.emit(id, track, update))
        };
        let resumed: Resumed = {
            let fetcher = self.fetcher.clone();
            Arc::new(move || {
                let fetcher = fetcher.clone();
                async move { fetcher.wait_resumed().await }.boxed()
            })
        };
        let summary = schedule(req.tracks, self.limiter.files.clone(), cancel, resumed, run, emit).await;
        let _ = self.tx.send(Event::BatchFinished { batch: id, summary });
    }

    async fn run_track(&self, ctx: &Arc<BatchCtx>, job: &TrackJob, upcoming: &[u64]) -> Result<Finished, TrackError> {
        let opts = &ctx.options;
        let track = &job.track;
        let batch = ctx.id;
        let stage = |s: Stage| self.emit(batch, track.id, TrackUpdate::Stage(s));
        if ctx.cancel.is_cancelled() {
            return Err(TrackError::Cancelled);
        }
        stage(Stage::Resolving);

        let requested = opts.level.as_str();
        let dir = naming::target_dir(opts.layout, &opts.output_dir, &name_ctx(job, &ctx.collection, requested));

        // Cheap early exit that saves an API call.
        if opts.on_exists == ExistsPolicy::Skip {
            let pre = naming::render_template(&opts.filename_template, &name_ctx(job, &ctx.collection, requested));
            if let Some(existing) = naming::find_existing(&dir, &pre) {
                return Err(TrackError::Skipped(existing));
            }
        }

        let mut song = self.resolve_song(ctx, track, upcoming, false).await?;
        let ext = pick_extension(&song);
        let level = song.level.clone().unwrap_or_else(|| requested.to_owned());
        self.emit(batch, track.id, TrackUpdate::Delivered { level: level.clone(), ext: ext.clone(), size: song.size });

        let base = naming::render_template(&opts.filename_template, &name_ctx(job, &ctx.collection, &level));
        let base = match opts.on_exists {
            ExistsPolicy::Skip => {
                if let Some(existing) = naming::find_existing(&dir, &base) {
                    return Err(TrackError::Skipped(existing));
                }
                base
            }
            ExistsPolicy::Overwrite => base,
            ExistsPolicy::KeepBoth => naming::unique_base(&dir, &base),
        };
        let final_path = dir.join(format!("{base}.{ext}"));
        let part_path = dir.join(format!("{base}.{ext}.part"));

        // Lyrics and cover are fetched while the audio downloads.
        let lyrics_task = (opts.embed_lyrics || opts.lyrics_file != LyricsFile::Off).then(|| {
            let api = self.api.clone();
            let (id, extra) = (track.id, opts.lyrics_extra);
            tokio::spawn(async move { fetch_lyrics(&api, id, extra).await })
        });
        let cover_url = track.album.pic_url.as_deref().map(|u| opts.cover_size.apply(u));
        let want_cover = opts.embed_cover || opts.cover_file != CoverFile::Off;

        self.emit(batch, track.id, TrackUpdate::PartFile(part_path.clone()));

        // What an earlier run left behind (a crash, a failure, a quit) is continued. It is checked
        // like any other download, and a file that does not verify is fetched once more from
        // scratch before the track is given up.
        let resume_key = format!("{}:{}", track.id, song.md5.as_deref().unwrap_or_default());
        let mut reuse = fetch::has_resume_data(&part_path);
        stage(Stage::Downloading);
        let bytes = loop {
            let resume = Resume { key: resume_key.clone(), reuse };
            let n = match self.download(ctx, track, &mut song, &part_path, upcoming, resume).await {
                Ok(n) => n,
                Err(e) => {
                    if let Some(h) = lyrics_task {
                        h.abort();
                    }
                    // Only a cancel by the user throws the partial file away. A quit (the engine's
                    // root token) and a failure keep it, so the download can be continued.
                    if matches!(e, TrackError::Cancelled) && !self.root.is_cancelled() {
                        fetch::discard_partial(&part_path);
                    }
                    return Err(e);
                }
            };

            stage(Stage::Verifying);
            let (verify_path, size, md5) = (part_path.clone(), song.size, song.md5.clone());
            let verified = tokio::task::spawn_blocking(move || verify_file(&verify_path, size, md5.as_deref())).await;
            match verified {
                Ok(Ok(())) => break n,
                Ok(Err(msg)) => {
                    fetch::discard_partial(&part_path);
                    if reuse {
                        tracing::warn!("{} did not verify after a resume ({msg}); downloading it again", track.id);
                        reuse = false;
                        stage(Stage::Downloading);
                        continue;
                    }
                    return Err(TrackError::failed(FailKind::Network, msg));
                }
                Err(e) => return Err(TrackError::failed(FailKind::Other, e.to_string())),
            }
        };

        let mut warnings = Vec::new();
        let prepared = match lyrics_task {
            Some(h) => match h.await {
                Ok(Ok(p)) => p,
                Ok(Err(e)) => {
                    warnings.push(format!("歌词获取失败：{e}"));
                    None
                }
                Err(_) => None,
            },
            None => None,
        };
        let cover = match (&cover_url, want_cover) {
            (Some(url), true) => {
                let c = self.cover_bytes(ctx, url).await;
                if c.is_none() {
                    warnings.push("封面下载失败".into());
                }
                c
            }
            _ => None,
        };

        if opts.embed_tags {
            stage(Stage::Tagging);
            let embed_text = prepared
                .as_ref()
                .filter(|_| opts.embed_lyrics)
                .map(|p| if opts.embed_timeline { p.lrc.clone().unwrap_or_else(|| p.plain.clone()) } else { p.plain.clone() });
            let comment = opts.source_comment.then(|| format!("https://music.163.com/song?id={}", track.id));
            let (path, track_c, cover_c) = (part_path.clone(), track.clone(), cover.clone().filter(|_| opts.embed_cover));
            let tag_result = tokio::task::spawn_blocking(move || {
                tagging::write_tags(
                    &path,
                    &TagData {
                        track: &track_c,
                        album_artist: None,
                        lyrics: embed_text.as_deref(),
                        cover: cover_c.as_deref().map(|v| v.as_slice()),
                        comment: comment.as_deref(),
                    },
                )
            })
            .await;
            match tag_result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => warnings.push(format!("写入标签失败：{e}")),
                Err(e) => warnings.push(format!("写入标签失败：{e}")),
            }
        }

        stage(Stage::Finishing);
        tokio::fs::rename(&part_path, &final_path)
            .await
            .map_err(|e| TrackError::failed(FailKind::Disk, format!("无法保存文件：{e}")))?;
        let _ = tokio::fs::remove_file(fetch::journal_path(&part_path)).await;

        // Sidecar files: never fatal.
        if let Some((ext, text)) = prepared.as_ref().and_then(|p| p.sidecar(opts.lyrics_file))
            && let Err(e) = tokio::fs::write(final_path.with_extension(ext), text).await
        {
            warnings.push(format!("保存 .{ext} 歌词失败：{e}"));
        }
        if let Err(e) = self.write_cover_files(ctx, track, &dir, &base, cover.as_deref()).await {
            warnings.push(format!("保存封面失败：{e}"));
        }

        Ok(Finished { path: final_path, bytes, level, warnings })
    }

    async fn resolve_song(&self, ctx: &BatchCtx, track: &Track, upcoming: &[u64], fresh: bool) -> Result<SongUrl, TrackError> {
        let level = ctx.options.level;
        let mut song = tokio::select! {
            r = self.resolver.resolve(level, track.id, upcoming, fresh) => r.map_err(|e| TrackError::failed(FailKind::Network, format!("获取下载地址失败：{e}")))?,
            _ = ctx.cancel.cancelled() => return Err(TrackError::Cancelled),
        };
        if !song.is_usable() {
            // The client's dedicated download endpoint sometimes succeeds where streaming only
            // offers a preview.
            if let Ok(Some(d)) = self.api.download_url(track.id, level).await
                && d.is_usable()
            {
                song = d;
            }
        }
        if !song.is_usable() {
            let logged_in = self.api.is_logged_in();
            return Err(if song.is_trial || track.availability() == Availability::VipRequired || track.is_vip_only() {
                let hint = if logged_in { "需要 VIP 会员" } else { "需要登录（VIP 歌曲）" };
                TrackError::failed(FailKind::VipRequired, hint)
            } else if track.availability() == Availability::PurchaseRequired {
                TrackError::failed(FailKind::VipRequired, "需要单独购买")
            } else {
                TrackError::failed(FailKind::Unavailable, format!("暂无可用音源（无版权或已下架，code {}）", song.code))
            });
        }
        if !ctx.options.allow_lower_quality
            && let Some(actual) = song.level.as_deref().and_then(Level::parse)
            && actual < level
        {
            return Err(TrackError::failed(
                FailKind::Quality,
                format!("没有「{}」音质，最高仅有「{}」", level.label(), actual.label()),
            ));
        }
        Ok(song)
    }

    async fn download(
        &self,
        ctx: &BatchCtx,
        track: &Track,
        song: &mut SongUrl,
        dest: &Path,
        upcoming: &[u64],
        resume: Resume,
    ) -> Result<u64, TrackError> {
        for attempt in 0..2 {
            let (tx, batch, id) = (self.tx.clone(), ctx.id, track.id);
            let req = FetchRequest {
                url: song.url.clone().expect("usable song has a url"),
                size_hint: (song.size > 0).then_some(song.size),
                dest: dest.to_path_buf(),
                progress: Arc::new(move |done, total| {
                    let _ = tx.send(Event::Track { batch, id, update: TrackUpdate::Progress { done, total } });
                }),
                cancel: ctx.cancel.clone(),
                // After an expired link the pieces fetched so far are good, whatever was asked.
                resume: Some(Resume { reuse: resume.reuse || attempt > 0, ..resume.clone() }),
            };
            match self.fetcher.download(req).await {
                Ok(n) => return Ok(n),
                Err(FetchError::Cancelled) => return Err(TrackError::Cancelled),
                Err(FetchError::Forbidden(_)) if attempt == 0 => {
                    // The link expired while it sat in the queue: get a fresh one and retry.
                    *song = self.resolve_song(ctx, track, upcoming, true).await?;
                }
                Err(FetchError::Io(e)) => return Err(TrackError::failed(FailKind::Disk, format!("磁盘错误：{e}"))),
                Err(e) => return Err(TrackError::failed(FailKind::Network, format!("下载失败：{e}"))),
            }
        }
        Err(TrackError::failed(FailKind::Network, "下载链接持续失效"))
    }

    /// Cover bytes for `url`, downloaded once per batch.
    async fn cover_bytes(&self, ctx: &BatchCtx, url: &str) -> Option<Arc<Vec<u8>>> {
        let cell = ctx.covers.lock().unwrap().entry(url.to_owned()).or_insert_with(|| Arc::new(OnceCell::new())).clone();
        cell.get_or_init(|| async {
            for attempt in 0..3 {
                match self.http.get(url).timeout(Duration::from_secs(20)).send().await {
                    Ok(r) if r.status().is_success() => {
                        if let Ok(b) = r.bytes().await
                            && b.len() > 512
                            && ImageKind::detect(&b).is_some()
                        {
                            return Some(Arc::new(b.to_vec()));
                        }
                    }
                    Ok(r) if r.status().is_client_error() => return None,
                    _ => {}
                }
                tokio::time::sleep(Duration::from_millis(400 * (attempt + 1))).await;
            }
            None
        })
        .await
        .clone()
    }

    async fn write_cover_files(
        &self,
        ctx: &BatchCtx,
        track: &Track,
        dir: &Path,
        base: &str,
        track_cover: Option<&Vec<u8>>,
    ) -> std::io::Result<()> {
        use crate::naming::Layout;
        match ctx.options.cover_file {
            CoverFile::Off => Ok(()),
            CoverFile::PerTrack => match track_cover.and_then(|c| ImageKind::detect(c).map(|k| (c, k))) {
                Some((bytes, kind)) => tokio::fs::write(dir.join(format!("{base}.{}", kind.extension())), bytes).await,
                None => Ok(()),
            },
            CoverFile::Folder => {
                let bytes: Option<Arc<Vec<u8>>> = match ctx.options.layout {
                    Layout::Flat => None,
                    Layout::Collection => match &ctx.collection_cover {
                        Some(u) => self.cover_bytes(ctx, &ctx.options.cover_size.apply(u)).await,
                        None => None,
                    },
                    Layout::ArtistAlbum => match &track.album.pic_url {
                        Some(u) => self.cover_bytes(ctx, &ctx.options.cover_size.apply(u)).await,
                        None => None,
                    },
                };
                let Some(bytes) = bytes else { return Ok(()) };
                let Some(kind) = ImageKind::detect(&bytes) else { return Ok(()) };
                let target = dir.join(format!("cover.{}", kind.extension()));
                if target.exists() { Ok(()) } else { tokio::fs::write(target, bytes.as_slice()).await }
            }
        }
    }
}

/// Runs one track; `upcoming` are the ids that follow it (their URLs are resolved together).
type RunTrack = Arc<dyn Fn(TrackJob, Vec<u64>) -> BoxFuture<'static, Result<Finished, TrackError>> + Send + Sync>;
/// Reports an update for the track with the given id.
type Emit = Arc<dyn Fn(u64, TrackUpdate) + Send + Sync>;
/// Completes when downloading is not paused.
type Resumed = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

/// How one track ended, for the batch summary.
enum Outcome {
    Done(u64),
    Skipped,
    Cancelled,
    Failed,
}

/// Start the tracks in order, one per free start slot, and report each result the moment its
/// task ends. Reporting from the task itself matters: a result collected only after the last
/// track has been started would leave early tracks looking busy until the whole batch is under way.
async fn schedule(
    tracks: Vec<TrackJob>,
    slots: Arc<Gate>,
    cancel: CancellationToken,
    resumed: Resumed,
    run: RunTrack,
    emit: Emit,
) -> BatchSummary {
    let all_ids: Vec<u64> = tracks.iter().map(|j| j.track.id).collect();
    let mut summary = BatchSummary { total: tracks.len(), ..BatchSummary::default() };
    let mut tasks: JoinSet<Outcome> = JoinSet::new();
    let mut pending = tracks.into_iter().enumerate();

    // A start slot is only handed out when the network can use it, and not while paused.
    for (pos, job) in pending.by_ref() {
        let slot = tokio::select! {
            s = async {
                resumed().await;
                slots.acquire().await
            } => s,
            _ = cancel.cancelled() => {
                emit(job.track.id, TrackUpdate::Cancelled);
                summary.cancelled += 1;
                break;
            }
        };
        let upcoming: Vec<u64> = all_ids[pos + 1..].iter().take(t::URL_BATCH).copied().collect();
        let (run, emit) = (run.clone(), emit.clone());
        tasks.spawn(async move {
            let track_id = job.track.id;
            let outcome = report(&emit, track_id, run(job, upcoming).await);
            drop(slot);
            outcome
        });
    }
    for (_, job) in pending {
        emit(job.track.id, TrackUpdate::Cancelled);
        summary.cancelled += 1;
    }

    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(Outcome::Done(bytes)) => {
                summary.done += 1;
                summary.bytes += bytes;
            }
            Ok(Outcome::Skipped) => summary.skipped += 1,
            Ok(Outcome::Cancelled) => summary.cancelled += 1,
            Ok(Outcome::Failed) => summary.failed += 1,
            Err(_) => {}
        }
    }
    summary
}

/// Emit the final update of a track.
fn report(emit: &Emit, track_id: u64, result: Result<Finished, TrackError>) -> Outcome {
    match result {
        Ok(f) => {
            let bytes = f.bytes;
            emit(track_id, TrackUpdate::Done { path: f.path, bytes, level: f.level, warnings: f.warnings });
            Outcome::Done(bytes)
        }
        Err(TrackError::Skipped(existing)) => {
            emit(track_id, TrackUpdate::Skipped { existing });
            Outcome::Skipped
        }
        Err(TrackError::Cancelled) => {
            emit(track_id, TrackUpdate::Cancelled);
            Outcome::Cancelled
        }
        Err(TrackError::Failed { kind, message }) => {
            tracing::warn!("track {track_id} failed: {message}");
            emit(track_id, TrackUpdate::Failed { kind, message });
            Outcome::Failed
        }
    }
}

fn name_ctx<'a>(job: &'a TrackJob, collection: &'a str, quality: &'a str) -> NameContext<'a> {
    NameContext { track: &job.track, index: Some(job.index), collection, quality }
}

async fn fetch_lyrics(api: &Client, id: u64, extra: lyrics::LyricsExtra) -> Result<Option<PreparedLyrics>, String> {
    let mut last = String::new();
    for attempt in 0..3u64 {
        match api.lyrics(id).await {
            Ok(l) => {
                if l.instrumental {
                    return Ok(None);
                }
                let extra_raw = match extra {
                    lyrics::LyricsExtra::None => None,
                    lyrics::LyricsExtra::Translation => l.translation.as_deref(),
                    lyrics::LyricsExtra::Romaji => l.romaji.as_deref(),
                };
                return Ok(lyrics::prepare(l.lrc.as_deref(), extra_raw, extra));
            }
            Err(e) if e.is_transient() || e.is_throttled() => {
                last = e.to_string();
                tokio::time::sleep(Duration::from_millis(600 * (attempt + 1))).await;
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Err(last)
}

fn pick_extension(song: &SongUrl) -> String {
    if let Some(k) = song.kind.as_deref().filter(|k| k.chars().all(|c| c.is_ascii_alphanumeric()) && !k.is_empty()) {
        return k.to_ascii_lowercase();
    }
    song.url
        .as_deref()
        .and_then(|u| u.split('?').next())
        .and_then(|p| p.rsplit('.').next())
        .filter(|e| naming::AUDIO_EXTENSIONS.contains(e))
        .unwrap_or("mp3")
        .to_owned()
}

/// Check size and, when the service published one, the MD5 of the finished file.
fn verify_file(path: &Path, expected_size: u64, md5: Option<&str>) -> Result<(), String> {
    let len = std::fs::metadata(path).map_err(|e| format!("无法读取文件：{e}"))?.len();
    if expected_size > 0 && len != expected_size {
        return Err(format!("文件大小不符（应为 {expected_size}，实际 {len}）"));
    }
    if let Some(expected) = md5.filter(|m| m.len() == 32) {
        let mut file = std::fs::File::open(path).map_err(|e| format!("无法读取文件：{e}"))?;
        let mut hasher = Md5::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf).map_err(|e| format!("读取文件失败：{e}"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        if !hex::encode(hasher.finalize()).eq_ignore_ascii_case(expected) {
            return Err("文件校验失败（MD5 不匹配），请重试".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: u64) -> TrackJob {
        TrackJob { track: Track { id, name: format!("track {id}"), ..Track::default() }, index: id as usize }
    }

    fn finished() -> Finished {
        Finished { path: PathBuf::from("a.mp3"), bytes: 10, level: "standard".into(), warnings: Vec::new() }
    }

    /// A `Resumed` that never waits.
    fn running() -> Resumed {
        Arc::new(|| async {}.boxed())
    }

    fn label(update: &TrackUpdate) -> &'static str {
        match update {
            TrackUpdate::Done { .. } => "done",
            TrackUpdate::Skipped { .. } => "skipped",
            TrackUpdate::Failed { .. } => "failed",
            TrackUpdate::Cancelled => "cancelled",
            _ => "other",
        }
    }

    #[tokio::test]
    async fn each_result_is_reported_as_soon_as_its_track_ends() {
        // One start slot: with the old "start everything, then collect" loop every `done` line
        // would come after the last `start`.
        let log = Arc::new(Mutex::new(Vec::<String>::new()));
        let run: RunTrack = {
            let log = log.clone();
            Arc::new(move |job, _| {
                let log = log.clone();
                async move {
                    log.lock().unwrap().push(format!("start {}", job.track.id));
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    Ok(finished())
                }
                .boxed()
            })
        };
        let emit: Emit = {
            let log = log.clone();
            Arc::new(move |id, update| log.lock().unwrap().push(format!("{} {id}", label(&update))))
        };
        let summary = schedule((1..=4).map(job).collect(), Gate::new(1), CancellationToken::new(), running(), run, emit).await;
        assert_eq!(*log.lock().unwrap(), ["start 1", "done 1", "start 2", "done 2", "start 3", "done 3", "start 4", "done 4"]);
        assert_eq!((summary.total, summary.done, summary.bytes), (4, 4, 40));
    }

    #[tokio::test]
    async fn summary_counts_every_kind_of_ending() {
        let run: RunTrack = Arc::new(|job, _| {
            async move {
                match job.track.id {
                    1 => Ok(finished()),
                    2 => Err(TrackError::Skipped(PathBuf::from("b.mp3"))),
                    _ => Err(TrackError::failed(FailKind::Network, "boom")),
                }
            }
            .boxed()
        });
        let seen = Arc::new(Mutex::new(Vec::<(u64, &'static str)>::new()));
        let emit: Emit = {
            let seen = seen.clone();
            Arc::new(move |id, update| seen.lock().unwrap().push((id, label(&update))))
        };
        let s = schedule((1..=3).map(job).collect(), Gate::new(3), CancellationToken::new(), running(), run, emit).await;
        assert_eq!((s.total, s.done, s.skipped, s.failed, s.cancelled), (3, 1, 1, 1, 0));
        let mut seen = seen.lock().unwrap().clone();
        seen.sort();
        assert_eq!(seen, [(1, "done"), (2, "skipped"), (3, "failed")]);
    }

    #[tokio::test]
    async fn nothing_starts_while_paused() {
        let (pause, rx) = tokio::sync::watch::channel(true);
        let resumed: Resumed = Arc::new(move || {
            let mut rx = rx.clone();
            async move {
                while *rx.borrow_and_update() {
                    if rx.changed().await.is_err() {
                        return;
                    }
                }
            }
            .boxed()
        });
        let started = Arc::new(Mutex::new(Vec::<u64>::new()));
        let run: RunTrack = {
            let started = started.clone();
            Arc::new(move |job, _| {
                let started = started.clone();
                async move {
                    started.lock().unwrap().push(job.track.id);
                    Ok(finished())
                }
                .boxed()
            })
        };
        let emit: Emit = Arc::new(|_, _| {});
        let batch =
            tokio::spawn(schedule((1..=2).map(job).collect(), Gate::new(2), CancellationToken::new(), resumed, run, emit));

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(started.lock().unwrap().is_empty(), "a paused batch must not start tracks");
        pause.send_replace(false);
        let s = batch.await.unwrap();
        assert_eq!(*started.lock().unwrap(), [1, 2]);
        assert_eq!(s.done, 2);
    }

    #[tokio::test]
    async fn cancelling_reports_the_running_and_the_waiting_tracks() {
        let cancel = CancellationToken::new();
        let run: RunTrack = {
            let cancel = cancel.clone();
            Arc::new(move |_, _| {
                let cancel = cancel.clone();
                async move {
                    cancel.cancelled().await;
                    Err(TrackError::Cancelled)
                }
                .boxed()
            })
        };
        let seen = Arc::new(Mutex::new(Vec::<(u64, &'static str)>::new()));
        let emit: Emit = {
            let seen = seen.clone();
            Arc::new(move |id, update| seen.lock().unwrap().push((id, label(&update))))
        };
        let batch = tokio::spawn(schedule((1..=3).map(job).collect(), Gate::new(1), cancel.clone(), running(), run, emit));
        tokio::time::sleep(Duration::from_millis(30)).await;
        cancel.cancel();
        let s = batch.await.unwrap();
        assert_eq!((s.total, s.done, s.cancelled), (3, 0, 3));
        let mut seen = seen.lock().unwrap().clone();
        seen.sort();
        assert_eq!(seen, [(1, "cancelled"), (2, "cancelled"), (3, "cancelled")]);
    }

    #[test]
    fn extension_prefers_reported_type_then_url() {
        let mut s = SongUrl { kind: Some("FLAC".into()), ..SongUrl::default() };
        assert_eq!(pick_extension(&s), "flac");
        s.kind = None;
        s.url = Some("http://m701.music.126.net/x/abc.m4a?vuutv=1".into());
        assert_eq!(pick_extension(&s), "m4a");
        s.url = Some("http://host/none".into());
        assert_eq!(pick_extension(&s), "mp3");
    }

    #[test]
    fn verify_detects_size_and_hash_problems() {
        let dir = std::env::temp_dir().join(format!("ncm-verify-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.bin");
        std::fs::write(&p, b"abc").unwrap();
        assert!(verify_file(&p, 3, Some("900150983cd24fb0d6963f7d28e17f72")).is_ok());
        assert!(verify_file(&p, 4, None).unwrap_err().contains("大小"));
        assert!(verify_file(&p, 3, Some("00000000000000000000000000000000")).unwrap_err().contains("MD5"));
        assert!(verify_file(&p, 0, None).is_ok());
        std::fs::remove_dir_all(&dir).ok();
    }
}
