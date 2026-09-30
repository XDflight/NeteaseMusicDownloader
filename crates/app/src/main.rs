#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod assets;
mod backend;
mod fonts;
mod state;
mod theme;
mod ui_about;
mod ui_browse;
mod ui_login;
mod ui_queue;
mod ui_settings;
mod widgets;

use eframe::egui;

fn main() -> eframe::Result {
    init_logging();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("云音下载姬 · 网易云音乐批量下载器")
            .with_app_id("netease-music-downloader")
            .with_inner_size([1120.0, 760.0])
            .with_min_inner_size([940.0, 620.0])
            .with_icon(assets::window_icon()),
        ..Default::default()
    };
    eframe::run_native("云音下载姬", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}

/// Log to stderr in debug builds and to `<data dir>/logs/app.log` otherwise.
fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,ncm_api=warn,ncm_update=info"));

    #[cfg(debug_assertions)]
    {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    #[cfg(not(debug_assertions))]
    {
        let dir = ncm_core::AppPaths::discover().data_dir.join("logs");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("app.log");
        // Keep the log from growing without bound.
        if std::fs::metadata(&path).map(|m| m.len() > 2 * 1024 * 1024).unwrap_or(false) {
            let _ = std::fs::rename(&path, dir.join("app.old.log"));
        }
        if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(std::sync::Mutex::new(file)).init();
            std::panic::set_hook(Box::new(|info| tracing::error!("panic: {info}")));
        }
    }
}
