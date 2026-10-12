use std::sync::Arc;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Galley, Layout, Rect, RichText, ScrollArea, Sense, Stroke, Vec2,
    text::LayoutJob,
};
use ncm_api::{Availability, Level, PlaylistSummary, ResourceKind, Track};

use crate::app::App;
use crate::state::*;
use crate::theme::{self, ACCENT, ACCENT_DEEP, pal};
use crate::widgets::{self, format_duration_ms, primary_button, secondary_button};

const ROW_H: f32 = 40.0;

impl App {
    pub fn page_browse(&mut self, ui: &mut egui::Ui) {
        self.page_header(ui, "解析与选择", "粘贴歌单 / 专辑 / 单曲链接，勾选想要的歌曲");
        ui.add_space(10.0);
        self.input_bar(ui);
        ui.add_space(6.0);

        if let Some(err) = self.browse.error.clone() {
            theme::card_frame(ui).stroke(Stroke::new(1.5, theme::BAD)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(egui_phosphor::regular::WARNING_CIRCLE).size(20.0).color(theme::BAD));
                    ui.label(err);
                });
            });
            ui.add_space(6.0);
        }

        if self.browse.loading {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.spinner();
                ui.label("正在解析…（歌曲很多时需要几秒钟）");
            });
        } else if self.browse.view.is_some() {
            self.collection_panel(ui);
        } else {
            self.landing(ui);
        }
    }

    fn input_bar(&mut self, ui: &mut egui::Ui) {
        let mut submit = false;
        theme::card_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(egui_phosphor::regular::LINK).size(20.0).color(ACCENT_DEEP));
                let te = egui::TextEdit::singleline(&mut self.browse.input)
                    .hint_text("粘贴歌单 / 专辑 / 单曲链接、分享文字或 ID（网易云 App 的分享文本可直接粘贴）")
                    .desired_width((ui.available_width() - 262.0).max(200.0))
                    .margin(egui::Margin::symmetric(10, 8));
                let resp = ui.add(te);
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                egui::ComboBox::from_id_salt("bare-kind")
                    .width(96.0)
                    .selected_text(match self.browse.bare_kind {
                        ResourceKind::Playlist => "数字=歌单",
                        ResourceKind::Album => "数字=专辑",
                        ResourceKind::Song => "数字=单曲",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.browse.bare_kind, ResourceKind::Playlist, "纯数字按歌单");
                        ui.selectable_value(&mut self.browse.bare_kind, ResourceKind::Album, "纯数字按专辑");
                        ui.selectable_value(&mut self.browse.bare_kind, ResourceKind::Song, "纯数字按单曲");
                    });
                if primary_button(ui, "解析").clicked() {
                    submit = true;
                }
            });
        });
        if submit && !self.browse.input.trim().is_empty() {
            self.browse.request_seq += 1;
            self.browse.loading = true;
            self.browse.error = None;
            self.be.load_input(self.browse.request_seq, self.browse.input.clone(), self.browse.bare_kind);
        }
    }

    fn landing(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        let logged_in = self.is_logged_in();
        if !logged_in {
            theme::card_frame(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("先登录，再下载更多").size(18.0).strong());
                        widgets::hint(ui, "未登录也可以下载免费歌曲；登录后可下载 VIP 歌曲（需你的账号是会员）和私有歌单。");
                        ui.add_space(4.0);
                        if primary_button(ui, "扫码登录").clicked() {
                            self.open_login();
                        }
                    });
                });
            });
            ui.add_space(8.0);
        }

        widgets::section_title(ui, egui_phosphor::regular::HEADPHONES, "我的歌单");
        let mut open: Option<u64> = None;
        // Covers to fetch: the cards that are on screen and have not asked for theirs yet.
        let mut want_covers: Vec<(u64, String)> = Vec::new();
        match &self.browse.my {
            Loadable::Idle if !logged_in => widgets::hint(ui, "登录后，这里会列出你创建和收藏的所有歌单（包括私有歌单）。"),
            Loadable::Idle | Loadable::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("正在加载歌单…");
                });
            }
            Loadable::Failed(e) => {
                ui.label(RichText::new(format!("加载歌单失败：{e}")).color(theme::BAD));
            }
            Loadable::Ready(list) => {
                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    let width = ui.available_width();
                    let cols = ((width / 270.0).floor() as usize).max(1);
                    let card_w = (width - (cols as f32 - 1.0) * 10.0) / cols as f32;
                    for row in list.chunks(cols) {
                        ui.horizontal(|ui| {
                            for pl in row {
                                let cover = match self.browse.my_covers.get(&pl.id) {
                                    Some(CoverState::Ready(tex)) => Some(tex),
                                    _ => None,
                                };
                                let (clicked, visible) = playlist_card(ui, pl, card_w, cover);
                                if clicked {
                                    open = Some(pl.id);
                                }
                                if let Some(url) = &pl.cover_url
                                    && visible
                                    && !self.browse.my_covers.contains_key(&pl.id)
                                {
                                    want_covers.push((pl.id, thumbnail_url(url, COVER_THUMB_PX)));
                                }
                            }
                        });
                    }
                    if list.is_empty() {
                        widgets::hint(ui, "没有找到歌单。");
                    }
                });
            }
        }
        for (id, url) in want_covers {
            self.browse.my_covers.insert(id, CoverState::Loading);
            self.be.fetch_playlist_cover(id, url);
        }
        if let Some(id) = open {
            self.browse.request_seq += 1;
            self.browse.loading = true;
            self.browse.error = None;
            self.be.load_playlist(self.browse.request_seq, id);
        }
    }

    fn collection_panel(&mut self, ui: &mut egui::Ui) {
        let logged_in = self.is_logged_in();
        let mut back = false;
        let mut download: Option<Vec<usize>> = None;
        let mut quality_changed = false;
        let mut level = self.settings.download.level;

        let Some(view) = self.browse.view.as_mut() else { return };
        let p = pal(ui);

        // Header.
        theme::card_frame(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                match &view.cover {
                    Some(tex) => {
                        ui.add(egui::Image::new(tex).fit_to_exact_size(Vec2::splat(76.0)).corner_radius(CornerRadius::same(14)));
                    }
                    None => {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(76.0), Sense::hover());
                        ui.painter().rect_filled(rect, CornerRadius::same(14), p.card_alt);
                        ui.painter().text(
                            rect.center(),
                            Align2::CENTER_CENTER,
                            egui_phosphor::regular::MUSIC_NOTES,
                            FontId::proportional(30.0),
                            ACCENT,
                        );
                    }
                }
                ui.vertical(|ui| {
                    ui.label(RichText::new(&view.col.name).size(20.0).strong());
                    let kind = match view.col.kind {
                        ncm_api::CollectionKind::Playlist => "歌单",
                        ncm_api::CollectionKind::Album => "专辑",
                        ncm_api::CollectionKind::Song => "单曲",
                    };
                    let who = if view.col.creator.is_empty() { String::new() } else { format!(" · {}", view.col.creator) };
                    widgets::hint(ui, &format!("{kind}{who} · 共 {} 首", view.col.tracks.len()));
                    let unavailable = view.col.tracks.iter().filter(|t| t.availability() != Availability::Ok).count();
                    if unavailable > 0 {
                        widgets::hint(
                            ui,
                            &format!(
                                "其中 {unavailable} 首当前账号可能无法下载（VIP / 无版权）{}",
                                if logged_in { "" } else { "——登录后 VIP 歌曲可能可用" }
                            ),
                        );
                    }
                });
                ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                    if secondary_button(ui, format!("{} 返回", egui_phosphor::regular::ARROW_LEFT)).clicked() {
                        back = true;
                    }
                });
            });
        });
        ui.add_space(4.0);

        // Toolbar.
        ui.horizontal_wrapped(|ui| {
            let te = egui::TextEdit::singleline(&mut view.filter)
                .hint_text("筛选歌曲 / 歌手 / 专辑")
                .desired_width(190.0)
                .margin(egui::Margin::symmetric(8, 6));
            if ui.add(te).changed() {
                view.refilter();
            }
            if secondary_button(ui, "全选").clicked() {
                for &i in &view.visible {
                    view.selected[i] = true;
                }
            }
            if secondary_button(ui, "全不选").clicked() {
                for &i in &view.visible {
                    view.selected[i] = false;
                }
            }
            if secondary_button(ui, "反选").clicked() {
                for &i in &view.visible {
                    view.selected[i] = !view.selected[i];
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let n = view.selected_count();
                ui.add_enabled_ui(n > 0, |ui| {
                    if primary_button(ui, format!("{} 下载选中 ({n})", egui_phosphor::regular::DOWNLOAD_SIMPLE)).clicked() {
                        download = Some((0..view.selected.len()).filter(|&i| view.selected[i]).collect());
                    }
                });
                if secondary_button(ui, "全部下载").clicked() {
                    download = Some((0..view.col.tracks.len()).collect());
                }
                egui::ComboBox::from_id_salt("quality").width(150.0).selected_text(format!("音质：{}", level.label())).show_ui(
                    ui,
                    |ui| {
                        for l in Level::ALL {
                            ui.selectable_value(&mut level, l, format!("{} · {}", l.label(), l.hint()));
                        }
                    },
                );
            });
        });
        quality_changed |= level != self.settings.download.level;
        ui.add_space(2.0);

        // Table.
        let view = self.browse.view.as_mut().expect("checked above");
        if track_table(ui, view, logged_in, &mut self.settings.track_columns) {
            self.save_settings();
        }

        if quality_changed {
            self.settings.download.level = level;
            self.save_settings();
        }
        if back {
            self.browse.view = None;
            self.browse.error = None;
        }
        if let Some(picks) = download {
            let vip_locked =
                self.browse.view.as_ref().map(|v| picks.iter().filter(|&&i| v.col.tracks[i].is_vip_only()).count()).unwrap_or(0);
            self.submit_download(picks);
            if vip_locked > 0 && !logged_in {
                self.toasts.push(ToastKind::Warning, format!("其中 {vip_locked} 首是 VIP 歌曲，未登录时会失败"));
            }
        }
    }
}

