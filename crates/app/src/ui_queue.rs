use std::path::{Path, PathBuf};

use eframe::egui::{self, Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, RichText, ScrollArea, Sense, Vec2};
use ncm_core::{FailKind, Stage};

use crate::app::App;
use crate::state::*;
use crate::theme::{self, ACCENT, ACCENT_DEEP, pal};
use crate::ui_browse::elide;
use crate::widgets::{self, danger_button, format_bytes, format_eta, format_speed, primary_button, secondary_button};

const ROW_H: f32 = 64.0;
/// Title line of a list, with the room below it.
const SECTION_HEAD_H: f32 = 40.0;
/// A list without items only shows its title and a hint.
const EMPTY_SECTION_H: f32 = SECTION_HEAD_H + 28.0;
const SECTION_GAP: f32 = 8.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    /// Running, waiting and paused downloads.
    Active,
    /// Everything that has ended: done, skipped, failed or cancelled.
    Finished,
}

#[derive(Default)]
struct ListActions {
    reveal: Option<PathBuf>,
    clear: bool,
}

impl App {
    pub fn page_queue(&mut self, ui: &mut egui::Ui) {
        self.page_header(ui, "下载队列", "自适应并发：网络好时自动加速，拥堵时自动退让");
        ui.add_space(10.0);

        let total = self.queue.items.len();
        let done = self.queue.count(|s| matches!(s, QState::Done));
        let skipped = self.queue.count(|s| matches!(s, QState::Skipped));
        let failed = self.queue.count(|s| matches!(s, QState::Failed(_)));
        let cancelled = self.queue.count(|s| matches!(s, QState::Cancelled));
        let active = self.queue.active();
        let restored = self.queue.restored();
        let paused = self.queue.paused;
        let waiting = active + restored;

        let mut cancel = false;
        let mut retry = false;
        let mut toggle_pause = false;
        let mut open_dir = false;

        theme::card_frame(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width() - 330.0);
                    if total == 0 {
                        ui.label(RichText::new("队列是空的").size(18.0).strong());
                        widgets::hint(ui, "在「解析与选择」里勾选歌曲后点击下载，任务会出现在这里。");
                    } else {
                        let state = if paused && active > 0 { " · 已暂停" } else { "" };
                        ui.label(
                            RichText::new(format!(
                                "共 {total} 首 · 完成 {done} · 跳过 {skipped} · 失败 {failed} · 未完成 {waiting}{state}"
                            ))
                            .size(16.0)
                            .strong(),
                        );
                        let (bytes_done, bytes_total) = self
                            .queue
                            .items
                            .iter()
                            .filter(|i| i.total > 0)
                            .fold((0u64, 0u64), |a, i| (a.0 + i.done.min(i.total), a.1 + i.total));
                        let finished = total - waiting;
                        let partial: f32 = self
                            .queue
                            .items
                            .iter()
                            .filter(|i| !i.state.is_finished() && i.total > 0)
                            .map(|i| i.done as f32 / i.total as f32)
                            .sum();
                        widgets::progress_bar(ui, (finished as f32 + partial) / total as f32, 10.0);
                        let remaining = bytes_total.saturating_sub(bytes_done) as f64;
                        let eta = if self.net.bytes_per_sec > 1024.0 && active > 0 && !paused {
                            format_eta(remaining / self.net.bytes_per_sec)
                        } else {
                            "--".into()
                        };
                        widgets::hint(ui, &format!("已下载 {} · 预计剩余 {eta}", format_bytes(bytes_done)));
                    }
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        if active > 0 || paused {
                            let clicked = if paused {
                                primary_button(ui, format!("{} 继续下载", egui_phosphor::regular::PLAY)).clicked()
                            } else {
                                secondary_button(ui, format!("{} 暂停全部", egui_phosphor::regular::PAUSE)).clicked()
                            };
                            toggle_pause |= clicked;
                        }
                        if active > 0 && danger_button(ui, "取消全部").clicked() {
                            cancel = true;
                        }
                        if failed + cancelled > 0 && primary_button(ui, format!("重试失败项 ({})", failed + cancelled)).clicked()
                        {
                            retry = true;
                        }
                        if secondary_button(ui, format!("{} 打开下载文件夹", egui_phosphor::regular::FOLDER_OPEN)).clicked()
                        {
                            open_dir = true;
                        }
                    });
                });
                ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            let speed = if paused { "已暂停".to_owned() } else { format_speed(self.net.bytes_per_sec) };
                            ui.label(RichText::new(speed).size(22.0).strong().color(ACCENT_DEEP));
                        });
                        widgets::hint(ui, &format!("连接 {} / 上限 {}（自适应）", self.net.in_flight, self.net.limit.max(1)));
                        widgets::sparkline(ui, &self.net_history, Vec2::new(300.0, 54.0));
                    });
                });
            });
        });

        let mut resume_restored = false;
        let mut discard_restored = false;
        if restored > 0 {
            ui.add_space(6.0);
            theme::card_frame(ui).fill(theme::ACCENT_SOFT.gamma_multiply(if ui.visuals().dark_mode { 0.25 } else { 1.0 })).show(
                ui,
                |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(egui_phosphor::regular::CLOCK_COUNTER_CLOCKWISE).size(24.0).color(ACCENT_DEEP));
                        ui.vertical(|ui| {
                            ui.label(RichText::new(format!("上次有 {restored} 首歌曲没有下载完成")).strong());
                            widgets::hint(ui, "已恢复到下面的列表。已经下载的部分会保留，继续时只补下载缺少的部分。");
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if secondary_button(ui, "丢弃").clicked() {
                                discard_restored = true;
                            }
                            if primary_button(ui, format!("{} 继续下载", egui_phosphor::regular::PLAY)).clicked() {
                                resume_restored = true;
                            }
                        });
                    });
                },
            );
        }
        ui.add_space(6.0);

        let mut actions = ListActions::default();
        if total == 0 {
            ui.add_space(16.0);
            let tex = self.assets.search.clone();
            let h = 300.0f32.min(ui.available_height() - 10.0).max(120.0);
            let stage_w = 300.0f32.min(ui.available_width());
            let (stage, _) = ui.allocate_exact_size(Vec2::new(stage_w, h), Sense::hover());
            let stage = Rect::from_center_size(egui::pos2(ui.max_rect().center().x, stage.center().y), stage.size());
            let time = self.started.elapsed().as_secs_f64();
            theme::mascot_stage(ui, stage, time);
            let ih = h - 20.0;
            let iw = ih * tex.size()[0] as f32 / tex.size()[1] as f32;
            let sprite = Rect::from_center_size(stage.center() + Vec2::new(0.0, 4.0), Vec2::new(iw, ih));
            ui.painter().image(tex.id(), sprite, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        } else {
            actions = self.queue_lists(ui);
        }

        if toggle_pause {
            self.toggle_pause();
        }
        if cancel {
            self.cancel_downloads();
        }
        if retry {
            self.retry_failed();
        }
        if resume_restored {
            self.resume_restored();
        }
        if discard_restored {
            self.discard_restored();
        }
        if actions.clear {
            self.queue.clear_finished();
        }
        if open_dir {
            self.open_download_folder();
        }
        if let Some(path) = actions.reveal {
            reveal_in_folder(&path);
        }
    }

    /// The two lists: what is still to do, and what has ended.
    fn queue_lists(&mut self, ui: &mut egui::Ui) -> ListActions {
        let (pending, finished) = self.queue.lists();
        let (pending_h, finished_h) = list_heights(ui.available_height(), pending.len(), finished.len());
        let mut actions = ListActions::default();
        self.queue_section(ui, "进行中与待下载", &pending, pending_h, Section::Active, &mut actions);
        ui.add_space(SECTION_GAP);
        self.queue_section(ui, "已完成", &finished, finished_h, Section::Finished, &mut actions);
        actions
    }

    fn queue_section(
        &mut self,
        ui: &mut egui::Ui,
        title: &str,
        items: &[usize],
        height: f32,
        section: Section,
        out: &mut ListActions,
    ) {
        let p = pal(ui);
        let paused = self.queue.paused;
        ui.horizontal(|ui| {
            ui.set_height(SECTION_HEAD_H - 6.0);
            ui.label(RichText::new(title).size(16.0).strong());
            widgets::pill(ui, &items.len().to_string(), p.card_alt, p.muted);
            if section == Section::Finished && !items.is_empty() {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if secondary_button(ui, format!("{} 清除已完成", egui_phosphor::regular::TRASH)).clicked() {
                        out.clear = true;
                    }
                });
            }
        });
        if items.is_empty() {
            widgets::hint(
                ui,
                match section {
                    Section::Active => "没有正在下载或等待的任务。",
                    Section::Finished => "还没有完成的任务。",
                },
            );
            return;
        }
        let rows_h = (height - SECTION_HEAD_H).max(ROW_H);
        let width = ui.available_width();
        ScrollArea::vertical().id_salt(("queue-list", section as u8)).max_height(rows_h).auto_shrink([false, false]).show_rows(
            ui,
            ROW_H,
            items.len(),
            |ui, range| {
                for k in range {
                    let item = &self.queue.items[items[k]];
                    if let Some(path) = draw_row(ui, item, width, &p, paused) {
                        out.reveal = Some(path);
                    }
                }
            },
        );
    }
}

