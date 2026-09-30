//! Asset pipeline. Run from the repository root:
//!
//! ```text
//! cargo run -p asset-tool -- all          # process everything found in assets/source/
//! cargo run -p asset-tool -- placeholders # write stand-in images where real ones are missing
//! ```
//!
//! Inputs (`assets/source/`): `mascot_<mood>.png` on a white background, `logo.png`,
//! `banner.png`, `installer_side.png`. Outputs go to `crates/app/assets/` (embedded in the app)
//! and `packaging/` (icons and installer images).

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use image::codecs::ico::{IcoEncoder, IcoFrame};
use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::imageops::{self, FilterType as Resize};
use image::{ExtendedColorType, ImageEncoder, Rgba, RgbaImage};

const MOODS: [&str; 6] = ["idle", "working", "happy", "sad", "login", "search"];
const SPRITE_HEIGHT: u32 = 640;

fn main() -> Result<()> {
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "all".into());
    match cmd.as_str() {
        "all" => {
            sprites()?;
            logo()?;
            banner()?;
            installer()?;
            wizard_header()?;
            dmg_background()?;
            placeholders()?;
        }
        "sprites" => sprites()?,
        "logo" => logo()?,
        "banner" => banner()?,
        "installer" => {
            installer()?;
            wizard_header()?;
            dmg_background()?;
        }
        "placeholders" => placeholders()?,
        other => bail!("unknown command {other:?}; use all | sprites | logo | banner | installer | placeholders"),
    }
    Ok(())
}

// ---------------------------------------------------------------------------- paths

fn source(name: &str) -> PathBuf {
    Path::new("assets/source").join(name)
}
fn app_asset(name: &str) -> PathBuf {
    Path::new("crates/app/assets").join(name)
}
fn out_path(p: &Path) -> Result<PathBuf> {
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir)?;
    }
    Ok(p.to_path_buf())
}

fn load(path: &Path) -> Result<RgbaImage> {
    Ok(image::open(path).with_context(|| format!("reading {}", path.display()))?.to_rgba8())
}

fn save_png(img: &RgbaImage, path: &Path) -> Result<()> {
    let file = BufWriter::new(File::create(out_path(path)?)?);
    PngEncoder::new_with_quality(file, CompressionType::Best, FilterType::Adaptive).write_image(
        img.as_raw(),
        img.width(),
        img.height(),
        ExtendedColorType::Rgba8,
    )?;
    println!("  wrote {} ({}x{})", path.display(), img.width(), img.height());
    Ok(())
}

// ------------------------------------------------------------------ background removal

/// Remove a near-white background that is connected to the image border, keep enclosed whites
/// (e.g. the uniform), and soften the edge so no white halo remains.
fn cut_out(mut img: RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    if img.pixels().filter(|p| p[3] < 250).count() * 100 > (w * h) as usize {
        return img; // already has real transparency
    }
    let is_bg = |p: &Rgba<u8>| {
        let (mn, mx) = (p[0].min(p[1]).min(p[2]), p[0].max(p[1]).max(p[2]));
        mn >= 238 && mx - mn <= 22
    };
    let mut bg = vec![false; (w * h) as usize];
    let mut queue = VecDeque::new();
    let seed = |x: u32, y: u32, bg: &mut Vec<bool>, q: &mut VecDeque<(u32, u32)>, img: &RgbaImage| {
        let i = (y * w + x) as usize;
        if !bg[i] && is_bg(img.get_pixel(x, y)) {
            bg[i] = true;
            q.push_back((x, y));
        }
    };
    for x in 0..w {
        seed(x, 0, &mut bg, &mut queue, &img);
        seed(x, h - 1, &mut bg, &mut queue, &img);
    }
    for y in 0..h {
        seed(0, y, &mut bg, &mut queue, &img);
        seed(w - 1, y, &mut bg, &mut queue, &img);
    }
    while let Some((x, y)) = queue.pop_front() {
        for (dx, dy) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
            let (nx, ny) = (x as i32 + dx, y as i32 + dy);
            if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                continue;
            }
            seed(nx as u32, ny as u32, &mut bg, &mut queue, &img);
        }
    }

    // Distance (in pixels, capped at 3) of each foreground pixel to the background.
    let mut dist = vec![0u8; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            if bg[(y * w + x) as usize] {
                continue;
            }
            'search: for r in 1..=3i32 {
                for dy in -r..=r {
                    for dx in -r..=r {
                        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                        if nx >= 0 && ny >= 0 && nx < w as i32 && ny < h as i32 && bg[(ny as u32 * w + nx as u32) as usize] {
                            dist[(y * w + x) as usize] = r as u8;
                            break 'search;
                        }
                    }
                }
            }
        }
    }
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let p = img.get_pixel_mut(x, y);
            if bg[i] {
                *p = Rgba([255, 255, 255, 0]);
                continue;
            }
            let alpha = match dist[i] {
                1 => 0.55,
                2 => 0.85,
                _ => 1.0,
            };
            if alpha < 1.0 {
                // Undo the blend with the white background.
                for c in 0..3 {
                    let v = (f32::from(p[c]) - (1.0 - alpha) * 255.0) / alpha;
                    p[c] = v.clamp(0.0, 255.0) as u8;
                }
                p[3] = (alpha * 255.0) as u8;
            }
        }
    }
    img
}

