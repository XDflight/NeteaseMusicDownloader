use eframe::egui::{self, RichText, ScrollArea, Vec2};

use crate::app::{App, REPO_URL, VERSION};
use crate::theme::{self, ACCENT_DEEP};
use crate::widgets;

impl App {
    pub fn page_about(&mut self, ui: &mut egui::Ui) {
        self.page_header(ui, "关于", "版本、项目主页与使用须知");
        ui.add_space(10.0);
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            theme::card_frame(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let tex = &self.assets.happy;
                    let h = 210.0;
                    let w = h * tex.size()[0] as f32 / tex.size()[1] as f32;
                    ui.add(egui::Image::new(tex).fit_to_exact_size(Vec2::new(w, h)));
                    ui.vertical(|ui| {
                        ui.add_space(8.0);
                        ui.label(RichText::new("云音下载姬").size(28.0).strong().color(ACCENT_DEEP));
                        ui.label("网易云音乐批量下载器 · Netease Music Downloader");
                        widgets::hint(ui, &format!("版本 v{VERSION}"));
                        ui.add_space(8.0);
                        ui.label("• 公开与私有歌单、专辑、单曲的批量下载");
                        ui.label("• 自选音质，自动写入标签、封面与歌词（含时间轴 LRC）");
                        ui.label("• 按网络状况自适应并发，登录信息本地加密保存");
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.hyperlink_to(format!("{} 项目主页", egui_phosphor::regular::GITHUB_LOGO), REPO_URL);
                            ui.hyperlink_to(
                                format!("{} 版本发布", egui_phosphor::regular::PACKAGE),
                                format!("{REPO_URL}/releases"),
                            );
                            ui.hyperlink_to(format!("{} 反馈问题", egui_phosphor::regular::BUG), format!("{REPO_URL}/issues"));
                        });
                    });
                });
            });
            ui.add_space(8.0);
            theme::card_frame(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                widgets::section_title(ui, egui_phosphor::regular::SCALES, "使用须知");
                ui.label("本工具仅供个人学习，以及备份你本人有权访问的音乐。请遵守网易云音乐的服务条款和所在地的版权法律。");
                ui.label("VIP 歌曲需要使用你自己已开通会员的账号；本工具不会绕过付费或版权限制。");
                ui.label("本项目与网易公司无任何关联，「网易云音乐」是其商标。请勿将下载内容用于传播或商业用途。");
                widgets::hint(ui, "开源协议：AGPL-3.0-only");
            });
        });
    }
}