/// Edge length of the cover thumbnails requested from the image server: the card shows them at
/// 44 points, so this stays sharp on high-density screens.
const COVER_THUMB_PX: u32 = 120;

/// The image server resizes on request: `url` with its size parameter replaced.
fn thumbnail_url(url: &str, px: u32) -> String {
    let base = url.split('?').next().unwrap_or(url);
    format!("{base}?param={px}y{px}")
}

/// One card of "my playlists". Returns whether it was clicked and whether it is on screen.
fn playlist_card(ui: &mut egui::Ui, pl: &PlaylistSummary, width: f32, cover: Option<&egui::TextureHandle>) -> (bool, bool) {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 68.0), Sense::click());
    let visible = ui.is_rect_visible(rect);
    let p = pal(ui);
    let hovered = resp.hovered();
    ui.painter().rect(
        rect,
        CornerRadius::same(16),
        if hovered { theme::ACCENT_SOFT.gamma_multiply(if ui.visuals().dark_mode { 0.3 } else { 1.0 }) } else { p.card },
        Stroke::new(if hovered { 1.5 } else { 1.0 }, if hovered { ACCENT } else { p.line }),
        egui::StrokeKind::Inside,
    );
    let icon_rect = Rect::from_min_size(rect.min + Vec2::new(12.0, 12.0), Vec2::splat(44.0));
    let liked = pl.special_type == 5;
    match cover {
        Some(tex) => {
            egui::Image::new(tex)
                .fit_to_exact_size(icon_rect.size())
                .corner_radius(CornerRadius::same(12))
                .paint_at(ui, icon_rect);
            if liked {
                // The "liked songs" list keeps its heart as a badge on the cover.
                let badge = icon_rect.right_bottom() - Vec2::splat(5.0);
                ui.painter().circle_filled(badge, 9.0, ACCENT);
                ui.painter().circle_stroke(badge, 9.0, Stroke::new(1.5, p.card));
                ui.painter().text(
                    badge,
                    Align2::CENTER_CENTER,
                    egui_phosphor::regular::HEART,
                    FontId::proportional(11.0),
                    Color32::WHITE,
                );
            }
        }
        // No cover yet (still loading, failed, or the playlist has none): the icon.
        None => {
            ui.painter().rect_filled(icon_rect, CornerRadius::same(12), if liked { ACCENT } else { p.card_alt });
            let icon = if liked { egui_phosphor::regular::HEART } else { egui_phosphor::regular::MUSIC_NOTES };
            ui.painter().text(
                icon_rect.center(),
                Align2::CENTER_CENTER,
                icon,
                FontId::proportional(22.0),
                if liked { Color32::WHITE } else { ACCENT_DEEP },
            );
        }
    }
    let text_x = icon_rect.right() + 12.0;
    let name = elide(ui, &pl.name, FontId::proportional(15.0), p.text, rect.right() - text_x - 12.0);
    ui.painter().galley(egui::pos2(text_x, rect.top() + 13.0), name, p.text);
    let mut sub = format!("{} 首", pl.track_count);
    if pl.subscribed && !pl.creator_name.is_empty() {
        sub.push_str(&format!(" · {}", pl.creator_name));
    }
    if pl.is_private() {
        sub.push_str(" · 私有");
    }
    let sub = elide(ui, &sub, FontId::proportional(12.0), p.muted, rect.right() - text_x - 12.0);
    ui.painter().galley(egui::pos2(text_x, rect.top() + 38.0), sub, p.muted);
    (resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked(), visible)
}