/// Heights of the two lists, including their title lines. A short list takes only what it
/// needs; the other one gets the rest.
fn list_heights(available: f32, pending: usize, finished: usize) -> (f32, f32) {
    let usable = (available - SECTION_GAP).max(0.0);
    let need = |n: usize| if n == 0 { EMPTY_SECTION_H } else { SECTION_HEAD_H + n as f32 * ROW_H };
    let (need_p, need_f) = (need(pending), need(finished));
    let half = usable / 2.0;
    if need_p <= half || need_p + need_f <= usable {
        let p = need_p.min(usable);
        (p, usable - p)
    } else if need_f <= half {
        (usable - need_f, need_f)
    } else {
        (half, usable - half)
    }
}

/// One row. Returns the path to show in the file manager when the row (or its button) was clicked.
fn draw_row(ui: &mut egui::Ui, it: &QItem, width: f32, p: &theme::Palette, paused: bool) -> Option<PathBuf> {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, ROW_H - 4.0), Sense::click());
    ui.painter().rect(
        rect,
        CornerRadius::same(14),
        if resp.hovered() { p.card_alt } else { p.card },
        egui::Stroke::new(1.0, p.line),
        egui::StrokeKind::Inside,
    );
    let (icon, color) = state_icon(&it.state, paused);
    let cy = rect.center().y;
    ui.painter().text(egui::pos2(rect.left() + 26.0, cy), Align2::CENTER_CENTER, icon, FontId::proportional(22.0), color);

    // Room for the "show in folder" button on the right.
    let button_w = if it.path.is_some() { 44.0 } else { 0.0 };
    let text_x = rect.left() + 52.0;
    let right_w = 250.0;
    let title_w = (rect.right() - button_w - text_x - right_w - 12.0).max(80.0);
    let title = elide(ui, &it.track.name, FontId::proportional(14.5), p.text, title_w);
    ui.painter().galley(egui::pos2(text_x, rect.top() + 9.0), title, p.text);
    let sub = format!("{} · {}", it.track.artist_names("/"), it.track.album.name);
    let sub = elide(ui, &sub, FontId::proportional(12.0), p.muted, title_w);
    ui.painter().galley(egui::pos2(text_x, rect.top() + 32.0), sub, p.muted);

    // Right side: status text / progress.
    let rx = rect.right() - button_w - right_w;
    match &it.state {
        QState::Working(Stage::Downloading) if it.total > 0 => {
            let frac = it.done as f32 / it.total as f32;
            let bar = Rect::from_min_size(egui::pos2(rx, cy - 5.0), Vec2::new(right_w - 90.0, 10.0));
            ui.painter().rect_filled(bar, CornerRadius::same(255), p.line);
            let fill = Rect::from_min_size(bar.min, Vec2::new((bar.width() * frac).max(10.0), bar.height()));
            ui.painter().rect_filled(fill, CornerRadius::same(255), if paused { p.muted } else { ACCENT });
            ui.painter().text(
                egui::pos2(bar.right() + 10.0, cy),
                Align2::LEFT_CENTER,
                format!("{:.0}%", frac * 100.0),
                FontId::proportional(13.0),
                p.text,
            );
            let detail = match (&it.quality, paused) {
                (Some(q), true) => Some(format!("已暂停 · {q} · {}", format_bytes(it.total))),
                (Some(q), false) => Some(format!("{q} · {}", format_bytes(it.total))),
                (None, true) => Some("已暂停".to_owned()),
                (None, false) => None,
            };
            if let Some(d) = detail {
                ui.painter().text(egui::pos2(rx, cy + 20.0), Align2::LEFT_CENTER, d, FontId::proportional(11.5), p.muted);
            }
        }
        other => {
            let (text, c) = status_text(other, it.message.as_deref(), p.muted, paused, it.is_restored());
            let g = elide(ui, &text, FontId::proportional(12.5), c, right_w - 8.0);
            let h = g.size().y;
            ui.painter().galley(egui::pos2(rx, cy - h / 2.0 - if it.quality.is_some() { 9.0 } else { 0.0 }), g, c);
            if let Some(q) = &it.quality {
                let label = if matches!(other, QState::Done) { format!("{q} · {}", format_bytes(it.total)) } else { q.clone() };
                ui.painter().text(egui::pos2(rx, cy + 12.0), Align2::LEFT_CENTER, label, FontId::proportional(11.5), p.muted);
            }
        }
    }

    let mut reveal = None;
    if let Some(path) = &it.path {
        let button = Rect::from_center_size(egui::pos2(rect.right() - 26.0, cy), Vec2::splat(32.0));
        let hit = ui.interact(button, resp.id.with("reveal"), Sense::click());
        if hit.hovered() {
            ui.painter().rect_filled(button, CornerRadius::same(10), p.line);
        }
        ui.painter().text(
            button.center(),
            Align2::CENTER_CENTER,
            egui_phosphor::regular::FOLDER_OPEN,
            FontId::proportional(18.0),
            if hit.hovered() { ACCENT_DEEP } else { p.muted },
        );
        if hit.on_hover_text("在文件夹中显示").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() || resp.clicked()
        {
            reveal = Some(path.clone());
        }
        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    reveal
}

