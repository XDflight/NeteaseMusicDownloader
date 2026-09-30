//! Downloads a few free tracks through the adaptive fetcher and verifies their MD5.
//!
//! `cargo run -p ncm-core --example fetch_live [level] [playlist_id]`

use std::sync::Arc;
use std::time::Instant;

use ncm_api::{Client, ClientOptions, Level, Session, crypto::md5_hex};
use ncm_core::adaptive::Limiter;
use ncm_core::fetch::{FetchRequest, Fetcher, build_http_client};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber_init();
    let mut args = std::env::args().skip(1);
    let level = args.next().and_then(|s| Level::parse(&s)).unwrap_or(Level::Exhigh);
    let playlist: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(3778678);

    let api = Client::new(Session::new(), ClientOptions::default())?;
    let pl = api.playlist(playlist).await?;
    let ids: Vec<u64> = pl.tracks.iter().take(6).map(|t| t.id).collect();
    let urls = api.song_urls(&ids, level).await?;

    let cancel = CancellationToken::new();
    let limiter = Limiter::new();
    tokio::spawn(limiter.clone().run_controller(cancel.clone()));
    let fetcher = Fetcher::new(build_http_client(None)?, limiter.clone(), cancel.clone());

    let dir = std::env::temp_dir().join("ncm-fetch-live");
    std::fs::create_dir_all(&dir)?;
    let started = Instant::now();
    let mut handles = Vec::new();
    for u in urls.into_iter().filter(|u| u.is_usable()) {
        let fetcher = fetcher.clone();
        let dest = dir.join(format!("{}.{}", u.id, u.kind.clone().unwrap_or_else(|| "bin".into())));
        handles.push(tokio::spawn(async move {
            let req = FetchRequest {
                url: u.url.clone().unwrap(),
                size_hint: (u.size > 0).then_some(u.size),
                dest: dest.clone(),
                progress: Arc::new(|_, _| {}),
                cancel: CancellationToken::new(),
            };
            let t0 = Instant::now();
            let n = fetcher.download(req).await?;
            let data = std::fs::read(&dest)?;
            let ok = u.md5.as_deref().map(|m| m.eq_ignore_ascii_case(&md5_hex(&data)));
            println!(
                "{:>10} {:>9} bytes in {:>5.1}s  md5 match: {:?}  size match: {}",
                u.id,
                n,
                t0.elapsed().as_secs_f32(),
                ok,
                n == u.size
            );
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
        }));
    }
    for h in handles {
        if let Err(e) = h.await? {
            println!("failed: {e}");
        }
    }
    let snap = limiter.snapshot();
    println!(
        "total {:.1}s, limit now {}, speed {:.2} MiB/s",
        started.elapsed().as_secs_f32(),
        snap.limit,
        snap.bytes_per_sec / 1_048_576.0
    );
    cancel.cancel();
    Ok(())
}

fn tracing_subscriber_init() {}