/// One-line text that ends in an ellipsis when it does not fit `width`.
pub fn elide(ui: &egui::Ui, text: &str, font: FontId, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap =
        egui::text::TextWrapping {
            max_width: width.max(8.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…')
        };
    ui.painter().layout_job(job)
}

/// A flexible column of the track table is never dragged narrower than this.
const MIN_COLUMN_PX: f32 = 70.0;

/// Move the boundary right of flexible column `i` (0 = title, 1 = artist) by `dx` pixels in a region
/// `flex` pixels wide: the column on its left grows by what the one on its right gives up, or the
/// other way round. The shares keep adding up to 1 and neither column gets narrower than `MIN_COLUMN_PX`.
fn drag_boundary(shares: &mut [f32; 3], i: usize, dx: f32, flex: f32) {
    let min = (MIN_COLUMN_PX / flex).min(0.3);
    let pair = shares[i] + shares[i + 1];
    let left = (shares[i] + dx / flex).max(min).min((pair - min).max(min));
    shares[i] = left;
    shares[i + 1] = pair - left;
}

/// The track table. `columns` are the shares of the title, artist and album columns; dragging the
/// dividers in the heading changes them (double-click restores the default). Returns `true` when a
/// drag has just ended, so the caller can save the new widths.
fn track_table(ui: &mut egui::Ui, view: &mut CollectionView, logged_in: bool, columns: &mut [f32; 3]) -> bool {
    let p = pal(ui);
    let total_w = ui.available_width();
    // Column layout: [check 34][# 44][title flex][artist][album][time 52][badge 70]
    let fixed = 34.0 + 44.0 + 52.0 + 76.0;
    let flex = (total_w - fixed - 24.0).max(240.0);
    let widths = |c: &[f32; 3]| (flex * c[0], flex * c[1], flex * c[2]);

    // Column headings, with a draggable divider after the title and after the artist column.
    let (head, _) = ui.allocate_exact_size(Vec2::new(total_w, 26.0), Sense::hover());
    let hx = |x: f32| head.left() + x;
    let mut resized = false;
    {
        let (w_title, w_artist, _) = widths(columns);
        let dividers = [34.0 + 44.0 + w_title + 4.0, 34.0 + 44.0 + w_title + 8.0 + w_artist + 4.0];
        for (i, x) in dividers.into_iter().enumerate() {
            let grip = Rect::from_center_size(egui::pos2(hx(x), head.center().y), Vec2::new(12.0, head.height()));
            let resp = ui
                .interact(grip, egui::Id::new(("track-column-divider", i)), Sense::click_and_drag())
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
                .on_hover_text("拖动调整列宽，双击恢复默认");
            if resp.dragged() {
                drag_boundary(columns, i, resp.drag_delta().x, flex);
            }
            if resp.double_clicked() {
                *columns = ncm_core::settings::DEFAULT_TRACK_COLUMNS;
                resized = true;
            }
            resized |= resp.drag_stopped();
            let hot = resp.hovered() || resp.dragged();
            ui.painter().line_segment(
                [egui::pos2(hx(x), head.center().y - 8.0), egui::pos2(hx(x), head.center().y + 8.0)],
                Stroke::new(if hot { 2.0 } else { 1.0 }, if hot { ACCENT } else { p.line }),
            );
        }
    }
    let (w_title, w_artist, w_album) = widths(columns);
    let hy = head.center().y;
    let heading = |x: f32, text: &str| {
        ui.painter().text(egui::pos2(hx(x), hy), Align2::LEFT_CENTER, text, FontId::proportional(12.5), p.muted);
    };
    heading(34.0 + 6.0, "#");
    heading(34.0 + 44.0, "标题");
    heading(34.0 + 44.0 + w_title + 8.0, "歌手");
    heading(34.0 + 44.0 + w_title + w_artist + 16.0, "专辑");
    heading(total_w - 52.0 - 76.0, "时长");

    let visible = view.visible.clone();
    ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, ROW_H, visible.len(), |ui, range| {
        for row in range {
            let orig = visible[row];
            let track = &view.col.tracks[orig];
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(total_w, ROW_H), Sense::click());
            let selected = view.selected[orig];
            let bg = if selected {
                ACCENT.gamma_multiply(if ui.visuals().dark_mode { 0.28 } else { 0.22 })
            } else if resp.hovered() {
                p.card_alt
            } else if row % 2 == 1 {
                p.card_alt.gamma_multiply(0.45)
            } else {
                Color32::TRANSPARENT
            };
            ui.painter().rect_filled(rect, CornerRadius::same(10), bg);
            if resp.clicked() {
                view.selected[orig] = !selected;
            }
            let cy = rect.center().y;
            // Checkbox (drawn by hand; the whole row is the click target).
            let box_rect = Rect::from_center_size(egui::pos2(rect.left() + 17.0, cy), Vec2::splat(18.0));
            if view.selected[orig] {
                ui.painter().rect_filled(box_rect, CornerRadius::same(5), ACCENT_DEEP);
                ui.painter().text(
                    box_rect.center(),
                    Align2::CENTER_CENTER,
                    egui_phosphor::regular::CHECK,
                    FontId::proportional(13.0),
                    Color32::WHITE,
                );
            } else {
                ui.painter().rect_stroke(
                    box_rect,
                    CornerRadius::same(5),
                    Stroke::new(1.5, p.muted.gamma_multiply(0.7)),
                    egui::StrokeKind::Inside,
                );
            }
            let mut x = rect.left() + 34.0;
            ui.painter().text(
                egui::pos2(x + 6.0, cy),
                Align2::LEFT_CENTER,
                format!("{}", orig + 1),
                FontId::proportional(12.5),
                p.muted,
            );
            x += 44.0;

            let avail = availability_color(track, logged_in);
            let title_color = if avail.is_some() { p.muted } else { p.text };
            let title = elide(ui, &track_title(track), FontId::proportional(14.5), title_color, w_title);
            ui.painter().galley(egui::pos2(x, cy - title.size().y / 2.0), title, title_color);
            x += w_title + 8.0;
            let artist = elide(ui, &track.artist_names("/"), FontId::proportional(13.0), p.muted, w_artist);
            ui.painter().galley(egui::pos2(x, cy - artist.size().y / 2.0), artist, p.muted);
            x += w_artist + 8.0;
            let album = elide(ui, &track.album.name, FontId::proportional(13.0), p.muted, w_album);
            ui.painter().galley(egui::pos2(x, cy - album.size().y / 2.0), album, p.muted);
            x = rect.right() - 52.0 - 76.0 + 0.0;
            ui.painter().text(
                egui::pos2(x, cy),
                Align2::LEFT_CENTER,
                format_duration_ms(track.duration_ms),
                FontId::proportional(12.5),
                p.muted,
            );

            // Badge on the right.
            let badge = match track.availability() {
                Availability::VipRequired => Some(("VIP", ACCENT_DEEP, Color32::WHITE)),
                Availability::PurchaseRequired => Some(("付费", theme::WARN, Color32::WHITE)),
                Availability::Unavailable => Some(("无版权", p.line, p.muted)),
                Availability::Ok if track.is_vip_only() => Some(("VIP", ACCENT.gamma_multiply(0.8), Color32::WHITE)),
                Availability::Ok => None,
            };
            if let Some((text, fill, fg)) = badge {
                let galley = ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(11.5), fg);
                let size = galley.size() + Vec2::new(12.0, 4.0);
                let r = Rect::from_center_size(egui::pos2(rect.right() - 8.0 - size.x / 2.0, cy), size);
                ui.painter().rect_filled(r, CornerRadius::same(255), fill);
                ui.painter().galley(r.center() - galley.size() / 2.0, galley, fg);
            }
        }
    });
    resized
}

