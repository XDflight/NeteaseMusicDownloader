use std::path::Path;

use eframe::egui::{self, Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, RichText, ScrollArea, Sense, Vec2};
use ncm_core::{FailKind, Stage};

use crate::app::App;
use crate::state::*;
use crate::theme::{self, ACCENT, ACCENT_DEEP, pal};
use crate::ui_browse::elide;
use crate::widgets::{self, danger_button, format_bytes, format_eta, format_speed, primary_button, secondary_button};

const ROW_H: f32 = 64.0;

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

        let mut clear = false;
        let mut retry = false;
        let mut cancel = false;
        let mut open_dir = false;

        theme::card_frame(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width() - 330.0);
                    if total == 0 {
                        ui.label(RichText::new("队列是空的").size(18.0).strong());
                        widgets::hint(ui, "在「解析与选择」里勾选歌曲后点击下载，任务会出现在这里。");
                    } else {
                        ui.label(
                            RichText::new(format!(
                                "共 {total} 首 · 完成 {done} · 跳过 {skipped} · 失败 {failed} · 进行中 {active}"
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
                        let finished = total - active;
                        let partial: f32 = self
                            .queue
                            .items
                            .iter()
                            .filter(|i| !i.state.is_finished() && i.total > 0)
                            .map(|i| i.done as f32 / i.total as f32)
                            .sum();
                        widgets::progress_bar(ui, (finished as f32 + partial) / total as f32, 10.0);
                        let remaining = bytes_total.saturating_sub(bytes_done) as f64;
                        let eta = if self.net.bytes_per_sec > 1024.0 && active > 0 {
                            format_eta(remaining / self.net.bytes_per_sec)
                        } else {
                            "--".into()
                        };
                        widgets::hint(ui, &format!("已下载 {} · 预计剩余 {eta}", format_bytes(bytes_done)));
                    }
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        if active > 0 && danger_button(ui, "取消全部").clicked() {
                            cancel = true;
                        }
                        if failed + cancelled > 0 && primary_button(ui, format!("重试失败项 ({})", failed + cancelled)).clicked()
                        {
                            retry = true;
                        }
                        if total > active && secondary_button(ui, "清除已完成").clicked() {
                            clear = true;
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
                            ui.label(RichText::new(format_speed(self.net.bytes_per_sec)).size(22.0).strong().color(ACCENT_DEEP));
                        });
                        widgets::hint(ui, &format!("连接 {} / 上限 {}（自适应）", self.net.in_flight, self.net.limit.max(1)));
                        widgets::sparkline(ui, &self.net_history, Vec2::new(300.0, 54.0));
                    });
                });
            });
        });
        ui.add_space(6.0);

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
            self.queue_list(ui);
        }

        if cancel {
            self.be.engine.cancel_all();
        }
        if retry {
            self.retry_failed();
        }
        if clear {
            self.queue.clear_finished();
        }
        if open_dir {
            let dir = self.settings.download.output_dir.clone();
            let _ = std::fs::create_dir_all(&dir);
            if let Err(e) = open::that(&dir) {
                self.toasts.push(ToastKind::Error, format!("无法打开文件夹：{e}"));
            }
        }
    }

    fn queue_list(&mut self, ui: &mut egui::Ui) {
        let p = pal(ui);
        let total_w = ui.available_width();
        let n = self.queue.items.len();
        let mut reveal: Option<std::path::PathBuf> = None;
        ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, ROW_H, n, |ui, range| {
            for i in range {
                let it = &self.queue.items[i];
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(total_w, ROW_H - 4.0), Sense::click());
                ui.painter().rect(
                    rect,
                    CornerRadius::same(14),
                    if resp.hovered() { p.card_alt } else { p.card },
                    egui::Stroke::new(1.0, p.line),
                    egui::StrokeKind::Inside,
                );
                let (icon, color) = state_icon(&it.state);
                let cy = rect.center().y;
                ui.painter().text(
                    egui::pos2(rect.left() + 26.0, cy),
                    Align2::CENTER_CENTER,
                    icon,
                    FontId::proportional(22.0),
                    color,
                );

                let text_x = rect.left() + 52.0;
                let right_w = 250.0;
                let title_w = (rect.right() - text_x - right_w - 12.0).max(80.0);
                let title = elide(ui, &it.track.name, FontId::proportional(14.5), p.text, title_w);
                ui.painter().galley(egui::pos2(text_x, rect.top() + 9.0), title, p.text);
                let sub = format!("{} · {}", it.track.artist_names("/"), it.track.album.name);
                let sub = elide(ui, &sub, FontId::proportional(12.0), p.muted, title_w);
                ui.painter().galley(egui::pos2(text_x, rect.top() + 32.0), sub, p.muted);

                // Right side: status text / progress.
                let rx = rect.right() - right_w;
                match &it.state {
                    QState::Working(stage) if matches!(stage, Stage::Downloading) && it.total > 0 => {
                        let frac = it.done as f32 / it.total as f32;
                        let bar = egui::Rect::from_min_size(egui::pos2(rx, cy - 5.0), Vec2::new(right_w - 90.0, 10.0));
                        ui.painter().rect_filled(bar, CornerRadius::same(255), p.line);
                        let fill = egui::Rect::from_min_size(bar.min, Vec2::new((bar.width() * frac).max(10.0), bar.height()));
                        ui.painter().rect_filled(fill, CornerRadius::same(255), ACCENT);
                        ui.painter().text(
                            egui::pos2(bar.right() + 10.0, cy),
                            Align2::LEFT_CENTER,
                            format!("{:.0}%", frac * 100.0),
                            FontId::proportional(13.0),
                            p.text,
                        );
                        if let Some(q) = &it.quality {
                            ui.painter().text(
                                egui::pos2(rx, cy + 20.0),
                                Align2::LEFT_CENTER,
                                format!("{q} · {}", format_bytes(it.total)),
                                FontId::proportional(11.5),
                                p.muted,
                            );
                        }
                    }
                    other => {
                        let (text, c) = status_text(other, it.message.as_deref(), p.muted);
                        let g = elide(ui, &text, FontId::proportional(12.5), c, right_w - 8.0);
                        let h = g.size().y;
                        ui.painter().galley(egui::pos2(rx, cy - h / 2.0 - if it.quality.is_some() { 9.0 } else { 0.0 }), g, c);
                        if let Some(q) = &it.quality {
                            let label = if matches!(other, QState::Done) {
                                format!("{q} · {}", format_bytes(it.total))
                            } else {
                                q.clone()
                            };
                            ui.painter().text(
                                egui::pos2(rx, cy + 12.0),
                                Align2::LEFT_CENTER,
                                label,
                                FontId::proportional(11.5),
                                p.muted,
                            );
                        }
                    }
                }
                if let (Some(path), true) = (&it.path, resp.clicked()) {
                    reveal = Some(path.clone());
                }
                if it.path.is_some() {
                    resp.on_hover_text("点击在文件夹中显示").on_hover_cursor(egui::CursorIcon::PointingHand);
                }
            }
        });
        if let Some(path) = reveal {
            reveal_in_folder(&path);
        }
    }
}

fn state_icon(state: &QState) -> (&'static str, Color32) {
    use egui_phosphor::regular as i;
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

fn status_text(state: &QState, message: Option<&str>, muted: Color32) -> (String, Color32) {
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
