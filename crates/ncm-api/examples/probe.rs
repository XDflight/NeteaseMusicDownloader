//! Manual smoke test against the live service (anonymous, a handful of requests).
//!
//! `cargo run -p ncm-api --example probe [playlist_id] [song_id]`

use ncm_api::{Client, ClientOptions, Level, Session};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let playlist_id: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(3778678);
    let song_id: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(347230);

    let client = Client::new(Session::new(), ClientOptions::default())?;

    println!("== account (anonymous) ==");
    match client.refresh_account().await {
        Ok(a) => println!("{a:?}"),
        Err(e) => println!("expected error: {e}"),
    }

    println!("== song detail {song_id} ==");
    let tracks = client.tracks(&[song_id]).await?;
    for t in &tracks {
        println!(
            "{} - {} | album {} | {}ms | fee {} | {:?}",
            t.artist_names("/"),
            t.name,
            t.album.name,
            t.duration_ms,
            t.fee,
            t.availability()
        );
    }

    println!("== playlist {playlist_id} ==");
    let pl = client.playlist(playlist_id).await?;
    println!("{} by {} — {} tracks resolved", pl.name, pl.creator, pl.tracks.len());
    for t in pl.tracks.iter().take(3) {
        println!("  {} - {} ({:?})", t.artist_names("/"), t.name, t.availability());
    }

    if let Some(t) = tracks.first() {
        println!("  privilege = {:?}", t.privilege);
    }
    let raw = client.eapi("/v3/song/detail", serde_json::json!({ "c": format!("[{{\"id\":{song_id}}}]") })).await?;
    println!("  raw privilege = {}", raw["privileges"][0]);

    // A free track from the playlist gives a real URL to inspect.
    let free_id = pl.tracks.first().map(|t| t.id).unwrap_or(song_id);
    println!("== song urls (free track {free_id}) ==");
    for level in [Level::Standard, Level::Exhigh, Level::Lossless, Level::Hires] {
        for u in client.song_urls(&[free_id], level).await? {
            println!(
                "  want {:<8} -> got level={:?} type={:?} br={} size={} trial={} md5={:?} url={}",
                level.as_str(),
                u.level,
                u.kind,
                u.br,
                u.size,
                u.is_trial,
                u.md5.as_deref().map(|m| &m[..8]),
                u.url.as_deref().unwrap_or("<none>")
            );
        }
    }

    println!("== song urls {song_id} (VIP track, anonymous) ==");
    for level in [Level::Standard, Level::Exhigh, Level::Lossless] {
        let urls = client.song_urls(&[song_id], level).await?;
        for u in urls {
            println!(
                "  want {:<8} -> got level={:?} type={:?} br={} size={} trial={} md5={:?} url={}",
                level.as_str(),
                u.level,
                u.kind,
                u.br,
                u.size,
                u.is_trial,
                u.md5.as_deref().map(|m| &m[..8]),
                u.url.as_deref().map(|s| s.split('?').next().unwrap_or(s)).unwrap_or("<none>")
            );
        }
    }

    println!("== download url {song_id} ==");
    match client.download_url(song_id, Level::Exhigh).await {
        Ok(Some(u)) => println!("  level={:?} type={:?} br={} size={} url? {}", u.level, u.kind, u.br, u.size, u.url.is_some()),
        Ok(None) => println!("  none"),
        Err(e) => println!("  error: {e}"),
    }

    println!("== lyrics {song_id} ==");
    let ly = client.lyrics(song_id).await?;
    println!(
        "  lrc: {} bytes, translation: {}, romaji: {}, yrc: {}",
        ly.lrc.as_deref().map_or(0, str::len),
        ly.translation.is_some(),
        ly.romaji.is_some(),
        ly.word_by_word.is_some()
    );
    if let Some(l) = &ly.lrc {
        for line in l.lines().take(3) {
            println!("    {line}");
        }
    }

    println!("== qr key ==");
    let key = client.qr_login_key().await?;
    println!("  key = {key}");
    println!("  state = {:?}", client.qr_login_check(&key).await?);
    Ok(())
}