fn track_title(t: &Track) -> String {
    match t.aliases.first() {
        Some(a) if !a.is_empty() => format!("{}  ({a})", t.name),
        _ => t.name.clone(),
    }
}

/// `Some(colour)` when the track is probably not downloadable with the current login.
fn availability_color(t: &Track, logged_in: bool) -> Option<Color32> {
    match t.availability() {
        Availability::Ok => None,
        Availability::VipRequired if logged_in => None,
        _ => Some(theme::BAD),
    }
}

#[cfg(test)]
mod tests {
    use super::{MIN_COLUMN_PX, drag_boundary, thumbnail_url};

    fn total(c: &[f32; 3]) -> f32 {
        c.iter().sum()
    }

    #[test]
    fn dragging_a_divider_moves_width_between_its_two_columns() {
        let mut c = [0.40, 0.28, 0.32];
        drag_boundary(&mut c, 0, 100.0, 1000.0);
        assert!((c[0] - 0.50).abs() < 1e-6 && (c[1] - 0.18).abs() < 1e-6 && (c[2] - 0.32).abs() < 1e-6, "{c:?}");
        drag_boundary(&mut c, 1, -50.0, 1000.0);
        assert!((c[1] - 0.13).abs() < 1e-6 && (c[2] - 0.37).abs() < 1e-6 && (c[0] - 0.50).abs() < 1e-6, "{c:?}");
        assert!((total(&c) - 1.0).abs() < 1e-6, "the shares keep adding up to one");
    }