/// While paused, what would be moving shows as paused.
fn is_held(state: &QState, paused: bool) -> bool {
    paused && matches!(state, QState::Queued | QState::Working(Stage::Resolving | Stage::Downloading))
}

fn state_icon(state: &QState, paused: bool) -> (&'static str, Color32) {
    use egui_phosphor::regular as i;
    if is_held(state, paused) {
        return (i::PAUSE_CIRCLE, Color32::GRAY);
    }
    match state {
        QState::Queued => (i::CLOCK, Color32::GRAY),
        QState::Working(Stage::Resolving) => (i::MAGNIFYING_GLASS, theme::SECONDARY_DEEP),
        QState::Working(Stage::Downloading) => (i::DOWNLOAD_SIMPLE, ACCENT_DEEP),
        QState::Working(Stage::Verifying) => (i::SEAL_CHECK, theme::SECONDARY_DEEP),
        QState::Working(Stage::Tagging) => (i::TAG, theme::SECONDARY_DEEP),
        QState::Working(Stage::Finishing) => (i::FLOPPY_DISK, theme::SECONDARY_DEEP),
        QState::Done => (i::CHECK_CIRCLE, theme::GOOD),
        QState::Skipped => (i::SKIP_FORWARD_CIRCLE, Color32::GRAY),
        QState::Failed(FailKind::VipRequired) => (i::CROWN_SIMPLE, theme::WARN),
        QState::Failed(_) => (i::X_CIRCLE, theme::BAD),
        QState::Cancelled => (i::PROHIBIT, Color32::GRAY),
    }
}

