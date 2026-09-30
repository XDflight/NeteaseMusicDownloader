//! "Vinyl Red" design language: NetEase-style red, white and black with warm amber accents,
//! generous rounding and soft cards.

use eframe::egui::{
    self, Color32, CornerRadius, Margin, Mesh, Pos2, Rect, Shadow, Shape, Stroke, TextStyle, Theme, Vec2, epaint::Vertex,
};

pub const ACCENT: Color32 = Color32::from_rgb(0xFF, 0x54, 0x68);
pub const ACCENT_DEEP: Color32 = Color32::from_rgb(0xD9, 0x20, 0x2F);
pub const ACCENT_SOFT: Color32 = Color32::from_rgb(0xFF, 0xE3, 0xE6);
pub const SECONDARY: Color32 = Color32::from_rgb(0xFF, 0xB3, 0x6B);
pub const SECONDARY_DEEP: Color32 = Color32::from_rgb(0xF2, 0x8B, 0x30);
pub const GOOD: Color32 = Color32::from_rgb(0x4C, 0xC3, 0x8A);
pub const WARN: Color32 = Color32::from_rgb(0xF5, 0xA6, 0x23);
pub const BAD: Color32 = Color32::from_rgb(0xE5, 0x48, 0x4D);

pub const RADIUS: u8 = 14;

/// Colours that differ between light and dark mode.
#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub card: Color32,
    pub card_alt: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub line: Color32,
}

pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bg: Color32::from_rgb(0x17, 0x12, 0x14),
            card: Color32::from_rgb(0x21, 0x1A, 0x1D),
            card_alt: Color32::from_rgb(0x2B, 0x22, 0x26),
            text: Color32::from_rgb(0xF6, 0xEE, 0xEE),
            muted: Color32::from_rgb(0xB0, 0xA0, 0xA4),
            line: Color32::from_rgb(0x3A, 0x2D, 0x31),
        }
    } else {
        Palette {
            bg: Color32::from_rgb(0xFF, 0xF7, 0xF6),
            card: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            card_alt: Color32::from_rgb(0xFF, 0xF0, 0xEF),
            text: Color32::from_rgb(0x3A, 0x2A, 0x2E),
            muted: Color32::from_rgb(0x8C, 0x7A, 0x7E),
            line: Color32::from_rgb(0xF3, 0xDC, 0xDC),
        }
    }
}

pub fn pal(ui: &egui::Ui) -> Palette {
    palette(ui.visuals().dark_mode)
}

fn visuals(dark: bool) -> egui::Visuals {
    let p = palette(dark);
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.extreme_bg_color = if dark { Color32::from_rgb(0x1C, 0x15, 0x18) } else { Color32::from_rgb(0xFF, 0xFA, 0xFA) };
    v.faint_bg_color = p.card_alt;
    v.override_text_color = Some(p.text);
    v.hyperlink_color = if dark { Color32::from_rgb(0x7F, 0xB2, 0xFF) } else { Color32::from_rgb(0x2F, 0x6F, 0xDE) };
    v.selection.bg_fill = ACCENT.gamma_multiply(if dark { 0.55 } else { 0.5 });
    v.selection.stroke = Stroke::new(1.0, ACCENT_DEEP);
    v.window_corner_radius = CornerRadius::same(18);
    v.menu_corner_radius = CornerRadius::same(12);
    v.window_stroke = Stroke::new(1.0, p.line);
    v.window_shadow =
        Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(if dark { 110 } else { 38 }) };
    v.popup_shadow = v.window_shadow;

    let r = CornerRadius::same(RADIUS);
    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    w.noninteractive.corner_radius = r;
    for (state, fill, stroke) in [
        (&mut w.inactive, p.card_alt, Stroke::new(1.0, p.line)),
        (&mut w.hovered, ACCENT_SOFT.gamma_multiply(if dark { 0.35 } else { 1.0 }), Stroke::new(1.5, ACCENT)),
        (&mut w.active, ACCENT.gamma_multiply(if dark { 0.55 } else { 0.7 }), Stroke::new(1.5, ACCENT_DEEP)),
        (&mut w.open, p.card_alt, Stroke::new(1.0, ACCENT)),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = stroke;
        state.corner_radius = r;
        state.fg_stroke.color = p.text;
    }
    v
}

pub fn apply(ctx: &egui::Context) {
    ctx.set_visuals_of(Theme::Light, visuals(false));
    ctx.set_visuals_of(Theme::Dark, visuals(true));
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = Vec2::new(10.0, 9.0);
        style.spacing.button_padding = Vec2::new(14.0, 7.0);
        style.spacing.interact_size.y = 32.0;
        style.spacing.window_margin = Margin::same(16);
        style.spacing.scroll.bar_width = 8.0;
        style.spacing.scroll.floating = true;
        style.text_styles.insert(TextStyle::Heading, egui::FontId::proportional(26.0));
        style.text_styles.insert(TextStyle::Body, egui::FontId::proportional(15.0));
        style.text_styles.insert(TextStyle::Button, egui::FontId::proportional(15.0));
        style.text_styles.insert(TextStyle::Small, egui::FontId::proportional(12.5));
        style.text_styles.insert(TextStyle::Monospace, egui::FontId::monospace(13.5));
    });
}

pub fn card_frame(ui: &egui::Ui) -> egui::Frame {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.line))
        .corner_radius(CornerRadius::same(18))
        .inner_margin(Margin::same(16))
        .shadow(Shadow {
            offset: [0, 4],
            blur: 16,
            spread: 0,
            color: Color32::from_black_alpha(if ui.visuals().dark_mode { 70 } else { 14 }),
        })
}