    #[test]
    fn a_column_cannot_be_dragged_below_the_minimum() {
        let flex = 800.0;
        let min = MIN_COLUMN_PX / flex;
        let mut c = [0.40, 0.28, 0.32];
        drag_boundary(&mut c, 0, 10_000.0, flex);
        assert!((c[1] - min).abs() < 1e-6, "the right column stops at the minimum: {c:?}");
        drag_boundary(&mut c, 0, -10_000.0, flex);
        assert!((c[0] - min).abs() < 1e-6, "and so does the left one: {c:?}");
        assert!((total(&c) - 1.0).abs() < 1e-6);
        assert!((c[2] - 0.32).abs() < 1e-6, "the column on the other side is untouched");
    }

    #[test]
    fn a_tiny_region_cannot_make_the_drag_panic() {
        let mut c = [0.40, 0.28, 0.32];
        drag_boundary(&mut c, 1, 5.0, 10.0);
        assert!(c.iter().all(|v| v.is_finite() && *v > 0.0));
    }

    #[test]
    fn thumbnails_replace_any_existing_size_parameter() {
        let plain = "https://p1.music.126.net/abc==/123.jpg";
        assert_eq!(thumbnail_url(plain, 120), "https://p1.music.126.net/abc==/123.jpg?param=120y120");
        assert_eq!(thumbnail_url(&format!("{plain}?param=500y500"), 120), "https://p1.music.126.net/abc==/123.jpg?param=120y120");
    }
}
