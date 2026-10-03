//! End-to-end run of the download engine against the live service (anonymous).
//!
//! `cargo run -p ncm-core --example engine_live [playlist_id] [count] [level]`

use std::path::PathBuf;

use lofty::file::TaggedFileExt;
use lofty::tag::Accessor;
use ncm_api::{Client, ClientOptions, Level, Session};
use ncm_core::{BatchRequest, CoverFile, DownloadOptions, Engine, Event, TrackJob, TrackUpdate};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let playlist: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(3778678);
    let count: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);
    let level = args.next().and_then(|s| Level::parse(&s)).unwrap_or(Level::Exhigh);

    let api = Client::new(Session::new(), ClientOptions::default())?;
    let pl = api.playlist(playlist).await?;
    println!("playlist: {} ({} tracks, taking {count})", pl.name, pl.tracks.len());

    let out: PathBuf = std::env::temp_dir().join("ncm-engine-live");
    let _ = std::fs::remove_dir_all(&out);

    let (engine, mut events) = Engine::new(&tokio::runtime::Handle::current(), api, None)?;
    let options = DownloadOptions {
        output_dir: out.clone(),
        level,
        cover_file: CoverFile::Folder,
        source_comment: true,
        ..DownloadOptions::default()
    };
    let tracks: Vec<TrackJob> =
        pl.tracks.iter().take(count).cloned().enumerate().map(|(i, track)| TrackJob { track, index: i + 1 }).collect();
    let names: std::collections::HashMap<u64, String> = tracks.iter().map(|j| (j.track.id, j.track.name.clone())).collect();
    engine.submit(BatchRequest { collection: pl.name.clone(), collection_cover: pl.cover_url.clone(), tracks, options });

    let mut last_pct = std::collections::HashMap::new();
    while let Some(ev) = events.recv().await {
        match ev {
            Event::Track { id, update, .. } => match update {
                TrackUpdate::Progress { done, total } => {
                    let pct = done * 100 / total.max(1) / 25 * 25;
                    if last_pct.insert(id, pct) != Some(pct) {
                        println!("  [{}] {pct}%", names[&id]);
                    }
                }
                TrackUpdate::Stage(s) => println!("  [{}] {s:?}", names[&id]),
                other => println!("  [{}] {other:?}", names[&id]),
            },
            Event::Net(n) => {
                if n.in_flight > 0 {
                    println!("  net: {:.2} MiB/s, {} in flight, limit {}", n.bytes_per_sec / 1_048_576.0, n.in_flight, n.limit);
                }
            }
            Event::BatchFinished { summary, .. } => {
                println!("finished: {summary:?}");
                break;
            }
            Event::Paused(paused) => println!("  paused: {paused}"),
        }
    }
    engine.shutdown();

    println!("--- files ---");
    for entry in walk(&out) {
        let size = entry.metadata()?.len();
        println!("{:>10}  {}", size, entry.strip_prefix(&out)?.display());
        if matches!(entry.extension().and_then(|e| e.to_str()), Some("mp3" | "flac" | "m4a")) {
            let f = lofty::read_from_path(&entry)?;
            if let Some(tag) = f.primary_tag() {
                println!(
                    "            tag: title={:?} artist={:?} album={:?} track={:?} pictures={} lyrics_bytes={}",
                    tag.title(),
                    tag.artist(),
                    tag.album(),
                    tag.track(),
                    tag.picture_count(),
                    tag.get_string(lofty::tag::ItemKey::UnsyncLyrics)
                        .or_else(|| tag.get_string(lofty::tag::ItemKey::Lyrics))
                        .map_or(0, str::len)
                );
            }
        }
    }
    Ok(())
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { out.extend(walk(&p)) } else { out.push(p) }
        }
    }
    out.sort();
    out
}
