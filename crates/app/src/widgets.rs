//! Small reusable widgets and formatters.

use std::collections::VecDeque;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Response, RichText, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
};

use crate::theme::{self, ACCENT, ACCENT_DEEP, SECONDARY, pal};

pub fn primary_button(ui: &mut Ui, text: impl Into<String>) -> Response {
    let text: String = text.into();
    ui.add(
        egui::Button::new(RichText::new(text).strong().color(Color32::WHITE))
            .fill(ACCENT_DEEP)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(255))
            .min_size(Vec2::new(0.0, 36.0)),
    )
}

pub fn secondary_button(ui: &mut Ui, text: impl Into<String>) -> Response {
    let text: String = text.into();
    ui.add(egui::Button::new(RichText::new(text)).corner_radius(CornerRadius::same(255)).min_size(Vec2::new(0.0, 34.0)))
}

pub fn danger_button(ui: &mut Ui, text: impl Into<String>) -> Response {
    let text: String = text.into();
    ui.add(
        egui::Button::new(RichText::new(text).color(theme::BAD))
            .stroke(Stroke::new(1.0, theme::BAD.gamma_multiply(0.6)))
            .corner_radius(CornerRadius::same(255))
            .min_size(Vec2::new(0.0, 34.0)),
    )
}

/// A small rounded label such as `VIP`.
pub fn pill(ui: &mut Ui, text: &str, fill: Color32, fg: Color32) {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(11.5), fg);
    let size = galley.size() + Vec2::new(12.0, 4.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(255), fill);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, fg);
}

pub fn section_title(ui: &mut Ui, icon: &str, text: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(icon).size(20.0).color(ACCENT_DEEP));
        ui.label(RichText::new(text).size(17.0).strong());
    });
}

pub fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).small().color(pal(ui).muted));
}

/// Sidebar navigation entry; returns the click response.
pub fn nav_button(ui: &mut Ui, icon: &str, label: &str, selected: bool, badge: Option<usize>) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::click());
    let p = pal(ui);
    let hovered = resp.hovered();
    if selected {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(14),
            ACCENT.gamma_multiply(if ui.visuals().dark_mode { 0.5 } else { 0.42 }),
        );
        ui.painter().rect_filled(
            Rect::from_min_size(rect.left_center() + Vec2::new(-1.0, -11.0), Vec2::new(4.0, 22.0)),
            CornerRadius::same(2),
            ACCENT_DEEP,
        );
    } else if hovered {
        ui.painter().rect_filled(rect, CornerRadius::same(14), p.card_alt);
    }
    let fg = if selected { p.text } else { p.muted };
    ui.painter().text(
        rect.left_center() + Vec2::new(18.0, 0.0),
        Align2::LEFT_CENTER,
        icon,
        FontId::proportional(20.0),
        if selected { ACCENT_DEEP } else { fg },
    );
    ui.painter().text(rect.left_center() + Vec2::new(50.0, 0.0), Align2::LEFT_CENTER, label, FontId::proportional(15.5), fg);
    if let Some(n) = badge.filter(|n| *n > 0) {
        let text = if n > 99 { "99+".to_owned() } else { n.to_string() };
        let galley = ui.painter().layout_no_wrap(text, FontId::proportional(11.5), Color32::WHITE);
        let size = Vec2::new(galley.size().x.max(14.0) + 10.0, 20.0);
        let r = Rect::from_center_size(rect.right_center() - Vec2::new(size.x / 2.0 + 12.0, 0.0), size);
        ui.painter().rect_filled(r, CornerRadius::same(255), ACCENT_DEEP);
        ui.painter().galley(r.center() - galley.size() / 2.0, galley, Color32::WHITE);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Rounded progress bar with a pink→blue fill.
pub fn progress_bar(ui: &mut Ui, fraction: f32, height: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    let p = pal(ui);
    ui.painter().rect_filled(rect, CornerRadius::same(255), p.line);
    let f = fraction.clamp(0.0, 1.0);
    if f > 0.0 {
        let w = (rect.width() * f).max(height);
        let fill = Rect::from_min_size(rect.min, Vec2::new(w, rect.height()));
        theme::rounded_gradient(ui.painter(), fill, height / 2.0, ACCENT, ACCENT.lerp_to_gamma(SECONDARY, 0.75), true);
    }
}

/// Filled area chart for the throughput history.
pub fn sparkline(ui: &mut Ui, values: &VecDeque<f32>, size: Vec2) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = pal(ui);
    ui.painter().rect_filled(rect, CornerRadius::same(12), p.card_alt);
    if values.len() < 2 {
        return;
    }
    let max = values.iter().copied().fold(1.0f32, f32::max) * 1.15;
    let n = values.len();
    let pts: Vec<Pos2> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            Pos2::new(
                rect.left() + rect.width() * i as f32 / (n - 1) as f32,
                rect.bottom() - 4.0 - (rect.height() - 8.0) * (v / max),
            )
        })
        .collect();
    let mut area = pts.clone();
    area.push(Pos2::new(rect.right(), rect.bottom()));
    area.push(Pos2::new(rect.left(), rect.bottom()));
    let painter = ui.painter_at(rect);
    painter.add(Shape::convex_polygon(area, ACCENT.gamma_multiply(0.25), Stroke::NONE));
    painter.add(Shape::line(pts, Stroke::new(2.0, ACCENT_DEEP)));
    painter.rect_stroke(rect, CornerRadius::same(12), Stroke::new(1.0, p.line), StrokeKind::Inside);
}

pub fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

pub fn format_speed(bytes_per_sec: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec.max(0.0) as u64))
}

pub fn format_duration_ms(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

pub fn format_eta(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--".into();
    }
    let s = secs as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_duration_ms(326_000), "5:26");
        assert_eq!(format_eta(75.0), "1:15");
        assert_eq!(format_eta(f64::INFINITY), "--");
        assert_eq!(format_eta(3725.0), "1:02:05");
    }
}
