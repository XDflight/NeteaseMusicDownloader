//! Chunked, range-based file fetching driven by the adaptive connection gate.
//!
//! Files are split into `CHUNK_SIZE` pieces which go into one global FIFO. A fixed pool of
//! workers pulls pieces, but each piece first needs a permit from the [`Limiter`] gate, so the
//! number of requests in flight follows the adaptive limit. FIFO order keeps early files
//! finishing first instead of spreading connections over many half-finished files.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use rand::RngExt;
use reqwest::StatusCode;
use reqwest::header::{CONTENT_RANGE, RANGE};
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::adaptive::{Limiter, Permit};
use crate::tuning as t;

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("cancelled")]
    Cancelled,
    /// The link is no longer valid (expired token or hot-link protection).
    #[error("download link rejected (HTTP {0})")]
    Forbidden(u16),
    #[error("HTTP {0}")]
    Status(u16),
    #[error("network error: {0}")]
    Network(String),
    #[error("connection stalled")]
    Stalled,
    #[error("server does not support range requests")]
    RangeUnsupported,
    #[error("truncated response: expected {expected} bytes, got {actual}")]
    Truncated { expected: u64, actual: u64 },
    #[error("disk error: {0}")]
    Io(String),
    /// Downloading was paused while this request was in flight; it is not a failure.
    #[error("paused")]
    Paused,
}

impl From<io::Error> for FetchError {
    fn from(e: io::Error) -> Self {
        FetchError::Io(e.to_string())
    }
}

impl FetchError {
    fn is_retryable(&self) -> bool {
        match self {
            FetchError::Network(_) | FetchError::Stalled | FetchError::Truncated { .. } => true,
            FetchError::Status(s) => matches!(*s, 408 | 425 | 429 | 500..=599),
            _ => false,
        }
    }

    /// Failures that suggest the path is overloaded or throttled, so the limiter should shrink.
    fn signals_congestion(&self) -> bool {
        match self {
            FetchError::Network(_) | FetchError::Stalled => true,
            FetchError::Status(s) => matches!(*s, 429 | 503),
            FetchError::Forbidden(_) => false,
            _ => false,
        }
    }
}