fn trim(img: &RgbaImage, pad: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (x, y, p) in img.enumerate_pixels() {
        if p[3] > 8 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x1 < x0 {
        return img.clone();
    }
    let (x0, y0) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
    let (x1, y1) = ((x1 + pad).min(w - 1), (y1 + pad).min(h - 1));
    imageops::crop_imm(img, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image()
}

fn fit_height(img: &RgbaImage, height: u32) -> RgbaImage {
    let width = (u64::from(img.width()) * u64::from(height) / u64::from(img.height())).max(1) as u32;
    imageops::resize(img, width, height, Resize::Lanczos3)
}

// -------------------------------------------------------------------------- commands

fn sprites() -> Result<()> {
    println!("sprites:");
    for mood in MOODS {
        let src = source(&format!("mascot_{mood}.png"));
        if !src.exists() {
            println!("  skip {} (missing)", src.display());
            continue;
        }
        let img = trim(&cut_out(load(&src)?), 6);
        save_png(&fit_height(&img, SPRITE_HEIGHT), &app_asset(&format!("mascot_{mood}.png")))?;
    }
    Ok(())
}

fn center_square(img: &RgbaImage) -> RgbaImage {
    let s = img.width().min(img.height());
    imageops::crop_imm(img, (img.width() - s) / 2, (img.height() - s) / 2, s, s).to_image()
}

/// Round the corners (radius as a fraction of the side) with anti-aliased edges.
fn round_corners(img: &RgbaImage, radius_frac: f32) -> RgbaImage {
    let mut out = img.clone();
    let s = out.width() as f32;
    let r = s * radius_frac;
    for (x, y, p) in out.enumerate_pixels_mut() {
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        let cx = fx.clamp(r, s - r);
        let cy = fy.clamp(r, s - r);
        let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
        let cover = (r - d + 0.5).clamp(0.0, 1.0);
        p[3] = (f32::from(p[3]) * cover) as u8;
    }
    out
}

fn logo() -> Result<()> {
    let src = source("logo.png");
    if !src.exists() {
        println!("logo: skip (missing {})", src.display());
        return Ok(());
    }
    println!("logo:");
    let square = imageops::resize(&center_square(&load(&src)?), 1024, 1024, Resize::Lanczos3);
    let round = round_corners(&square, 0.2237);

    for size in [16u32, 24, 32, 48, 64, 128, 256, 512, 1024] {
        let img = if size == 1024 { round.clone() } else { imageops::resize(&round, size, size, Resize::Lanczos3) };
        save_png(&img, &Path::new("packaging/icons/png").join(format!("icon-{size}.png")))?;
    }
    save_png(&imageops::resize(&round, 256, 256, Resize::Lanczos3), &app_asset("icon_256.png"))?;

    // Windows .ico with PNG-compressed frames.
    let mut frames = Vec::new();
    for size in [16u32, 24, 32, 48, 64, 128, 256] {
        let img = imageops::resize(&round, size, size, Resize::Lanczos3);
        frames.push(IcoFrame::as_png(img.as_raw(), size, size, ExtendedColorType::Rgba8)?);
    }
    let ico = out_path(Path::new("packaging/icons/app.ico"))?;
    IcoEncoder::new(BufWriter::new(File::create(&ico)?)).encode_images(&frames)?;
    println!("  wrote {}", ico.display());

    // macOS .icns: the artwork sits inside the 824/1024 safe area of Apple's icon grid.
    let mut canvas = RgbaImage::from_pixel(1024, 1024, Rgba([0, 0, 0, 0]));
    let inner = imageops::resize(&round, 824, 824, Resize::Lanczos3);
    imageops::overlay(&mut canvas, &inner, 100, 100);
    let mut family = icns::IconFamily::new();
    for (size, ty) in [
        (16u32, icns::IconType::RGBA32_16x16),
        (32, icns::IconType::RGBA32_16x16_2x),
        (64, icns::IconType::RGBA32_32x32_2x),
        (128, icns::IconType::RGBA32_128x128),
        (256, icns::IconType::RGBA32_128x128_2x),
        (512, icns::IconType::RGBA32_256x256_2x),
        (1024, icns::IconType::RGBA32_512x512_2x),
    ] {
        let img = imageops::resize(&canvas, size, size, Resize::Lanczos3);
        family.add_icon_with_type(&icns::Image::from_data(icns::PixelFormat::RGBA, size, size, img.into_raw())?, ty)?;
    }
    let icns_path = out_path(Path::new("packaging/icons/app.icns"))?;
    family.write(BufWriter::new(File::create(&icns_path)?))?;
    println!("  wrote {}", icns_path.display());

    // Small installer badge (Inno Setup wants a BMP; flatten onto white).
    save_bmp(
        &flatten(&imageops::resize(&round, 55, 55, Resize::Lanczos3), [255, 255, 255]),
        Path::new("packaging/windows/wizard_small.bmp"),
    )?;
    Ok(())
}

fn flatten(img: &RgbaImage, bg: [u8; 3]) -> image::RgbImage {
    let mut out = image::RgbImage::new(img.width(), img.height());
    for (x, y, p) in img.enumerate_pixels() {
        let a = f32::from(p[3]) / 255.0;
        let px = out.get_pixel_mut(x, y);
        for c in 0..3 {
            px[c] = (f32::from(p[c]) * a + f32::from(bg[c]) * (1.0 - a)) as u8;
        }
    }
    out
}

fn save_bmp(img: &image::RgbImage, path: &Path) -> Result<()> {
    img.save_with_format(out_path(path)?, image::ImageFormat::Bmp)?;
    println!("  wrote {} ({}x{})", path.display(), img.width(), img.height());
    Ok(())
}

fn save_jpeg(img: &image::RgbImage, path: &Path, quality: u8) -> Result<()> {
    let file = BufWriter::new(File::create(out_path(path)?)?);
    image::codecs::jpeg::JpegEncoder::new_with_quality(file, quality).encode_image(img)?;
    println!("  wrote {} ({}x{})", path.display(), img.width(), img.height());
    Ok(())
}

/// Crop `img` to the aspect ratio `aw:ah`, keeping the horizontal position `focus_x` (0..1).
fn crop_aspect(img: &RgbaImage, aw: u32, ah: u32, focus_x: f32, focus_y: f32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let (cw, ch) = if u64::from(w) * u64::from(ah) > u64::from(h) * u64::from(aw) {
        ((u64::from(h) * u64::from(aw) / u64::from(ah)) as u32, h)
    } else {
        (w, (u64::from(w) * u64::from(ah) / u64::from(aw)) as u32)
    };
    let x = ((w - cw) as f32 * focus_x) as u32;
    let y = ((h - ch) as f32 * focus_y) as u32;
    imageops::crop_imm(img, x, y, cw, ch).to_image()
}

fn banner() -> Result<()> {
    let src = source("banner.png");
    if !src.exists() {
        println!("banner: skip (missing {})", src.display());
        return Ok(());
    }
    println!("banner:");
    let wide = crop_aspect(&load(&src)?, 1600, 460, 0.5, 0.5);
    let wide = imageops::resize(&wide, 1600, 460, Resize::Lanczos3);
    save_jpeg(&flatten(&wide, [255, 240, 246]), &app_asset("banner.jpg"), 88)?;
    save_jpeg(&flatten(&wide, [255, 240, 246]), Path::new("docs/banner.jpg"), 90)?;
    Ok(())
}

fn installer() -> Result<()> {
    let src = source("installer_side.png");
    if !src.exists() {
        println!("installer: skip (missing {})", src.display());
        return Ok(());
    }
    println!("installer:");
    let tall = crop_aspect(&load(&src)?, 164, 314, 0.5, 1.0);
    for (w, h, name) in
        [(164u32, 314u32, "wizard_large.bmp"), (246, 471, "wizard_large_150.bmp"), (328, 628, "wizard_large_200.bmp")]
    {
        let img = imageops::resize(&tall, w, h, Resize::Lanczos3);
        save_bmp(&flatten(&img, [255, 240, 246]), &Path::new("packaging/windows").join(name))?;
    }
    Ok(())
}

/// 150x57 strip shown at the top of the installer pages (NSIS header image).
fn wizard_header() -> Result<()> {
    println!("wizard header:");
    let banner = source("banner.png");
    let img = if banner.exists() {
        let cropped = crop_aspect(&load(&banner)?, 150, 57, 1.0, 0.5);
        imageops::resize(&cropped, 150, 57, Resize::Lanczos3)
    } else {
        RgbaImage::from_fn(150, 57, |x, _| {
            let t = x as f32 / 150.0;
            Rgba([
                (255.0 + (214.0 - 255.0) * t) as u8,
                (226.0 + (234.0 - 226.0) * t) as u8,
                (238.0 + (255.0 - 238.0) * t) as u8,
                255,
            ])
        })
    };
    save_bmp(&flatten(&img, [255, 255, 255]), Path::new("packaging/windows/wizard_header.bmp"))
}

/// Soft pastel background for the macOS disk image (drawn in code; no artwork needed).
fn dmg_background() -> Result<()> {
    println!("dmg background:");
    let (w, h) = (660u32, 400u32);
    let mut img = RgbaImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let t = (x as f32 / w as f32 * 0.55 + y as f32 / h as f32 * 0.45).clamp(0.0, 1.0);
        let (a, b) = ([255.0, 226.0, 238.0], [214.0, 234.0, 255.0]);
        *p = Rgba([(a[0] + (b[0] - a[0]) * t) as u8, (a[1] + (b[1] - a[1]) * t) as u8, (a[2] + (b[2] - a[2]) * t) as u8, 255]);
    }
    // A few translucent petals.
    for (i, (cx, cy, r)) in
        [(80, 60, 14), (560, 330, 18), (610, 70, 10), (40, 340, 12), (330, 45, 9), (460, 200, 7)].into_iter().enumerate()
    {
        let tint = if i % 2 == 0 { [255.0, 158.0, 192.0] } else { [142.0, 203.0, 255.0] };
        for y in -r..=r {
            for x in -(r * 2)..=(r * 2) {
                let d = (x as f32 / 2.0).powi(2) + (y as f32).powi(2);
                if d <= (r * r) as f32 {
                    let (px, py) = ((cx + x) as u32, (cy + y) as u32);
                    if px < w && py < h {
                        let p = img.get_pixel_mut(px, py);
                        for c in 0..3 {
                            p[c] = (f32::from(p[c]) * 0.65 + tint[c] * 0.35) as u8;
                        }
                    }
                }
            }
        }
    }
    save_png(&img, Path::new("packaging/macos/dmg_background.png"))
}

/// Stand-ins so the app builds and looks reasonable before the real artwork exists.
fn placeholders() -> Result<()> {
    println!("placeholders (only where missing):");
    let gradient = |w: u32, h: u32, a: [f32; 3], b: [f32; 3]| {
        RgbaImage::from_fn(w, h, |x, y| {
            let t = (x as f32 / w as f32 + y as f32 / h as f32) / 2.0;
            Rgba([(a[0] + (b[0] - a[0]) * t) as u8, (a[1] + (b[1] - a[1]) * t) as u8, (a[2] + (b[2] - a[2]) * t) as u8, 255])
        })
    };
    for mood in MOODS {
        let path = app_asset(&format!("mascot_{mood}.png"));
        if path.exists() {
            continue;
        }
        // A round pink "face" with two eyes on a transparent background.
        let (w, h) = (320u32, 440u32);
        let mut img = RgbaImage::new(w, h);
        let (cx, cy, r) = (160.0f32, 150.0f32, 110.0f32);
        for (x, y, p) in img.enumerate_pixels_mut() {
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            let body = (x as f32 - cx).abs() < 70.0 && y > 240 && y < 430;
            if d < r || body {
                *p = Rgba([255, 200, 220, 255]);
            }
            for ex in [cx - 40.0, cx + 40.0] {
                if ((x as f32 - ex).powi(2) + (y as f32 - cy).powi(2)).sqrt() < 16.0 {
                    *p = Rgba([60, 90, 140, 255]);
                }
            }
        }
        save_png(&img, &path)?;
    }
    let banner = app_asset("banner.jpg");
    if !banner.exists() {
        let img = gradient(1600, 460, [255.0, 226.0, 238.0], [200.0, 225.0, 255.0]);
        save_jpeg(&flatten(&img, [255, 255, 255]), &banner, 85)?;
    }
    let icon = app_asset("icon_256.png");
    if !icon.exists() {
        save_png(&round_corners(&gradient(256, 256, [255.0, 158.0, 192.0], [142.0, 203.0, 255.0]), 0.2237), &icon)?;
    }
    Ok(())
}
