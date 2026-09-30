use std::io::Write;
use std::path::PathBuf;

use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;
use crate::apply::{extract_tar_gz_file, extract_zip_file};
use crate::target::enclosing_app;

/// Minimal HTTP server: `routes` maps a request path to a response body.
async fn serve(listener: TcpListener, routes: Vec<(String, Vec<u8>)>) {
    loop {
        let Ok((mut sock, _)) = listener.accept().await else { return };
        let routes = routes.clone();
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
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
            let (status, body) = match routes.iter().find(|(p, _)| *p == path) {
                Some((_, body)) => ("200 OK", body.clone()),
                None => ("404 Not Found", b"{}".to_vec()),
            };
            let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(&body).await;
            let _ = sock.shutdown().await;
        });
    }
}

const ASSET: &str = "netease-music-downloader-0.2.0-windows-x86_64-portable.zip";

struct Fixture {
    cfg: UpdateConfig,
    payload: Vec<u8>,
}

async fn fixture(latest_tag: &str, corrupt_sums: bool, with_asset: bool) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let payload = b"pretend this is a zip archive".to_vec();
    let hash = if corrupt_sums { "0".repeat(64) } else { hex::encode(Sha256::digest(&payload)) };
    let sums = format!("{hash}  {ASSET}\n").into_bytes();

    let mut assets =
        vec![json!({"name": "SHA256SUMS.txt", "browser_download_url": format!("{base}/dl/SHA256SUMS.txt"), "size": sums.len()})];
    if with_asset {
        assets.push(json!({"name": ASSET, "browser_download_url": format!("{base}/dl/{ASSET}"), "size": payload.len()}));
    }
    let release = json!({
        "tag_name": latest_tag, "name": latest_tag, "body": "notes", "draft": false, "prerelease": false,
        "html_url": "https://example.invalid/r", "published_at": "2026-01-01T00:00:00Z", "assets": assets
    });
    let routes = vec![
        ("/repos/o/r/releases/latest".to_owned(), release.to_string().into_bytes()),
        ("/dl/SHA256SUMS.txt".to_owned(), sums),
        (format!("/dl/{ASSET}"), payload.clone()),
    ];
    tokio::spawn(serve(listener, routes));
    let mut cfg = UpdateConfig::new("0.1.0");
    cfg.repo = "o/r".into();
    cfg.api_base = base;
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("ncm-update-test-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    cfg.download_dir = Some(dir);
    Fixture { cfg, payload }
}

fn portable() -> std::result::Result<Target, String> {
    Ok(Target::WindowsPortable { arch: "x86_64" })
}

#[tokio::test]
async fn newer_release_is_found_downloaded_and_verified() {
    let f = fixture("v0.2.0", false, true).await;
    let info = check_for(&f.cfg, portable()).await.unwrap().expect("update available");
    assert_eq!(info.release.version.to_string(), "0.2.0");
    assert_eq!(info.asset.name, ASSET);

    let seen = std::sync::Mutex::new(0u64);
    let prepared = download(&f.cfg, &info, |done, _| *seen.lock().unwrap() = done).await.unwrap();
    assert_eq!(std::fs::read(&prepared.path).unwrap(), f.payload);
    assert_eq!(*seen.lock().unwrap(), f.payload.len() as u64);
    let _ = std::fs::remove_dir_all(prepared.path.parent().unwrap());
}

#[tokio::test]
async fn tampered_download_is_rejected_and_deleted() {
    let f = fixture("v0.2.0", true, true).await;
    let info = check_for(&f.cfg, portable()).await.unwrap().unwrap();
    let err = download(&f.cfg, &info, |_, _| {}).await.unwrap_err();
    assert!(matches!(err, UpdateError::ChecksumMismatch { .. }), "{err}");
    assert!(!f.cfg.download_dir.as_ref().unwrap().join(ASSET).exists(), "the rejected file must be removed");
}

#[tokio::test]
async fn up_to_date_and_missing_releases_are_not_errors() {
    let f = fixture("v0.1.0", false, true).await;
    assert!(check_for(&f.cfg, portable()).await.unwrap().is_none());

    let mut none = f.cfg.clone();
    none.repo = "nobody/nothing".into();
    assert!(check_for(&none, portable()).await.unwrap().is_none(), "404 means no releases yet");
}

#[tokio::test]
async fn release_without_matching_asset_is_reported() {
    let f = fixture("v0.2.0", false, false).await;
    let err = check_for(&f.cfg, portable()).await.unwrap_err();
    assert!(matches!(err, UpdateError::NoAsset(_)), "{err}");
}

#[tokio::test]
async fn unsupported_installation_is_reported_only_when_an_update_exists() {
    let f = fixture("v0.2.0", false, true).await;
    let err = check_for(&f.cfg, Err("system package".into())).await.unwrap_err();
    assert!(matches!(err, UpdateError::Unsupported(_)));
    let g = fixture("v0.1.0", false, true).await;
    assert!(check_for(&g.cfg, Err("system package".into())).await.unwrap().is_none());
}

