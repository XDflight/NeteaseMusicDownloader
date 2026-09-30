use eframe::egui::{self, RichText, ScrollArea};
use ncm_api::{AlbumRef, Artist, Level, Track};
use ncm_core::lyrics::{LyricsExtra, LyricsFile};
use ncm_core::naming::{self, Layout, NameContext};
use ncm_core::{CoverFile, CoverSize, ExistsPolicy, Theme};

use crate::app::{App, VERSION, apply_theme_preference};
use crate::state::*;
use crate::theme::{self, ACCENT_DEEP};
use crate::widgets::{self, danger_button, primary_button, secondary_button};

impl App {
    pub fn page_settings(&mut self, ui: &mut egui::Ui) {
        self.page_header(ui, "设置", "音质、元数据、命名规则、网络与账号安全");
        ui.add_space(10.0);
        let before = serde_json::to_string(&(
            &self.settings.download,
            &self.settings.theme,
            &self.settings.update.auto_check,
            &self.settings.update.include_prerelease,
            self.settings.confirm_quit_while_downloading,
        ))
        .unwrap_or_default();

        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(ui.available_width());
            self.settings_download(ui);
            ui.add_space(8.0);
            self.settings_metadata(ui);
            ui.add_space(8.0);
            self.settings_network(ui);
            ui.add_space(8.0);
            self.settings_account(ui);
            ui.add_space(8.0);
            self.settings_appearance(ui);
            ui.add_space(8.0);
            self.settings_update(ui);
            ui.add_space(16.0);
        });

        let after = serde_json::to_string(&(
            &self.settings.download,
            &self.settings.theme,
            &self.settings.update.auto_check,
            &self.settings.update.include_prerelease,
            self.settings.confirm_quit_while_downloading,
        ))
        .unwrap_or_default();
        if before != after {
            apply_theme_preference(ui.ctx(), self.settings.theme);
            self.save_settings();
        }
    }

    fn settings_download(&mut self, ui: &mut egui::Ui) {
        theme::card_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            widgets::section_title(ui, egui_phosphor::regular::FOLDER_OPEN, "下载");

            ui.label("保存位置");
            ui.horizontal(|ui| {
                let mut text = self.settings.download.output_dir.display().to_string();
                if ui.add(egui::TextEdit::singleline(&mut text).desired_width(ui.available_width() - 100.0)).changed() {
                    self.settings.download.output_dir = text.into();
                }
                if secondary_button(ui, "浏览…").clicked() {
                    self.be.pick_folder(self.settings.download.output_dir.clone());
                }
            });
            let d = &mut self.settings.download;

            ui.add_space(6.0);
            ui.label("音质");
            for l in Level::ALL {
                ui.radio_value(&mut d.level, l, format!("{}  ·  {}", l.label(), l.hint()));
            }
            ui.checkbox(&mut d.allow_lower_quality, "所选音质不可用时自动降级（否则该歌曲报错）");
            widgets::hint(ui, "Hi-Res / 环绕声 / 母带需要相应会员；账号无权限时服务器会给出较低音质。");

            ui.add_space(6.0);
            ui.label("目录结构");
            ui.horizontal(|ui| {
                ui.radio_value(&mut d.layout, Layout::Collection, "按歌单/专辑建文件夹");
                ui.radio_value(&mut d.layout, Layout::ArtistAlbum, "歌手 / 专辑");
                ui.radio_value(&mut d.layout, Layout::Flat, "不建文件夹");
            });

            ui.add_space(6.0);
            ui.label("文件名模板");
            ui.add(egui::TextEdit::singleline(&mut d.filename_template).desired_width(f32::INFINITY));
            let sample = Track {
                id: 347230,
                name: "海阔天空".into(),
                artists: vec![Artist { id: 1, name: "Beyond".into() }],
                album: AlbumRef { id: 2, name: "乐与怒".into(), pic_url: None },
                track_no: 3,
                disc: "1".into(),
                publish_time_ms: Some(662_688_000_000),
                ..Track::default()
            };
            let ctx = NameContext { track: &sample, index: Some(7), collection: "我的歌单", quality: d.level.as_str() };
            let preview = naming::render_template(&d.filename_template, &ctx);
            widgets::hint(ui, &format!("预览：{preview}.mp3"));
            widgets::hint(ui, "可用：{artist} {title} {album} {track:02} {index:03} {year} {id} {quality} {playlist}");

            ui.add_space(6.0);
            ui.label("文件已存在时");
            ui.horizontal(|ui| {
                ui.radio_value(&mut d.on_exists, ExistsPolicy::Skip, "跳过");
                ui.radio_value(&mut d.on_exists, ExistsPolicy::Overwrite, "覆盖");
                ui.radio_value(&mut d.on_exists, ExistsPolicy::KeepBoth, "保留两份（自动改名）");
            });
        });
    }

    fn settings_metadata(&mut self, ui: &mut egui::Ui) {
        theme::card_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            widgets::section_title(ui, egui_phosphor::regular::TAG, "元数据、封面与歌词");
            let d = &mut self.settings.download;
            ui.checkbox(&mut d.embed_tags, "写入标签（标题、歌手、专辑、音轨号、年份）");
            ui.horizontal(|ui| {
                ui.checkbox(&mut d.embed_cover, "内嵌封面");
                egui::ComboBox::from_id_salt("cover-size").selected_text(format!("尺寸：{}", d.cover_size.label())).show_ui(
                    ui,
                    |ui| {
                        for s in [CoverSize::Px500, CoverSize::Px800, CoverSize::Px1400, CoverSize::Original] {
                            ui.selectable_value(&mut d.cover_size, s, s.label());
                        }
                    },
                );
            });
            ui.horizontal(|ui| {
                ui.label("同时保存封面图片：");
                egui::ComboBox::from_id_salt("cover-file")
                    .selected_text(match d.cover_file {
                        CoverFile::Off => "不保存",
                        CoverFile::PerTrack => "每首歌一张（歌名.jpg）",
                        CoverFile::Folder => "文件夹封面（cover.jpg）",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut d.cover_file, CoverFile::Off, "不保存");
                        ui.selectable_value(&mut d.cover_file, CoverFile::PerTrack, "每首歌一张（歌名.jpg）");
                        ui.selectable_value(&mut d.cover_file, CoverFile::Folder, "文件夹封面（cover.jpg）");
                    });
            });
            ui.add_space(4.0);
            ui.checkbox(&mut d.embed_lyrics, "内嵌歌词");
            ui.add_enabled_ui(d.embed_lyrics, |ui| {
                ui.checkbox(&mut d.embed_timeline, "内嵌歌词保留时间轴（LRC 格式；关闭则内嵌纯文本）");
            });
            ui.horizontal(|ui| {
                ui.label("外挂歌词文件：");
                egui::ComboBox::from_id_salt("lyrics-file").selected_text(d.lyrics_file.label()).show_ui(ui, |ui| {
                    for mode in LyricsFile::ALL {
                        ui.selectable_value(&mut d.lyrics_file, mode, mode.label());
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.label("歌词附加：");
                ui.radio_value(&mut d.lyrics_extra, LyricsExtra::None, "仅原文");
                ui.radio_value(&mut d.lyrics_extra, LyricsExtra::Translation, "原文 + 翻译");
                ui.radio_value(&mut d.lyrics_extra, LyricsExtra::Romaji, "原文 + 罗马音");
            });
            ui.checkbox(&mut d.source_comment, "在备注标签中写入歌曲网址");
        });
    }

    fn settings_network(&mut self, ui: &mut egui::Ui) {
        theme::card_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            widgets::section_title(ui, egui_phosphor::regular::GLOBE, "网络");
            ui.label("代理（留空表示直连）");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.settings.proxy)
                        .hint_text("http://127.0.0.1:7890  或  socks5://127.0.0.1:1080")
                        .desired_width(ui.available_width() - 100.0),
                );
                if secondary_button(ui, "应用").clicked() {
                    self.save_settings();
                    self.rebuild_backend();
                }
            });
            widgets::hint(ui, "并发连接数由程序根据网络状况自动调节（1～8 条），无需手动设置。");
        });
    }

    fn settings_account(&mut self, ui: &mut egui::Ui) {
        theme::card_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            widgets::section_title(ui, egui_phosphor::regular::SHIELD_CHECK, "账号与安全");
            match &self.account {
                Some(a) => {
                    ui.label(format!("当前账号：{}（{}）", a.nickname, account_label(a)));
                }
                None => {
                    ui.label("未登录");
                }
            }
            if let Some(store) = &self.store {
                widgets::hint(ui, &format!("登录信息以 AES-256-GCM 加密保存在：{}", store.path().display()));
            }
            widgets::hint(ui, "密钥由本机标识派生（换电脑无法解密）；不使用系统钥匙串。可另设口令，进一步保护。");
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if self.passphrase.is_some() {
                    ui.label(RichText::new("口令保护：已启用").color(theme::GOOD));
                    if secondary_button(ui, "移除口令").clicked() {
                        self.passphrase = None;
                        self.persist_session();
                        self.toasts.push(ToastKind::Info, "已移除口令，仅使用本机绑定加密");
                    }
                } else {
                    ui.label("口令保护：未启用");
                    if secondary_button(ui, "设置口令…").clicked() {
                        self.pass_ui.set_open = true;
                    }
                }
                if self.account.is_some() && danger_button(ui, "退出登录并清除").clicked() {
                    self.sign_out();
                }
            });
        });
    }

    fn settings_appearance(&mut self, ui: &mut egui::Ui) {
        theme::card_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            widgets::section_title(ui, egui_phosphor::regular::PALETTE, "外观");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.settings.theme, Theme::System, "跟随系统");
                ui.radio_value(&mut self.settings.theme, Theme::Light, "浅色");
                ui.radio_value(&mut self.settings.theme, Theme::Dark, "深色");
            });
            ui.checkbox(&mut self.settings.confirm_quit_while_downloading, "下载未完成时退出前询问");
        });
    }

    fn settings_update(&mut self, ui: &mut egui::Ui) {
        theme::card_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            widgets::section_title(ui, egui_phosphor::regular::ARROWS_CLOCKWISE, "更新");
            ui.label(format!("当前版本 v{VERSION}"));
            ui.checkbox(&mut self.settings.update.auto_check, "启动时自动检查更新（GitHub Releases）");
            ui.checkbox(&mut self.settings.update.include_prerelease, "包含预发布版本");
            ui.horizontal(|ui| {
                if primary_button(ui, "检查更新").clicked() {
                    self.update = UpdateState::Checking;
                    self.be.check_update(true, VERSION.to_owned(), self.settings.update.include_prerelease);
                }
                match &self.update {
                    UpdateState::Checking => {
                        ui.spinner();
                        ui.label("正在检查…");
                    }
                    UpdateState::UpToDate => {
                        ui.label(RichText::new("已是最新版本").color(theme::GOOD));
                    }
                    UpdateState::Available(i) => {
                        ui.label(RichText::new(format!("发现新版本 v{}", i.release.version)).color(ACCENT_DEEP));
                    }
                    UpdateState::Failed(e) => {
                        ui.label(RichText::new(format!("检查失败：{e}")).color(theme::BAD));
                    }
                    _ => {}
                }
            });
        });
    }
}