/// Horizontal (or diagonal) colour gradient filling `rect`.
pub fn gradient_rect(painter: &egui::Painter, rect: Rect, corner: f32, colors: [Color32; 4]) {
    // Rounded gradients are approximated by drawing the gradient mesh and masking the corners
    // with the surrounding background colour is not possible here, so square meshes are used for
    // the interior and rounded solid caps for the corners.
    let mut mesh = Mesh::default();
    let uv = egui::epaint::WHITE_UV;
    let pts = [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()];
    for (p, c) in pts.iter().zip(colors) {
        mesh.vertices.push(Vertex { pos: *p, uv, color: c });
    }
    mesh.indices.extend([0, 1, 2, 0, 2, 3]);
    let _ = corner;
    painter.add(Shape::mesh(mesh));
}

/// Linear gradient (top to bottom, or left to right) inside a rounded rectangle (a fan of vertex-coloured triangles).
pub fn rounded_gradient(painter: &egui::Painter, rect: Rect, radius: f32, start: Color32, end: Color32, horizontal: bool) {
    let r = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let corners = [
        (rect.right() - r, rect.top() + r, -90.0f32),
        (rect.right() - r, rect.bottom() - r, 0.0),
        (rect.left() + r, rect.bottom() - r, 90.0),
        (rect.left() + r, rect.top() + r, 180.0),
    ];
    let mut outline = Vec::new();
    for (cx, cy, start) in corners {
        for i in 0..=8 {
            let a = (start + 90.0 * i as f32 / 8.0).to_radians();
            outline.push(Pos2::new(cx + r * a.cos(), cy + r * a.sin()));
        }
    }
    let color_at = |p: Pos2| {
        let t = if horizontal { (p.x - rect.left()) / rect.width() } else { (p.y - rect.top()) / rect.height() };
        start.lerp_to_gamma(end, t.clamp(0.0, 1.0))
    };
    let uv = egui::epaint::WHITE_UV;
    let mut mesh = Mesh::default();
    mesh.vertices.push(Vertex { pos: rect.center(), uv, color: color_at(rect.center()) });
    for p in &outline {
        mesh.vertices.push(Vertex { pos: *p, uv, color: color_at(*p) });
    }
    for i in 0..outline.len() {
        mesh.indices.extend([0, 1 + i as u32, 1 + ((i + 1) % outline.len()) as u32]);
    }
    painter.add(Shape::mesh(mesh));
}

/// Pastel backdrop for a mascot sprite; it also hides the small white gaps between hair strands
/// that the cut-out keeps, so the sprite looks right in dark mode as well.
pub fn mascot_stage(ui: &egui::Ui, rect: Rect, time: f64) {
    rounded_gradient(ui.painter(), rect, 22.0, ACCENT_SOFT, Color32::from_rgb(0xFF, 0xF4, 0xEC), false);
    ui.painter().rect_stroke(rect, CornerRadius::same(22), Stroke::new(1.0, pal(ui).line), egui::StrokeKind::Inside);
    // Two soft clouds.
    let painter = ui.painter_at(rect);
    for (fx, fy, s) in [(0.22f32, 0.16f32, 1.0f32), (0.8, 0.3, 0.7)] {
        let c = Pos2::new(rect.left() + rect.width() * fx, rect.top() + rect.height() * fy);
        let white = Color32::from_white_alpha(150);
        for (dx, dy, r) in [(-14.0f32, 4.0f32, 9.0f32), (0.0, -3.0, 12.0), (15.0, 3.0, 10.0), (0.0, 6.0, 10.0)] {
            painter.circle_filled(c + Vec2::new(dx * s, dy * s), r * s, white);
        }
    }
    sakura_petals(ui, rect, time, 7);
}

/// Little translucent petals drifting across `rect`; call every frame (requests repaints).
pub fn sakura_petals(ui: &egui::Ui, rect: Rect, time: f64, count: usize) {
    let painter = ui.painter_at(rect);
    for i in 0..count {
        let f = i as f64;
        let speed = 10.0 + (f * 7.3) % 14.0;
        let sway = 14.0 + (f * 3.1) % 18.0;
        let t = time * speed / 100.0 + f * 0.137;
        let x = rect.left() as f64 + ((f * 61.7) % rect.width() as f64) + (time * 0.9 + f).sin() * sway;
        let progress = (t + f * 0.31).fract();
        let y = rect.top() as f64 - 10.0 + progress * (rect.height() as f64 + 20.0);
        let size = 4.0 + (f * 1.7) % 5.0;
        let angle = time * (0.6 + (f % 3.0) * 0.3) + f;
        let alpha = (200.0 * (1.0 - (progress - 0.5).abs() * 1.4).clamp(0.15, 1.0)) as u8;
        let color = if i % 4 == 0 {
            Color32::from_rgba_unmultiplied(255, 255, 255, alpha)
        } else {
            Color32::from_rgba_unmultiplied(255, 110, 130, alpha)
        };
        petal(&painter, Pos2::new(x as f32, y as f32), size as f32, angle as f32, color);
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
}

fn petal(painter: &egui::Painter, center: Pos2, size: f32, angle: f32, color: Color32) {
    // A heart-ish petal: an ellipse with a notch, rotated by `angle`.
    let (s, c) = angle.sin_cos();
    let pts: Vec<Pos2> = (0..14)
        .map(|k| {
            let a = k as f32 / 14.0 * std::f32::consts::TAU;
            let (mut x, mut y) = (a.cos() * size, a.sin() * size * 1.5);
            if a.sin() > 0.85 {
                y -= size * 0.5 * (a.sin() - 0.85);
            }
            x *= 0.7 + 0.3 * (a * 2.0).cos().abs();
            Pos2::new(center.x + x * c - y * s, center.y + x * s + y * c)
        })
        .collect();
    painter.add(Shape::convex_polygon(pts, color, Stroke::NONE));
}
