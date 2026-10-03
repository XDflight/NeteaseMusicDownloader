use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui::{self, Align2, Color32, CornerRadius, RichText, Sense, Vec2};
use ncm_core::StoreError;
use zeroize::Zeroizing;

use crate::app::App;
use crate::state::*;
use crate::theme::{self, ACCENT_DEEP};
use crate::widgets::{self, primary_button, secondary_button};

impl App {
    pub fn open_login(&mut self) {
        self.login.open = true;
        self.login.error = None;
        self.login.busy = false;
        if self.login.tab == LoginTab::Qr {
            self.start_qr();
        }
    }

    fn close_login(&mut self) {
        self.login.open = false;
        self.login.qr.cancel.store(true, Ordering::Relaxed);
    }

    fn start_qr(&mut self) {
        self.login.qr.cancel.store(true, Ordering::Relaxed);
        self.login.qr.cancel = Arc::new(AtomicBool::new(false));
        self.login.qr.generation += 1;
        self.login.qr.phase = QrPhase::Starting;
        self.login.qr.modules.clear();
        self.be.start_qr_login(self.login.qr.generation, self.login.qr.cancel.clone());
    }

    pub fn login_window(&mut self, ctx: &egui::Context) {
        if !self.login.open {
            return;
        }
        let mut open = true;
        let mut restart_qr = false;
        egui::Window::new("登录网易云音乐")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(Vec2::new(460.0, 470.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (tab, label) in
                        [(LoginTab::Qr, "扫码登录（推荐）"), (LoginTab::Phone, "手机号"), (LoginTab::Cookie, "Cookie")]
                    {
                        if ui.selectable_label(self.login.tab == tab, RichText::new(label).size(15.0)).clicked()
                            && self.login.tab != tab
                        {
                            self.login.tab = tab;
                            self.login.error = None;
                            if tab == LoginTab::Qr {
                                restart_qr = true;
                            } else {
                                self.login.qr.cancel.store(true, Ordering::Relaxed);
                            }
                        }
                    }
                });
                ui.separator();
                match self.login.tab {
                    LoginTab::Qr => self.login_qr_tab(ui, &mut restart_qr),
                    LoginTab::Phone => self.login_phone_tab(ui),
                    LoginTab::Cookie => self.login_cookie_tab(ui),
                }
                if let Some(e) = &self.login.error {
                    ui.add_space(6.0);
                    ui.label(RichText::new(e).color(theme::BAD));
                }
            });
        if restart_qr {
            self.start_qr();
        }
        if !open {
            self.close_login();
        }
    }

    fn login_qr_tab(&mut self, ui: &mut egui::Ui, restart: &mut bool) {
        ui.vertical_centered(|ui| {
            ui.add_space(6.0);
            ui.label("用网易云音乐手机 App 扫描二维码，并在手机上确认登录");
            ui.add_space(8.0);
            let side = 230.0;
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
            ui.painter().rect_filled(rect, CornerRadius::same(16), Color32::WHITE);
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(16),
                egui::Stroke::new(2.0, theme::ACCENT),
                egui::StrokeKind::Inside,
            );
            let qr = &self.login.qr;
            if qr.size > 0 && !qr.modules.is_empty() {
                let quiet = 2.0;
                let cell = (side - 24.0) / (qr.size as f32 + quiet * 2.0);
                let origin = rect.min + Vec2::splat(12.0 + quiet * cell);
                for y in 0..qr.size {
                    for x in 0..qr.size {
                        if qr.modules[y * qr.size + x] {
                            let r = egui::Rect::from_min_size(
                                origin + Vec2::new(x as f32 * cell, y as f32 * cell),
                                Vec2::splat(cell + 0.6),
                            );
                            ui.painter().rect_filled(r, 0.0, Color32::from_rgb(0x1E, 0x16, 0x1A));
                        }
                    }
                }
                let dim = matches!(qr.phase, QrPhase::Expired | QrPhase::Failed(_) | QrPhase::Scanned(_));
                if dim {
                    ui.painter().rect_filled(rect, CornerRadius::same(16), Color32::from_white_alpha(215));
                }
            } else {
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    "生成中…",
                    egui::FontId::proportional(16.0),
                    Color32::GRAY,
                );
            }
            match &self.login.qr.phase {
                QrPhase::Scanned(name) => {
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        egui_phosphor::regular::CHECK_CIRCLE,
                        egui::FontId::proportional(54.0),
                        theme::GOOD,
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(if name.is_empty() {
                            "已扫描，请在手机上确认".to_owned()
                        } else {
                            format!("{name}，请在手机上点击确认登录")
                        })
                        .strong()
                        .color(ACCENT_DEEP),
                    );
                }
                QrPhase::Expired => {
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        "二维码已过期",
                        egui::FontId::proportional(18.0),
                        Color32::DARK_GRAY,
                    );
                    ui.add_space(8.0);
                    if primary_button(ui, "刷新二维码").clicked() {
                        *restart = true;
                    }
                }
                QrPhase::Failed(e) => {
                    ui.add_space(8.0);
                    ui.label(RichText::new(e).color(theme::BAD));
                    if primary_button(ui, "重试").clicked() {
                        *restart = true;
                    }
                }
                QrPhase::Starting | QrPhase::Waiting => {
                    ui.add_space(8.0);
                    widgets::hint(ui, "扫码后无需输入密码；登录信息仅加密保存在本机");
                }
            }
        });
    }

    fn login_phone_tab(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        widgets::hint(ui, "网易云对手机号登录有风控（可能要求图形验证）。失败时请改用扫码登录。");
        ui.add_space(6.0);
        egui::Grid::new("phone-grid").num_columns(2).spacing([10.0, 10.0]).show(ui, |ui| {
            ui.label("手机号");
            ui.horizontal(|ui| {
                ui.label("+");
                ui.add(egui::TextEdit::singleline(&mut self.login.country).desired_width(40.0));
                ui.add(egui::TextEdit::singleline(&mut self.login.phone).hint_text("手机号码").desired_width(200.0));
            });
            ui.end_row();
            ui.label(if self.login.use_captcha { "验证码" } else { "密码" });
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.login.secret)
                        .password(!self.login.use_captcha)
                        .desired_width(if self.login.use_captcha { 120.0 } else { 260.0 }),
                );
                if self.login.use_captcha {
                    ui.add_enabled_ui(!self.login.sending && !self.login.phone.trim().is_empty(), |ui| {
                        if secondary_button(ui, "发送验证码").clicked() {
                            self.login.sending = true;
                            self.login.error = None;
                            self.login.qr.generation += 1;
                            self.be.send_sms(
                                self.login.qr.generation,
                                self.login.phone.trim().to_owned(),
                                self.login.country.trim().to_owned(),
                            );
                        }
                    });
                }
            });
            ui.end_row();
        });
        ui.checkbox(&mut self.login.use_captcha, "使用短信验证码登录");
        ui.add_space(8.0);
        ui.add_enabled_ui(!self.login.busy && !self.login.phone.trim().is_empty() && !self.login.secret.is_empty(), |ui| {
            if primary_button(ui, if self.login.busy { "登录中…" } else { "登录" }).clicked() {
                self.login.busy = true;
                self.login.error = None;
                self.login.qr.generation += 1;
                self.be.login_phone(
                    self.login.qr.generation,
                    self.login.phone.trim().to_owned(),
                    self.login.country.trim().to_owned(),
                    self.login.secret.clone(),
                    self.login.use_captcha,
                );
            }
        });
    }

    fn login_cookie_tab(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.label("已经在别处登录过？把 Cookie 里 MUSIC_U 的值（或整段 Cookie）粘贴到这里：");
        widgets::hint(ui, "浏览器：F12 → 应用程序/存储 → Cookie → music.163.com → MUSIC_U");
        ui.add_space(4.0);
        ui.add(
            egui::TextEdit::multiline(&mut self.login.cookie)
                .password(false)
                .desired_rows(5)
                .desired_width(f32::INFINITY)
                .hint_text("MUSIC_U=…"),
        );
        ui.add_space(8.0);
        ui.add_enabled_ui(!self.login.busy && !self.login.cookie.trim().is_empty(), |ui| {
            if primary_button(ui, if self.login.busy { "验证中…" } else { "使用此 Cookie 登录" }).clicked() {
                self.login.busy = true;
                self.login.error = None;
                self.login.qr.generation += 1;
                self.be.login_cookie(self.login.qr.generation, self.login.cookie.clone());
                self.login.cookie.clear();
            }
        });
    }

    // ------------------------------------------------------------- passphrase dialogs

    pub fn unlock_window(&mut self, ctx: &egui::Context) {
        if !self.pass_ui.unlock_open {
            return;
        }
        let mut unlock = false;
        let mut forget = false;
        egui::Window::new("解锁已保存的登录")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(Vec2::new(400.0, 190.0))
            .show(ctx, |ui| {
                ui.label(format!("{} 你的登录信息受口令保护，请输入口令：", egui_phosphor::regular::LOCK_KEY));
                let resp = ui
                    .add(egui::TextEdit::singleline(&mut self.pass_ui.unlock_input).password(true).desired_width(f32::INFINITY));
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    unlock = true;
                }
                if let Some(e) = &self.pass_ui.unlock_error {
                    ui.label(RichText::new(e).color(theme::BAD));
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, "解锁").clicked() {
                        unlock = true;
                    }
                    if secondary_button(ui, "先不登录").clicked() {
                        self.pass_ui.unlock_open = false;
                    }
                    if widgets::danger_button(ui, "忘记口令（清除登录）").clicked() {
                        forget = true;
                    }
                });
            });
        if unlock {
            let pass = Zeroizing::new(std::mem::take(&mut self.pass_ui.unlock_input));
            match self.store.as_ref().map(|s| s.load(Some(&pass))) {
                Some(Ok(Some(session))) => {
                    self.be.api.update_session(|s| *s = session);
                    self.passphrase = Some(pass);
                    self.pass_ui.unlock_open = false;
                    self.pass_ui.unlock_error = None;
                    self.account = self.be.api.session().account;
                    self.be.check_account();
                    if let Some(a) = &self.account {
                        self.browse.my = Loadable::Loading;
                        self.be.load_my_playlists(a.user_id);
                    }
                }
                Some(Err(StoreError::Decrypt)) => self.pass_ui.unlock_error = Some("口令不正确".into()),
                Some(Err(e)) => self.pass_ui.unlock_error = Some(e.to_string()),
                _ => self.pass_ui.unlock_open = false,
            }
        }
        if forget {
            if let Some(store) = &self.store {
                let _ = store.delete();
            }
            self.pass_ui = PassphraseUi::default();
            self.passphrase = None;
            self.toasts.push(ToastKind::Info, "已清除保存的登录，请重新登录");
        }
    }

    pub fn passphrase_window(&mut self, ctx: &egui::Context) {
        if !self.pass_ui.set_open {
            return;
        }
        let mut apply = false;
        let mut open = true;
        egui::Window::new("设置口令保护")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(Vec2::new(400.0, 230.0))
            .show(ctx, |ui| {
                widgets::hint(ui, "口令会参与加密密钥的计算，不会保存到磁盘。忘记口令只能重新登录。");
                ui.add_space(6.0);
                ui.label("新口令");
                ui.add(egui::TextEdit::singleline(&mut self.pass_ui.set_input).password(true).desired_width(f32::INFINITY));
                ui.label("再输入一次");
                ui.add(egui::TextEdit::singleline(&mut self.pass_ui.set_confirm).password(true).desired_width(f32::INFINITY));
                if let Some(e) = &self.pass_ui.set_error {
                    ui.label(RichText::new(e).color(theme::BAD));
                }
                ui.add_space(6.0);
                if primary_button(ui, "保存").clicked() {
                    apply = true;
                }
            });
        if apply {
            if self.pass_ui.set_input.chars().count() < 6 {
                self.pass_ui.set_error = Some("口令至少 6 个字符".into());
            } else if self.pass_ui.set_input != self.pass_ui.set_confirm {
                self.pass_ui.set_error = Some("两次输入不一致".into());
            } else {
                self.passphrase = Some(Zeroizing::new(std::mem::take(&mut self.pass_ui.set_input)));
                self.pass_ui = PassphraseUi { ..PassphraseUi::default() };
                self.persist_session();
                self.toasts.push(ToastKind::Success, "已启用口令保护");
                return;
            }
        }
        if !open {
            self.pass_ui.set_open = false;
            self.pass_ui.set_input.clear();
            self.pass_ui.set_confirm.clear();
            self.pass_ui.set_error = None;
        }
    }

    pub fn quit_window(&mut self, ctx: &egui::Context) {
        if !self.quit_dialog {
            return;
        }
        egui::Window::new("还有任务在下载").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, Vec2::ZERO).show(
            ctx,
            |ui| {
                ui.label(format!(
                    "还有 {} 首歌曲没有完成。退出会中断下载；已下载的部分会保留，下次启动时可以继续。",
                    self.queue.active()
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if widgets::danger_button(ui, "退出").clicked() {
                        self.force_quit = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if primary_button(ui, "继续下载").clicked() {
                        self.quit_dialog = false;
                    }
                });
            },
        );
    }
}
