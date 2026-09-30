//! Chunked, range-based file fetching driven by the adaptive connection gate.
//!
//! Files are split into `CHUNK_SIZE` pieces which go into one global FIFO. A fixed pool of
//! workers pulls pieces, but each piece first needs a permit from the [`Limiter`] gate, so the
//! number of requests in flight follows the adaptive limit. FIFO order keeps early files
//! finishing first instead of spreading connections over many half-finished files.

use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use rand::Rng;
use reqwest::StatusCode;
use reqwest::header::{CONTENT_RANGE, RANGE};
use tokio::sync::{Notify, oneshot};
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
}

struct FileJob {
    url: String,
    total: u64,
    file: Arc<File>,
    chunks: u64,
    next: AtomicU64,
    remaining: AtomicU64,
    downloaded: AtomicU64,
    reported: AtomicU64,
    failed: AtomicBool,
    done: Mutex<Option<oneshot::Sender<Result<(), FetchError>>>>,
    progress: ProgressFn,
    cancel: CancellationToken,
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
}

struct Shared {
    http: reqwest::Client,
    limiter: Arc<Limiter>,
    queue: Mutex<VecDeque<Arc<FileJob>>>,
    wake: Notify,
    cancel: CancellationToken,
}

/// Cheap handle; workers stop when `cancel` (given to [`Fetcher::new`]) fires.
#[derive(Clone)]
pub struct Fetcher {
    shared: Arc<Shared>,
}

impl Fetcher {
    pub fn new(http: reqwest::Client, limiter: Arc<Limiter>, cancel: CancellationToken) -> Self {
        let shared = Arc::new(Shared { http, limiter, queue: Mutex::new(VecDeque::new()), wake: Notify::new(), cancel });
        for _ in 0..t::MAX_CONNECTIONS {
            tokio::spawn(shared.clone().worker());
        }
        Self { shared }
    }

    pub fn limiter(&self) -> &Arc<Limiter> {
        &self.shared.limiter
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

        let file = Arc::new(prepare_file(&req.dest, total)?);
        let chunks = total.div_ceil(t::CHUNK_SIZE);
        let (tx, rx) = oneshot::channel();
        let job = Arc::new(FileJob {
            url: req.url.clone(),
            total,
            file,
            chunks,
            next: AtomicU64::new(0),
            remaining: AtomicU64::new(chunks),
            downloaded: AtomicU64::new(0),
            reported: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            done: Mutex::new(Some(tx)),
            progress: req.progress.clone(),
            cancel: req.cancel.clone(),
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
        let mut last_err = FetchError::Stalled;
        for attempt in 0..=t::MAX_RETRIES.min(3) {
            if req.cancel.is_cancelled() {
                return Err(FetchError::Cancelled);
            }
            let permit = tokio::select! {
                p = self.shared.limiter.gate.acquire() => p,
                _ = req.cancel.cancelled() => return Err(FetchError::Cancelled),
            };
            let result = self.stream_once(req).await;
            drop(permit);
            match result {
                Ok(n) => return Ok(n),
                Err(e) if e.is_retryable() && attempt < t::MAX_RETRIES.min(3) => {
                    if e.signals_congestion() {
                        self.shared.limiter.report_error();
                    }
                    last_err = e;
                    sleep_or_cancel(backoff(attempt), &req.cancel).await?;
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_err)
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
    async fn worker(self: Arc<Self>) {
        loop {
            // Registered before we look at the queue so a push between "empty" and "wait"
            // cannot be missed.
            let notified = self.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

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
            let idx = job.next.fetch_add(1, Ordering::SeqCst);
            if idx >= job.chunks {
                q.pop_front();
                continue;
            }
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

fn prepare_file(path: &Path, len: u64) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(path)?;
    f.set_len(len)?;
    Ok(f)
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
    use super::*;

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