fn status_text(state: &QState, message: Option<&str>, muted: Color32, paused: bool, restored: bool) -> (String, Color32) {
    if restored {
        // Brought back from the previous run; nothing happens until the user continues.
        return ("等待继续".into(), muted);
    }
    if is_held(state, paused) {
        return ("已暂停".into(), muted);
    }
    match state {
        QState::Queued => ("等待中".into(), muted),
        QState::Working(Stage::Resolving) => ("正在获取地址…".into(), muted),
        QState::Working(Stage::Downloading) => ("正在下载…".into(), muted),
        QState::Working(Stage::Verifying) => ("正在校验…".into(), muted),
        QState::Working(Stage::Tagging) => ("正在写入标签与歌词…".into(), muted),
        QState::Working(Stage::Finishing) => ("正在保存…".into(), muted),
        QState::Done => match message {
            Some(m) => (format!("完成（{m}）"), theme::WARN),
            None => ("完成".into(), theme::GOOD),
        },
        QState::Skipped => (message.unwrap_or("已跳过").to_owned(), muted),
        QState::Failed(FailKind::VipRequired) => (message.unwrap_or("需要 VIP").to_owned(), theme::WARN),
        QState::Failed(_) => (message.unwrap_or("失败").to_owned(), theme::BAD),
        QState::Cancelled => ("已取消".into(), muted),
    }
}

pub fn reveal_in_folder(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        let _ = open::that(path.parent().unwrap_or(path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_list_takes_only_what_it_needs() {
        let (p, f) = list_heights(800.0, 2, 30);
        assert_eq!(p, SECTION_HEAD_H + 2.0 * ROW_H);
        assert!((p + f - (800.0 - SECTION_GAP)).abs() < 0.01, "the other list gets the rest: {p} + {f}");
    }

    #[test]
    fn two_long_lists_share_the_space_equally() {
        let (p, f) = list_heights(800.0, 40, 40);
        assert!((p - f).abs() < 0.01);
        assert!((p + f - (800.0 - SECTION_GAP)).abs() < 0.01);
    }

    #[test]
    fn an_empty_list_only_shows_its_title() {
        let (p, f) = list_heights(800.0, 0, 50);
        assert_eq!(p, EMPTY_SECTION_H);
        assert!((p + f - (800.0 - SECTION_GAP)).abs() < 0.01);
        let (p, f) = list_heights(800.0, 50, 0);
        assert_eq!(f, EMPTY_SECTION_H);
        assert!((p + f - (800.0 - SECTION_GAP)).abs() < 0.01);
    }

    #[test]
    fn paused_holds_back_only_what_would_be_moving() {
        assert!(is_held(&QState::Queued, true));
        assert!(is_held(&QState::Working(Stage::Downloading), true));
        assert!(!is_held(&QState::Working(Stage::Tagging), true), "local work still finishes");
        assert!(!is_held(&QState::Done, true));
        assert!(!is_held(&QState::Queued, false));
    }
}
