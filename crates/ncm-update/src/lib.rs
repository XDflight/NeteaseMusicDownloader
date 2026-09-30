//! In-app updates from GitHub Releases.
//!
//! Flow: [`check`] → [`download`] (streamed + SHA-256 verified) → [`apply`].
//!
//! Release assets follow one naming scheme (produced by the release workflow):
//! `netease-music-downloader-<version>-<platform>-<arch>[-suffix].<ext>` plus a
//! `SHA256SUMS.txt` covering every asset. See [`Target`] for how the asset is chosen.

mod apply;
mod checksum;
mod release;
mod target;

pub use apply::{ApplyOutcome, Prepared, apply};
pub use release::{Asset, Release, UpdateInfo};
pub use target::{Target, detect_target};

use std::path::PathBuf;
use std::time::Duration;

use futures_util::StreamExt;
use semver::Version;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

/// Slug used in asset file names.
pub const APP_SLUG: &str = "netease-music-downloader";

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("GitHub answered HTTP {0}")]
    Status(u16),
    #[error("GitHub API rate limit reached; try again later")]
    RateLimited,
    #[error("malformed release data: {0}")]
    Parse(String),
    #[error("this installation cannot update itself: {0}")]
    Unsupported(String),
    #[error("the release has no asset for this platform ({0})")]
    NoAsset(String),
    #[error("release is missing SHA256SUMS.txt; refusing to install an unverified download")]
    NoChecksums,
    #[error("checksum mismatch for {name}: expected {expected}, got {actual}")]
    ChecksumMismatch { name: String, expected: String, actual: String },
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("archive error: {0}")]
    Archive(String),
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, UpdateError>;

#[derive(Debug, Clone)]
pub struct UpdateConfig {
    /// `owner/name` of the GitHub repository.
    pub repo: String,
    pub current_version: String,
    pub include_prerelease: bool,
    pub proxy: Option<String>,
    /// Override for tests / GitHub Enterprise.
    pub api_base: String,
    /// Where downloads are placed; a fresh folder under the system temp dir when `None`.
    pub download_dir: Option<PathBuf>,
}

impl UpdateConfig {
    pub fn new(current_version: &str) -> Self {
        Self {
            repo: option_env!("NMD_GITHUB_REPO").unwrap_or("XDflight/NeteaseMusicDownloader").to_owned(),
            current_version: current_version.to_owned(),
            include_prerelease: false,
            proxy: None,
            // `NMD_UPDATE_API_BASE` points the updater at a mirror or a GitHub Enterprise host.
            api_base: std::env::var("NMD_UPDATE_API_BASE")
                .ok()
                .map(|v| v.trim().trim_end_matches('/').to_owned())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "https://api.github.com".into()),
            download_dir: None,
        }
    }
}

fn client(cfg: &UpdateConfig, timeout: Option<Duration>) -> Result<reqwest::Client> {
    ncm_api::install_tls_provider();
    let mut b = reqwest::Client::builder()
        .user_agent(format!("{APP_SLUG}/{}", cfg.current_version))
        .connect_timeout(Duration::from_secs(15));
    if let Some(t) = timeout {
        b = b.timeout(t);
    }
    if let Some(p) = cfg.proxy.as_deref().filter(|p| !p.trim().is_empty()) {
        b = b.proxy(reqwest::Proxy::all(p.trim())?);
    }
    Ok(b.build()?)
}

/// Ask GitHub whether a newer release exists for this platform.
/// `Ok(None)` when up to date (or the repository has no releases yet).
pub async fn check(cfg: &UpdateConfig) -> Result<Option<UpdateInfo>> {
    check_for(cfg, detect_target()).await
}

pub(crate) async fn check_for(cfg: &UpdateConfig, target: std::result::Result<Target, String>) -> Result<Option<UpdateInfo>> {
    let current = Version::parse(cfg.current_version.trim_start_matches('v'))
        .map_err(|e| UpdateError::Parse(format!("current version: {e}")))?;
    let http = client(cfg, Some(Duration::from_secs(20)))?;

    let url = if cfg.include_prerelease {
        format!("{}/repos/{}/releases?per_page=10", cfg.api_base, cfg.repo)
    } else {
        format!("{}/repos/{}/releases/latest", cfg.api_base, cfg.repo)
    };
    let resp = http
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await?;
    match resp.status().as_u16() {
        200 => {}
        404 => return Ok(None),
        403 | 429 => return Err(UpdateError::RateLimited),
        s => return Err(UpdateError::Status(s)),
    }
    let body: serde_json::Value = resp.json().await?;
    let releases = release::parse_releases(&body, cfg.include_prerelease)?;

    let mut best: Option<UpdateInfo> = None;
    for rel in releases {
        if rel.version <= current {
            continue;
        }
        if best.as_ref().is_some_and(|b| b.release.version >= rel.version) {
            continue;
        }
        match target.as_ref().map_err(|e| UpdateError::Unsupported(e.clone())) {
            Ok(t) => {
                let asset = t.pick_asset(&rel).ok_or_else(|| UpdateError::NoAsset(t.describe()))?;
                let sums = rel.assets.iter().find(|a| a.name.eq_ignore_ascii_case("SHA256SUMS.txt")).cloned();
                best = Some(UpdateInfo { release: rel, asset, checksums: sums, target: t.clone() });
            }
            Err(e) => return Err(e),
        }
    }
    Ok(best)
}

/// Download the asset (reporting `done, total` bytes) and verify it against `SHA256SUMS.txt`.
pub async fn download(cfg: &UpdateConfig, info: &UpdateInfo, progress: impl Fn(u64, u64) + Send) -> Result<Prepared> {
    let sums_asset = info.checksums.as_ref().ok_or(UpdateError::NoChecksums)?;
    let http = client(cfg, None)?;

    let sums_text = http.get(&sums_asset.url).send().await?.error_for_status()?.text().await?;
    let expected = checksum::find_hash(&sums_text, &info.asset.name).ok_or(UpdateError::NoChecksums)?;

    let dir = match &cfg.download_dir {
        Some(d) => d.clone(),
        None => {
            cleanup_stale_downloads();
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            std::env::temp_dir().join(format!("{APP_SLUG}-update-{}-{}-{nanos}", info.release.version, std::process::id()))
        }
    };
    tokio::fs::create_dir_all(&dir).await?;
    let path: PathBuf = dir.join(&info.asset.name);

    let resp = http.get(&info.asset.url).send().await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(info.asset.size);
    let mut file = tokio::fs::File::create(&path).await?;
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        progress(done, total.max(done));
    }
    file.flush().await?;
    drop(file);

    let actual = hex::encode(hasher.finalize());
    if !actual.eq_ignore_ascii_case(&expected) {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(UpdateError::ChecksumMismatch { name: info.asset.name.clone(), expected, actual });
    }
    Ok(Prepared { path, target: info.target.clone(), version: info.release.version.clone() })
}

/// Remove download folders left behind by earlier runs (older than a day).
fn cleanup_stale_downloads() {
    let prefix = format!("{APP_SLUG}-update-");
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for entry in entries.flatten() {
        let stale = entry.file_name().to_string_lossy().starts_with(&prefix)
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > Duration::from_secs(24 * 3600));
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests;
