//! Embedded artwork, decoded once into GPU textures.

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Mood {
    Idle,
    Working,
    Happy,
    Sad,
    Login,
    Search,
}

pub struct Assets {
    pub idle: TextureHandle,
    pub working: TextureHandle,
    pub happy: TextureHandle,
    pub sad: TextureHandle,
    pub login: TextureHandle,
    pub search: TextureHandle,
    pub banner: TextureHandle,
    pub logo: TextureHandle,
}

fn decode(ctx: &egui::Context, name: &str, bytes: &[u8]) -> TextureHandle {
    let img = image::load_from_memory(bytes).unwrap_or_else(|e| panic!("embedded asset {name} is corrupt: {e}")).to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    ctx.load_texture(name, ColorImage::from_rgba_unmultiplied(size, img.as_raw()), TextureOptions::LINEAR)
}

impl Assets {
    pub fn load(ctx: &egui::Context) -> Self {
        Self {
            idle: decode(ctx, "idle", include_bytes!("../assets/mascot_idle.png")),
            working: decode(ctx, "working", include_bytes!("../assets/mascot_working.png")),
            happy: decode(ctx, "happy", include_bytes!("../assets/mascot_happy.png")),
            sad: decode(ctx, "sad", include_bytes!("../assets/mascot_sad.png")),
            login: decode(ctx, "login", include_bytes!("../assets/mascot_login.png")),
            search: decode(ctx, "search", include_bytes!("../assets/mascot_search.png")),
            banner: decode(ctx, "banner", include_bytes!("../assets/banner.jpg")),
            logo: decode(ctx, "logo", include_bytes!("../assets/icon_256.png")),
        }
    }

    pub fn mascot(&self, mood: Mood) -> &TextureHandle {
        match mood {
            Mood::Idle => &self.idle,
            Mood::Working => &self.working,
            Mood::Happy => &self.happy,
            Mood::Sad => &self.sad,
            Mood::Login => &self.login,
            Mood::Search => &self.search,
        }
    }
}

/// Window / taskbar icon.
pub fn window_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon_256.png")).expect("embedded icon is a valid PNG")
}
