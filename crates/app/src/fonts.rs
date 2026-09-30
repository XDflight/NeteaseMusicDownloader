//! Font setup: the bundled Latin font, a system CJK font (none is shipped with egui) and the
//! Phosphor icon font.

use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

const CJK_CANDIDATES: &[&str] = &[
    // Windows
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\msyhl.ttc",
    r"C:\Windows\Fonts\Deng.ttf",
    r"C:\Windows\Fonts\simhei.ttf",
    r"C:\Windows\Fonts\simsun.ttc",
    // macOS
    "/System/Library/Fonts/PingFang.ttc",
    "/System/Library/Fonts/STHeiti Medium.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/Library/Fonts/Arial Unicode.ttf",
    // Linux
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/opentype/source-han-sans/SourceHanSansSC-Regular.otf",
    "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
    "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
];

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);

    match CJK_CANDIDATES.iter().find_map(|p| std::fs::read(p).ok().map(|b| (p, b))) {
        Some((path, bytes)) => {
            tracing::info!("CJK font: {path}");
            fonts.font_data.insert("cjk".into(), Arc::new(FontData::from_owned(bytes)));
            // Right after the primary Latin font so Latin text keeps its look while CJK glyphs
            // fall through to the system font.
            for family in [FontFamily::Proportional, FontFamily::Monospace] {
                let list = fonts.families.entry(family).or_default();
                let at = 1.min(list.len());
                list.insert(at, "cjk".into());
            }
        }
        None => tracing::warn!("no CJK font found; Chinese text will not render correctly"),
    }
    ctx.set_fonts(fonts);
}