/// Build the HTTP client used for CDN downloads.
pub fn build_http_client(proxy: Option<&str>) -> reqwest::Result<reqwest::Client> {
    ncm_api::install_tls_provider();
    let mut b = reqwest::Client::builder()
        .connect_timeout(t::CONNECT_TIMEOUT)
        .pool_max_idle_per_host(t::MAX_CONNECTIONS)
        .pool_idle_timeout(Duration::from_secs(20))
        .tcp_nodelay(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        // Same identity as the API client.
        .user_agent(ncm_api::client::USER_AGENT);
    if let Some(p) = proxy.filter(|p| !p.trim().is_empty()) {
        b = b.proxy(reqwest::Proxy::all(p.trim())?);
    }
    b.build()
}

pub type ProgressFn = Arc<dyn Fn(u64, u64) + Send + Sync>;

pub struct FetchRequest {
    pub url: String,
    /// Size announced by the API; probed from the server when `None`.
    pub size_hint: Option<u64>,
    /// Destination (usually a `.part` file); created / truncated by the fetcher.
    pub dest: std::path::PathBuf,
    pub progress: ProgressFn,
    pub cancel: CancellationToken,
    /// Keep a journal of the finished pieces next to `dest` so that an interrupted download can
    /// be continued. `None` downloads without one.
    pub resume: Option<Resume>,
}

#[derive(Debug, Clone)]
pub struct Resume {
    /// Identifies the content (for example track id and MD5). A journal written for other
    /// content is ignored.
    pub key: String,
    /// Continue from an existing journal. `false` starts over (and writes a fresh journal).
    pub reuse: bool,
}

// A journal is a header line followed by one little-endian u32 per finished piece:
//   NMDR1 <piece size> <total size> <key>\n
// The data of a piece is flushed to disk before its record is appended, so a record never
// promises more than the file holds. A half-written last record is ignored.
const JOURNAL_MAGIC: &str = "NMDR1";

/// The file that records which pieces of the partial download `part` are finished.
pub fn journal_path(part: &Path) -> PathBuf {
    let mut p = part.as_os_str().to_owned();
    p.push(".resume");
    PathBuf::from(p)
}

/// Whether `part` is an interrupted download that can be continued.
pub fn has_resume_data(part: &Path) -> bool {
    part.is_file() && journal_path(part).is_file()
}

/// Delete a partial download together with its journal.
pub fn discard_partial(part: &Path) {
    let _ = std::fs::remove_file(part);
    let _ = std::fs::remove_file(journal_path(part));
}

fn journal_header(key: &str, total: u64) -> String {
    format!("{JOURNAL_MAGIC} {} {total} {key}\n", t::CHUNK_SIZE)
}

/// Which pieces `path` records as finished and how many bytes of it are valid, provided the
/// journal belongs to the download described by `header`.
fn read_journal(path: &Path, header: &str, pieces: u64) -> Option<(Vec<bool>, u64)> {
    let data = std::fs::read(path).ok()?;
    let records = data.strip_prefix(header.as_bytes())?;
    // Whole records only; a half-written last one is left out.
    let (finished, _half_written) = records.as_chunks::<4>();
    let mut done = vec![false; pieces as usize];
    for rec in finished {
        if let Some(d) = done.get_mut(u32::from_le_bytes(*rec) as usize) {
            *d = true;
        }
    }
    Some((done, (header.len() + finished.len() * 4) as u64))
}

fn piece_len(index: u64, total: u64) -> u64 {
    (total - index * t::CHUNK_SIZE).min(t::CHUNK_SIZE)
}

/// Record finished pieces in the journal, off the download path. Everything that finished since
/// the last round is recorded together after one flush of the file, so a slow disk can never hold
/// a download back, and a record is only written once the data it stands for is on disk. The task
/// ends when the job (and with it the sender) is gone and the queue is empty.
fn spawn_journal(file: Arc<File>, journal: File) -> mpsc::UnboundedSender<u32> {
    let (tx, mut rx) = mpsc::unbounded_channel::<u32>();
    let journal = Arc::new(Mutex::new(journal));
    tokio::spawn(async move {
        while let Some(first) = rx.recv().await {
            let mut records = first.to_le_bytes().to_vec();
            while let Ok(next) = rx.try_recv() {
                records.extend_from_slice(&next.to_le_bytes());
            }
            let (file, journal) = (file.clone(), journal.clone());
            let written = tokio::task::spawn_blocking(move || -> io::Result<()> {
                file.sync_data()?;
                journal.lock().unwrap().write_all(&records)
            })
            .await;
            if !matches!(written, Ok(Ok(()))) {
                // Only the ability to continue is lost; the download itself is unaffected.
                tracing::warn!("cannot update the resume journal: {written:?}");
                return;
            }
        }
    });
    tx
}

struct FileJob {
    url: String,
    total: u64,
    file: Arc<File>,
    /// Pieces that still have to be fetched, in ascending order.
    pending: Vec<u64>,
    next: AtomicU64,
    remaining: AtomicU64,
    downloaded: AtomicU64,
    reported: AtomicU64,
    failed: AtomicBool,
    done: Mutex<Option<oneshot::Sender<Result<(), FetchError>>>>,
    progress: ProgressFn,
    cancel: CancellationToken,
    /// Finished pieces go to the journal task (see [`spawn_journal`]).
    journal: Option<mpsc::UnboundedSender<u32>>,
}

impl FileJob {
    fn fail(&self, e: FetchError) {
        if !self.failed.swap(true, Ordering::SeqCst)
            && let Some(tx) = self.done.lock().unwrap().take()
        {
            let _ = tx.send(Err(e));
        }
    }

    fn succeed(&self) {
        if let Some(tx) = self.done.lock().unwrap().take() {
            let _ = tx.send(Ok(()));
        }
    }

    fn add_progress(&self, n: u64) {
        let now = self.downloaded.fetch_add(n, Ordering::Relaxed) + n;
        let last = self.reported.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= 64 * 1024 || now >= self.total {
            self.reported.store(now, Ordering::Relaxed);
            (self.progress)(now.min(self.total), self.total);
        }
    }

    fn sub_progress(&self, n: u64) {
        self.downloaded.fetch_sub(n, Ordering::Relaxed);
    }

    /// Tell the listener where things stand now (after progress was taken back).
    fn report_now(&self) {
        let now = self.downloaded.load(Ordering::Relaxed);
        self.reported.store(now, Ordering::Relaxed);
        (self.progress)(now.min(self.total), self.total);
    }
}

struct Shared {
    http: reqwest::Client,
    limiter: Arc<Limiter>,
    queue: Mutex<VecDeque<Arc<FileJob>>>,
    wake: Notify,
    cancel: CancellationToken,
    pause: watch::Sender<bool>,
}

/// Cheap handle; workers stop when `cancel` (given to [`Fetcher::new`]) fires.
#[derive(Clone)]
pub struct Fetcher {
    shared: Arc<Shared>,
}

impl Fetcher {
    pub fn new(http: reqwest::Client, limiter: Arc<Limiter>, cancel: CancellationToken) -> Self {
        let shared = Arc::new(Shared {
            http,
            limiter,
            queue: Mutex::new(VecDeque::new()),
            wake: Notify::new(),
            cancel,
            pause: watch::Sender::new(false),
        });
        for _ in 0..t::MAX_CONNECTIONS {
            tokio::spawn(shared.clone().worker());
        }
        Self { shared }
    }

    pub fn limiter(&self) -> &Arc<Limiter> {
        &self.shared.limiter
    }

    /// Stop (or continue) transferring. While paused no new request is started and requests in
    /// flight are dropped: a piece is only written once it is complete, so the part of it that had
    /// arrived is simply fetched again after the resume.
    pub fn set_paused(&self, paused: bool) {
        self.shared.pause.send_replace(paused);
    }

    pub fn is_paused(&self) -> bool {
        *self.shared.pause.borrow()
    }

    /// Returns once downloading is not paused.
    pub async fn wait_resumed(&self) {
        self.shared.wait_resumed().await;
    }

    /// Download `req.url` into `req.dest`. Falls back to a single stream when the server does
    /// not honour `Range`.
    pub async fn download(&self, req: FetchRequest) -> Result<u64, FetchError> {
        let total = match req.size_hint.filter(|s| *s > 0) {
            Some(s) => s,
            None => match self.probe_size(&req.url, &req.cancel).await? {
                Some(s) => s,
                None => return self.stream_download(&req).await,
            },
        };

        let pieces = total.div_ceil(t::CHUNK_SIZE);
        let prepared = prepare_file(&req.dest, total, pieces, req.resume.as_ref())?;
        let pending: Vec<u64> = (0..pieces).filter(|&i| !prepared.done[i as usize]).collect();
        let have: u64 = (0..pieces).filter(|&i| prepared.done[i as usize]).map(|i| piece_len(i, total)).sum();
        if have > 0 {
            tracing::info!("continuing {}: {have} of {total} bytes are already on disk", req.dest.display());
            (req.progress)(have, total);
        }
        if pending.is_empty() {
            return Ok(total);
        }
        let (tx, rx) = oneshot::channel();
        let file = Arc::new(prepared.file);
        let job = Arc::new(FileJob {
            url: req.url.clone(),
            total,
            file: file.clone(),
            remaining: AtomicU64::new(pending.len() as u64),
            pending,
            next: AtomicU64::new(0),
            downloaded: AtomicU64::new(have),
            reported: AtomicU64::new(have),
            failed: AtomicBool::new(false),
            done: Mutex::new(Some(tx)),
            progress: req.progress.clone(),
            cancel: req.cancel.clone(),
            journal: prepared.journal.map(|j| spawn_journal(file, j)),
        });
        self.shared.queue.lock().unwrap().push_back(job.clone());
        self.shared.wake.notify_waiters();

        let outcome = tokio::select! {
            r = rx => r.unwrap_or(Err(FetchError::Cancelled)),
            _ = req.cancel.cancelled() => Err(FetchError::Cancelled),
            _ = self.shared.cancel.cancelled() => Err(FetchError::Cancelled),
        };
        match outcome {
            Ok(()) => Ok(total),
            Err(FetchError::RangeUnsupported) => {
                tracing::debug!("range unsupported, streaming {}", req.url);
                job.failed.store(true, Ordering::SeqCst);
                // The whole file is rewritten, so the pieces recorded so far mean nothing.
                let _ = std::fs::remove_file(journal_path(&req.dest));
                self.stream_download(&req).await
            }
            Err(e) => {
                job.failed.store(true, Ordering::SeqCst);
                Err(e)
            }
        }
    }

    /// Ask the server for the total size with a 1-byte range request.
    /// `Ok(None)` means ranges are not supported.
    async fn probe_size(&self, url: &str, cancel: &CancellationToken) -> Result<Option<u64>, FetchError> {
        let _permit = tokio::select! {
            p = self.shared.limiter.gate.acquire() => p,
            _ = cancel.cancelled() => return Err(FetchError::Cancelled),
        };
        let resp = tokio::time::timeout(t::STALL_TIMEOUT, self.shared.http.get(url).header(RANGE, "bytes=0-0").send())
            .await
            .map_err(|_| FetchError::Stalled)?
            .map_err(|e| FetchError::Network(e.to_string()))?;
        match resp.status() {
            StatusCode::PARTIAL_CONTENT => Ok(resp
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit('/').next())
                .and_then(|n| n.parse().ok())),
            StatusCode::OK => Ok(None),
            s if s == StatusCode::FORBIDDEN || s == StatusCode::UNAUTHORIZED => Err(FetchError::Forbidden(s.as_u16())),
            s => Err(FetchError::Status(s.as_u16())),
        }
    }

    /// Whole file over one connection (used when ranges are unsupported).
    async fn stream_download(&self, req: &FetchRequest) -> Result<u64, FetchError> {
        let max_attempts = t::MAX_RETRIES.min(3);
        let mut attempt = 0;
        loop {
            if req.cancel.is_cancelled() {
                return Err(FetchError::Cancelled);
            }
            self.shared.resume_or_cancel(&req.cancel).await?;
            let permit = tokio::select! {
                p = self.shared.limiter.gate.acquire() => p,
                _ = req.cancel.cancelled() => return Err(FetchError::Cancelled),
            };
            let result = self.stream_once(req).await;
            drop(permit);
            match result {
                Ok(n) => return Ok(n),
                // A single stream cannot continue where it stopped: it starts over after the resume.
                Err(FetchError::Paused) => {}
                Err(e) if e.is_retryable() && attempt < max_attempts => {
                    if e.signals_congestion() {
                        self.shared.limiter.report_error();
                    }
                    sleep_or_cancel(backoff(attempt), &req.cancel).await?;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn stream_once(&self, req: &FetchRequest) -> Result<u64, FetchError> {
        use tokio::io::AsyncWriteExt;
        let resp = tokio::time::timeout(t::STALL_TIMEOUT, self.shared.http.get(&req.url).send())
            .await
            .map_err(|_| FetchError::Stalled)?
            .map_err(|e| FetchError::Network(e.to_string()))?;
        let status = resp.status();
        if status == StatusCode::FORBIDDEN || status == StatusCode::UNAUTHORIZED {
            return Err(FetchError::Forbidden(status.as_u16()));
        }
        if !status.is_success() {
            return Err(FetchError::Status(status.as_u16()));
        }
        let total = resp.content_length().or(req.size_hint).unwrap_or(0);
        let mut out = tokio::fs::File::create(&req.dest).await?;
        let mut stream = resp.bytes_stream();
        let mut done = 0u64;
        loop {
            let next = tokio::select! {
                n = tokio::time::timeout(t::STALL_TIMEOUT, stream.next()) => n,
                _ = req.cancel.cancelled() => return Err(FetchError::Cancelled),
                _ = self.shared.paused() => return Err(FetchError::Paused),
            };
            match next {
                Err(_) => return Err(FetchError::Stalled),
                Ok(None) => break,
                Ok(Some(Err(e))) => return Err(FetchError::Network(e.to_string())),
                Ok(Some(Ok(bytes))) => {
                    out.write_all(&bytes).await?;
                    done += bytes.len() as u64;
                    self.shared.limiter.add_bytes(bytes.len() as u64);
                    (req.progress)(done, total.max(done));
                }
            }
        }
        out.flush().await?;
        if total > 0 && done < total {
            return Err(FetchError::Truncated { expected: total, actual: done });
        }
        Ok(done)
    }
}

impl Shared {
    async fn wait_resumed(&self) {
        let mut rx = self.pause.subscribe();
        while *rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Resolves as soon as downloading is paused.
    async fn paused(&self) {
        let mut rx = self.pause.subscribe();
        loop {
            if *rx.borrow_and_update() {
                return;
            }
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }

    /// Wait for the end of a pause; `Err` when the download was cancelled meanwhile.
    async fn resume_or_cancel(&self, cancel: &CancellationToken) -> Result<(), FetchError> {
        tokio::select! {
            _ = self.wait_resumed() => Ok(()),
            _ = cancel.cancelled() => Err(FetchError::Cancelled),
            _ = self.cancel.cancelled() => Err(FetchError::Cancelled),
        }
    }

    async fn worker(self: Arc<Self>) {
        loop {
            // Registered before we look at the queue so a push between "empty" and "wait"
            // cannot be missed.
            let notified = self.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            tokio::select! {
                _ = self.wait_resumed() => {}
                _ = self.cancel.cancelled() => return,
            }
            let permit = tokio::select! {
                p = self.limiter.gate.acquire() => p,
                _ = self.cancel.cancelled() => return,
            };
            match self.claim() {
                Some((job, idx)) => self.run_chunk(job, idx, permit).await,
                None => {
                    drop(permit);
                    tokio::select! {
                        _ = notified => {}
                        _ = self.cancel.cancelled() => return,
                    }
                }
            }
        }
    }

    fn claim(&self) -> Option<(Arc<FileJob>, u64)> {
        let mut q = self.queue.lock().unwrap();
        loop {
            let job = q.front()?.clone();
            if job.cancel.is_cancelled() || job.failed.load(Ordering::SeqCst) {
                q.pop_front();
                continue;
            }
            let pos = job.next.fetch_add(1, Ordering::SeqCst);
            let Some(&idx) = job.pending.get(pos as usize) else {
                q.pop_front();
                continue;
            };
            return Some((job, idx));
        }
    }

    async fn run_chunk(&self, job: Arc<FileJob>, idx: u64, permit: Permit) {
        let start = idx * t::CHUNK_SIZE;
        let end = (start + t::CHUNK_SIZE).min(job.total) - 1;
        match self.fetch_with_retry(&job, start, end, permit).await {
            Ok(buf) => {
                let file = job.file.clone();
                let written = tokio::task::spawn_blocking(move || write_all_at(&file, &buf, start)).await;
                match written {
                    Ok(Ok(())) => {
                        if let Some(journal) = &job.journal {
                            let _ = journal.send(idx as u32);
                        }
                        if job.remaining.fetch_sub(1, Ordering::SeqCst) == 1 && !job.failed.load(Ordering::SeqCst) {
                            job.succeed();
                        }
                    }
                    Ok(Err(e)) => job.fail(e.into()),
                    Err(e) => job.fail(FetchError::Io(e.to_string())),
                }
            }
            Err(e) => job.fail(e),
        }
    }

    async fn fetch_with_retry(&self, job: &FileJob, start: u64, end: u64, first: Permit) -> Result<Vec<u8>, FetchError> {
        let mut permit = Some(first);
        let mut attempt = 0u32;
        loop {
            if job.cancel.is_cancelled() || job.failed.load(Ordering::SeqCst) {
                return Err(FetchError::Cancelled);
            }
            if *self.pause.borrow() {
                // Do not sit on a connection permit while paused.
                permit = None;
                self.resume_or_cancel(&job.cancel).await?;
            }
            let held = match permit.take() {
                Some(p) => p,
                None => tokio::select! {
                    p = self.limiter.gate.acquire() => p,
                    _ = job.cancel.cancelled() => return Err(FetchError::Cancelled),
                },
            };
            let mut attempt_bytes = 0u64;
            let result = self.fetch_range_once(job, start, end, &mut attempt_bytes).await;
            drop(held);
            match result {
                Ok(buf) => return Ok(buf),
                Err(FetchError::Paused) => {
                    // Not a failure and not an attempt: the same piece is fetched again after the
                    // resume. What had arrived of it does not count as progress.
                    job.sub_progress(attempt_bytes);
                    job.report_now();
                    self.resume_or_cancel(&job.cancel).await?;
                }
                Err(e) => {
                    job.sub_progress(attempt_bytes);
                    if !e.is_retryable() || attempt >= t::MAX_RETRIES {
                        return Err(e);
                    }
                    if e.signals_congestion() {
                        self.limiter.report_error();
                    }
                    tracing::debug!("chunk {start}-{end} failed ({e}); retry {}", attempt + 1);
                    // The permit is released while backing off so other chunks can use it.
                    sleep_or_cancel(backoff(attempt), &job.cancel).await?;
                    attempt += 1;
                }
            }
        }
    }

    async fn fetch_range_once(&self, job: &FileJob, start: u64, end: u64, counted: &mut u64) -> Result<Vec<u8>, FetchError> {
        let expected = end - start + 1;
        let send = self.http.get(&job.url).header(RANGE, format!("bytes={start}-{end}")).send();
        let resp = tokio::select! {
            r = tokio::time::timeout(t::STALL_TIMEOUT, send) => r.map_err(|_| FetchError::Stalled)?
                .map_err(|e| FetchError::Network(e.to_string()))?,
            _ = job.cancel.cancelled() => return Err(FetchError::Cancelled),
            _ = self.paused() => return Err(FetchError::Paused),
        };
        match resp.status() {
            StatusCode::PARTIAL_CONTENT => {}
            StatusCode::OK if start == 0 && resp.content_length() == Some(expected) => {}
            StatusCode::OK => return Err(FetchError::RangeUnsupported),
            s if s == StatusCode::FORBIDDEN || s == StatusCode::UNAUTHORIZED => return Err(FetchError::Forbidden(s.as_u16())),
            s => return Err(FetchError::Status(s.as_u16())),
        }

        let mut buf = Vec::with_capacity(expected as usize);
        let mut stream = resp.bytes_stream();
        loop {
            let next = tokio::select! {
                n = tokio::time::timeout(t::STALL_TIMEOUT, stream.next()) => n,
                _ = job.cancel.cancelled() => return Err(FetchError::Cancelled),
                _ = self.paused() => return Err(FetchError::Paused),
            };
            match next {
                Err(_) => return Err(FetchError::Stalled),
                Ok(None) => break,
                Ok(Some(Err(e))) => return Err(FetchError::Network(e.to_string())),
                Ok(Some(Ok(bytes))) => {
                    let n = bytes.len() as u64;
                    if buf.len() as u64 + n > expected {
                        return Err(FetchError::Truncated { expected, actual: buf.len() as u64 + n });
                    }
                    buf.extend_from_slice(&bytes);
                    *counted += n;
                    self.limiter.add_bytes(n);
                    job.add_progress(n);
                }
            }
        }
        if buf.len() as u64 != expected {
            return Err(FetchError::Truncated { expected, actual: buf.len() as u64 });
        }
        Ok(buf)
    }
}

struct Prepared {
    file: File,
    /// `done[i]` is true when piece `i` is already in the file.
    done: Vec<bool>,
    journal: Option<File>,
}

/// Open the destination: an interrupted download is continued when its journal matches,
/// otherwise the file starts over.
fn prepare_file(path: &Path, total: u64, pieces: u64, resume: Option<&Resume>) -> io::Result<Prepared> {
    use std::fs::OpenOptions;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let fresh = || -> io::Result<File> {
        let f = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(path)?;
        f.set_len(total)?;
        Ok(f)
    };
    let none_done = || vec![false; pieces as usize];
    let Some(resume) = resume else { return Ok(Prepared { file: fresh()?, done: none_done(), journal: None }) };

    let journal_file = journal_path(path);
    let header = journal_header(&resume.key, total);
    if resume.reuse
        && let Some((done, valid_len)) = read_journal(&journal_file, &header, pieces)
        && std::fs::metadata(path).is_ok_and(|m| m.len() == total)
    {
        // Cut off a half-written last record, then carry on appending.
        OpenOptions::new().write(true).open(&journal_file)?.set_len(valid_len)?;
        let journal = OpenOptions::new().append(true).open(&journal_file)?;
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        return Ok(Prepared { file, done, journal: Some(journal) });
    }
    let file = fresh()?;
    let mut journal = OpenOptions::new().create(true).write(true).truncate(true).open(&journal_file)?;
    journal.write_all(header.as_bytes())?;
    Ok(Prepared { file, done: none_done(), journal: Some(journal) })
}

fn backoff(attempt: u32) -> Duration {
    let base = t::BACKOFF_BASE.as_millis() as u64;
    let exp = base.saturating_mul(1u64 << attempt.min(16));
    let capped = exp.min(t::BACKOFF_CAP.as_millis() as u64);
    // Full jitter on the upper half keeps parallel workers from retrying in lock-step.
    let jittered = capped / 2 + rand::rng().random_range(0..=capped / 2);
    Duration::from_millis(jittered)
}

async fn sleep_or_cancel(d: Duration, cancel: &CancellationToken) -> Result<(), FetchError> {
    tokio::select! {
        _ = tokio::time::sleep(d) => Ok(()),
        _ = cancel.cancelled() => Err(FetchError::Cancelled),
    }
}

#[cfg(unix)]
fn write_all_at(f: &File, buf: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(f, buf, offset)
}

#[cfg(windows)]
fn write_all_at(f: &File, mut buf: &[u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_write(buf, offset)?;
        if n == 0 {
            return Err(io::Error::from(io::ErrorKind::WriteZero));
        }
        buf = &buf[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::atomic::AtomicUsize;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    /// A server for one file that understands `Range`. It records the start offset of every
    /// ranged request and answers each one after `delay`.
    struct Mock {
        base: String,
        hits: Arc<Mutex<Vec<u64>>>,
    }

    async fn mock(body: Arc<Vec<u8>>, delay: Duration) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let log = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let (body, log) = (body.clone(), log.clone());
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let mut n = 0;
                    loop {
                        let Ok(read) = sock.read(&mut buf[n..]).await else { return };
                        if read == 0 {
                            return;
                        }
                        n += read;
                        if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let request = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                    let range = request.lines().find_map(|l| l.strip_prefix("range: bytes=")).and_then(|r| {
                        let (a, b) = r.trim().split_once('-')?;
                        Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?))
                    });
                    tokio::time::sleep(delay).await;
                    let (head, slice) = match range {
                        Some((a, b)) => {
                            log.lock().unwrap().push(a);
                            let b = b.min(body.len() as u64 - 1);
                            let head = format!(
                                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{b}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len(),
                                b - a + 1
                            );
                            (head, &body[a as usize..=b as usize])
                        }
                        None => {
                            (format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()), &body[..])
                        }
                    };
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.write_all(slice).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        Mock { base: format!("http://{addr}/file"), hits }
    }

    fn sample(len: u64) -> Arc<Vec<u8>> {
        Arc::new((0..len).map(|i| (i % 251) as u8).collect())
    }

    /// Five full pieces and a short one.
    fn sample_len() -> u64 {
        t::CHUNK_SIZE * 5 + 1000
    }

    fn fetcher() -> Fetcher {
        Fetcher::new(build_http_client(None).unwrap(), Limiter::new(), CancellationToken::new())
    }

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ncm-fetch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn request(mock: &Mock, dest: &Path, total: u64) -> FetchRequest {
        FetchRequest {
            url: mock.base.clone(),
            size_hint: Some(total),
            dest: dest.to_path_buf(),
            progress: Arc::new(|_, _| {}),
            cancel: CancellationToken::new(),
            resume: None,
        }
    }

    fn resumable(mock: &Mock, dest: &Path, total: u64, key: &str, reuse: bool) -> FetchRequest {
        FetchRequest { resume: Some(Resume { key: key.into(), reuse }), ..request(mock, dest, total) }
    }

    /// What an interrupted run leaves behind: a part file whose finished pieces hold the right
    /// bytes and whose other pieces hold junk, plus a journal that lists the finished ones.
    fn leave_partial(dest: &Path, body: &[u8], key: &str, finished: &[u32], stray: &[u8]) {
        let mut data = vec![0xAAu8; body.len()];
        for &i in finished {
            let start = i as usize * t::CHUNK_SIZE as usize;
            let end = (start + t::CHUNK_SIZE as usize).min(body.len());
            data[start..end].copy_from_slice(&body[start..end]);
        }
        std::fs::write(dest, data).unwrap();
        let mut journal = journal_header(key, body.len() as u64).into_bytes();
        for i in finished {
            journal.extend_from_slice(&i.to_le_bytes());
        }
        journal.extend_from_slice(stray);
        std::fs::write(journal_path(dest), journal).unwrap();
    }

    fn requested(server: &Mock) -> Vec<u64> {
        let mut v = server.hits.lock().unwrap().clone();
        v.sort();
        v
    }

    fn piece(i: u64) -> u64 {
        i * t::CHUNK_SIZE
    }

    #[tokio::test]
    async fn downloads_a_file_in_pieces() {
        let body = sample(sample_len());
        let server = mock(body.clone(), Duration::ZERO).await;
        let dir = temp("plain");
        let dest = dir.join("a.part");
        let n = fetcher().download(request(&server, &dest, body.len() as u64)).await.unwrap();
        assert_eq!(n, body.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), *body);
        assert_eq!(server.hits.lock().unwrap().len(), 6);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn pausing_stops_requests_until_resumed() {
        let body = sample(sample_len());
        let server = mock(body.clone(), Duration::from_millis(80)).await;
        let dir = temp("pause");
        let dest = dir.join("a.part");
        let fetcher = fetcher();

        // Started while paused: nothing is requested.
        fetcher.set_paused(true);
        assert!(fetcher.is_paused());
        let task = tokio::spawn({
            let (f, req) = (fetcher.clone(), request(&server, &dest, body.len() as u64));
            async move { f.download(req).await }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(server.hits.lock().unwrap().is_empty(), "no request may be made while paused");

        // Let some requests start, then pause again: they are dropped and nothing new starts.
        fetcher.set_paused(false);
        tokio::time::sleep(Duration::from_millis(120)).await;
        fetcher.set_paused(true);
        tokio::time::sleep(Duration::from_millis(400)).await;
        let frozen = server.hits.lock().unwrap().len();
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(server.hits.lock().unwrap().len(), frozen, "no new request while paused");
        assert!(!task.is_finished());

        fetcher.set_paused(false);
        let n = tokio::time::timeout(Duration::from_secs(20), task).await.expect("finishes after the resume").unwrap().unwrap();
        assert_eq!(n, body.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), *body, "the file is complete and correct");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn continues_an_interrupted_download_from_its_journal() {
        let body = sample(sample_len());
        let server = mock(body.clone(), Duration::ZERO).await;
        let dir = temp("resume");
        let dest = dir.join("a.part");
        // Pieces 0, 1 and 4 are on disk; the journal ends in a half-written record.
        leave_partial(&dest, &body, "42:abc", &[0, 1, 4], &[7, 0]);

        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut req = resumable(&server, &dest, body.len() as u64, "42:abc", true);
        req.progress = {
            let seen = seen.clone();
            Arc::new(move |done, total| seen.lock().unwrap().push((done, total)))
        };
        let n = fetcher().download(req).await.unwrap();

        assert_eq!(n, body.len() as u64);
        assert_eq!(requested(&server), [piece(2), piece(3), piece(5)], "only the missing pieces are fetched");
        assert_eq!(std::fs::read(&dest).unwrap(), *body);
        let first = seen.lock().unwrap()[0];
        assert_eq!(first, (3 * t::CHUNK_SIZE, body.len() as u64), "progress starts at what is already there");

        // The journal now lists every piece and has no stray bytes left. It is written by a
        // background task, so give it a moment.
        let header = journal_header("42:abc", body.len() as u64);
        let mut listed = None;
        for _ in 0..100 {
            listed = read_journal(&journal_path(&dest), &header, 6).filter(|(done, _)| done.iter().all(|d| *d));
            if listed.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let (_, valid) = listed.expect("every piece ends up in the journal");
        assert_eq!(valid, std::fs::metadata(journal_path(&dest)).unwrap().len());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_journal_for_other_content_is_ignored() {
        let body = sample(sample_len());
        let server = mock(body.clone(), Duration::ZERO).await;
        let dir = temp("resume-other");
        let dest = dir.join("a.part");
        leave_partial(&dest, &body, "42:old-md5", &[0, 1, 2, 3, 4], &[]);
        fetcher().download(resumable(&server, &dest, body.len() as u64, "42:new-md5", true)).await.unwrap();
        assert_eq!(requested(&server).len(), 6, "everything is fetched again");
        assert_eq!(std::fs::read(&dest).unwrap(), *body);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn starting_over_ignores_what_is_on_disk() {
        let body = sample(sample_len());
        let server = mock(body.clone(), Duration::ZERO).await;
        let dir = temp("resume-fresh");
        let dest = dir.join("a.part");
        leave_partial(&dest, &body, "k", &[0, 1, 2, 3, 4, 5], &[]);
        fetcher().download(resumable(&server, &dest, body.len() as u64, "k", false)).await.unwrap();
        assert_eq!(requested(&server).len(), 6);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_part_file_of_the_wrong_size_is_not_trusted() {
        let body = sample(sample_len());
        let server = mock(body.clone(), Duration::ZERO).await;
        let dir = temp("resume-size");
        let dest = dir.join("a.part");
        leave_partial(&dest, &body, "k", &[0, 1, 2, 3, 4, 5], &[]);
        let truncated = std::fs::OpenOptions::new().write(true).open(&dest).unwrap();
        truncated.set_len(1234).unwrap();
        drop(truncated);
        fetcher().download(resumable(&server, &dest, body.len() as u64, "k", true)).await.unwrap();
        assert_eq!(requested(&server).len(), 6);
        assert_eq!(std::fs::read(&dest).unwrap(), *body);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_complete_journal_needs_no_request() {
        let body = sample(sample_len());
        let server = mock(body.clone(), Duration::ZERO).await;
        let dir = temp("resume-complete");
        let dest = dir.join("a.part");
        leave_partial(&dest, &body, "k", &[0, 1, 2, 3, 4, 5], &[]);
        let n = fetcher().download(resumable(&server, &dest, body.len() as u64, "k", true)).await.unwrap();
        assert_eq!(n, body.len() as u64);
        assert!(requested(&server).is_empty());
        assert_eq!(std::fs::read(&dest).unwrap(), *body);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_fresh_download_writes_a_journal_and_discard_removes_both_files() {
        let body = sample(sample_len());
        let server = mock(body.clone(), Duration::ZERO).await;
        let dir = temp("resume-journal");
        let dest = dir.join("a.part");
        assert!(!has_resume_data(&dest));
        fetcher().download(resumable(&server, &dest, body.len() as u64, "k", true)).await.unwrap();
        assert!(has_resume_data(&dest));
        discard_partial(&dest);
        assert!(!dest.exists() && !journal_path(&dest).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn wait_resumed_returns_when_the_pause_ends() {
        let fetcher = fetcher();
        fetcher.wait_resumed().await; // not paused: immediate
        fetcher.set_paused(true);
        let woke = Arc::new(AtomicUsize::new(0));
        let waiter = tokio::spawn({
            let (f, woke) = (fetcher.clone(), woke.clone());
            async move {
                f.wait_resumed().await;
                woke.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(woke.load(Ordering::SeqCst), 0);
        fetcher.set_paused(false);
        waiter.await.unwrap();
        assert_eq!(woke.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        for attempt in 0..20 {
            let d = backoff(attempt);
            assert!(d <= t::BACKOFF_CAP, "attempt {attempt}: {d:?}");
            assert!(d >= t::BACKOFF_BASE / 2);
        }
        assert!(backoff(0) <= t::BACKOFF_BASE);
        assert!(backoff(10) >= t::BACKOFF_CAP / 2);
    }

    #[test]
    fn retry_classification() {
        assert!(FetchError::Status(503).is_retryable());
        assert!(FetchError::Status(429).signals_congestion());
        assert!(!FetchError::Status(404).is_retryable());
        assert!(!FetchError::Forbidden(403).is_retryable());
        assert!(FetchError::Stalled.is_retryable());
    }
}