#[test]
fn asset_selection_per_target() {
    let names = [
        "netease-music-downloader-1.0.0-windows-x86_64-setup.exe",
        "netease-music-downloader-1.0.0-windows-x86_64-portable.zip",
        "netease-music-downloader-1.0.0-macos-universal.zip",
        "netease-music-downloader-1.0.0-macos-universal.dmg",
        "netease-music-downloader-1.0.0-linux-x86_64.AppImage",
        "netease-music-downloader-1.0.0-linux-x86_64.tar.gz",
        "netease-music-downloader-1.0.0-linux-aarch64.AppImage",
        "netease-music-downloader-1.0.0-linux-x86_64.deb",
        "SHA256SUMS.txt",
    ];
    let release = Release {
        tag: "v1.0.0".into(),
        version: semver::Version::new(1, 0, 0),
        name: String::new(),
        notes: String::new(),
        html_url: String::new(),
        published_at: String::new(),
        prerelease: false,
        assets: names.iter().map(|n| Asset { name: (*n).into(), url: format!("https://x/{n}"), size: 1 }).collect(),
    };
    let pick = |t: Target| t.pick_asset(&release).map(|a| a.name);
    assert_eq!(pick(Target::WindowsInstaller { arch: "x86_64" }).unwrap(), names[0]);
    assert_eq!(pick(Target::WindowsPortable { arch: "x86_64" }).unwrap(), names[1]);
    assert_eq!(pick(Target::MacApp { app_path: "/Applications/X.app".into() }).unwrap(), names[2]);
    assert_eq!(pick(Target::LinuxAppImage { path: "/x".into(), arch: "x86_64" }).unwrap(), names[4]);
    assert_eq!(pick(Target::LinuxAppImage { path: "/x".into(), arch: "aarch64" }).unwrap(), names[6]);
    assert_eq!(pick(Target::LinuxBinary { arch: "x86_64" }).unwrap(), names[5]);
    assert!(pick(Target::WindowsInstaller { arch: "aarch64" }).is_none());
}

#[test]
fn drafts_prereleases_and_non_semver_tags_are_skipped() {
    let body = json!([
        {"tag_name": "v9.9.9", "draft": true, "assets": []},
        {"tag_name": "v3.0.0-beta.1", "prerelease": true, "assets": []},
        {"tag_name": "nightly", "assets": []},
        {"tag_name": "v2.0.0", "assets": []}
    ]);
    let stable = release::parse_releases(&body, false).unwrap();
    assert_eq!(stable.iter().map(|r| r.tag.as_str()).collect::<Vec<_>>(), ["v2.0.0"]);
    let with_pre = release::parse_releases(&body, true).unwrap();
    assert_eq!(with_pre.len(), 2);
}

#[test]
fn checksum_file_parsing() {
    let h = "a".repeat(64);
    let text = format!("{h}  file-a.zip\n{}  *file-b.zip\nnot a hash line\n", "B".repeat(64));
    assert_eq!(checksum::find_hash(&text, "file-a.zip"), Some(h));
    assert_eq!(checksum::find_hash(&text, "file-b.zip"), Some("b".repeat(64)));
    assert_eq!(checksum::find_hash(&text, "missing.zip"), None);
}

#[test]
fn mac_bundle_is_found_from_the_executable_path() {
    let exe = PathBuf::from("/Applications/NeteaseMusicDownloader.app/Contents/MacOS/netease-music-downloader");
    assert_eq!(enclosing_app(&exe), Some(PathBuf::from("/Applications/NeteaseMusicDownloader.app")));
    assert_eq!(enclosing_app(&PathBuf::from("/usr/local/bin/tool")), None);
}

#[test]
fn archives_yield_the_executable() {
    let dir = std::env::temp_dir().join(format!("ncm-update-archives-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // zip
    let zip_path = dir.join("portable.zip");
    {
        let mut w = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        w.start_file("README.txt", opts).unwrap();
        w.write_all(b"readme").unwrap();
        w.start_file("bundle/netease-music-downloader.exe", opts).unwrap();
        w.write_all(b"MZ-new-binary").unwrap();
        w.finish().unwrap();
    }
    let out = extract_zip_file(&zip_path, "netease-music-downloader.exe").unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), b"MZ-new-binary");
    assert!(extract_zip_file(&zip_path, "nope.exe").is_err());

    // tar.gz
    let tgz_path = dir.join("linux.tar.gz");
    {
        let gz = flate2::write::GzEncoder::new(std::fs::File::create(&tgz_path).unwrap(), flate2::Compression::default());
        let mut b = tar::Builder::new(gz);
        let data = b"ELF-new-binary";
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        b.append_data(&mut header, "netease-music-downloader-1.0/netease-music-downloader", &data[..]).unwrap();
        b.into_inner().unwrap().finish().unwrap();
    }
    let out = extract_tar_gz_file(&tgz_path, "netease-music-downloader").unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), b"ELF-new-binary");
    std::fs::remove_dir_all(&dir).ok();
}
