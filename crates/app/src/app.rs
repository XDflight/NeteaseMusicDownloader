use std::collections::VecDeque;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Align2, Color32, CornerRadius, FontId, Layout, Margin, Rect, RichText, Sense, Stroke, Vec2};
use ncm_api::{Account, Level, Session};
use ncm_core::adaptive::NetSnapshot;
use ncm_core::{AppPaths, BatchRequest, Event, SecureStore, Settings, StoreError, TrackJob, TrackUpdate};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use zeroize::Zeroizing;

use crate::assets::{Assets, Mood};
use crate::backend::{AccountError, Backend, LoginMsg, UiMsg};
use crate::state::*;
use crate::theme::{self, ACCENT, ACCENT_DEEP, pal};
use crate::widgets::{self, nav_button};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const REPO_URL: &str = "https://github.com/XDflight/NeteaseMusicDownloader";

pub struct App {
    pub ctx: egui::Context,
    _runtime: tokio::runtime::Runtime,
    pub be: Backend,
    rx: UnboundedReceiver<UiMsg>,
    pub tx: UnboundedSender<UiMsg>,

    pub paths: AppPaths,
    pub settings: Settings,
    pub store: Option<SecureStore>,
    pub passphrase: Option<Zeroizing<String>>,

    pub assets: Assets,
    pub page: Page,
    pub account: Option<Account>,
    pub login: LoginUi,
    pub browse: Browse,
    pub queue: Queue,
    pub toasts: Toasts,
    pub pass_ui: PassphraseUi,
    pub update: UpdateState,
    pub net: NetSnapshot,
    pub net_history: VecDeque<f32>,
    pub mood_until: Option<(Mood, Instant)>,
    pub started: Instant,
    pub quit_dialog: bool,
    pub force_quit: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let paths = AppPaths::discover();
        let settings = Settings::load(&paths);

        crate::fonts::install(&ctx);
        theme::apply(&ctx);
        apply_theme_preference(&ctx, settings.theme);
        let assets = Assets::load(&ctx);

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .thread_name("ncm-worker")
            .build()
            .expect("tokio runtime");
        let (tx, rx) = unbounded_channel();

        // Saved login (encrypted with a machine-bound key, optionally plus a passphrase).
        let mut toasts = Toasts::default();
        let mut pass_ui = PassphraseUi::default();
        let store = match SecureStore::open(&paths.data_dir) {
            Ok(s) => Some(s),
            Err(e) => {
                toasts.push(ToastKind::Error, format!("无法初始化凭据存储：{e}"));
                None
            }
        };
        let mut session = Session::new();
        let mut locked = false;
        if let Some(store) = &store {
            match store.needs_passphrase() {
                Ok(true) => {
                    locked = true;
                    pass_ui.unlock_open = true;
                }
                Ok(false) => match store.load(None) {
                    Ok(Some(s)) => session = s,
                    Ok(None) => {}
                    Err(StoreError::Decrypt) | Err(StoreError::Format) => {
                        toasts.push(ToastKind::Warning, "无法读取已保存的登录信息（可能来自另一台电脑），请重新登录");
                    }
                    Err(e) => toasts.push(ToastKind::Error, format!("读取登录信息失败：{e}")),
                },
                Err(e) => toasts.push(ToastKind::Error, format!("读取登录信息失败：{e}")),
            }
        }
        let logged_in = session.is_logged_in();
        let account = session.account.clone().filter(|_| logged_in);

        let proxy = Some(settings.proxy.clone()).filter(|p| !p.trim().is_empty());
        let be = Backend::new(runtime.handle().clone(), tx.clone(), ctx.clone(), session, proxy)
            .unwrap_or_else(|e| panic!("cannot start networking: {e}"));
        if logged_in {
            be.check_account();
        }
        if settings.update.auto_check && now_unix().saturating_sub(settings.update.last_check) > 6 * 3600 {
            be.check_update(false, VERSION.to_owned(), settings.update.include_prerelease);
        }

        let mut app = Self {
            ctx,
            _runtime: runtime,
            be,
            rx,
            tx,
            paths,
            settings,
            store,
            passphrase: None,
            assets,
            page: Page::Browse,
            account,
            login: LoginUi::default(),
            browse: Browse::default(),
            queue: Queue::default(),
            toasts,
            pass_ui,
            update: UpdateState::Idle,
            net: NetSnapshot::default(),
            net_history: VecDeque::new(),
            mood_until: None,
            started: Instant::now(),
            quit_dialog: false,
            force_quit: false,
        };
        if locked {
            app.account = None;
        } else if let Some(a) = &app.account {
            app.browse.my = Loadable::Loading;
            app.be.load_my_playlists(a.user_id);
        }
        app
    }

    // ------------------------------------------------------------------ persistence

    pub fn save_settings(&mut self) {
        if let Err(e) = self.settings.save(&self.paths) {
            self.toasts.push(ToastKind::Error, format!("保存设置失败：{e}"));
        }
    }

    /// Store the current session (cookies, account, device) encrypted on disk.
    pub fn persist_session(&mut self) {
        let Some(store) = &self.store else { return };
        let session = self.be.api.session();
        let pass = self.passphrase.as_ref().map(|p| p.as_str());
        if let Err(e) = store.save(&session, pass) {
            self.toasts.push(ToastKind::Error, format!("保存登录信息失败：{e}"));
        }
    }

    pub fn is_logged_in(&self) -> bool {
        self.account.is_some() && self.be.api.is_logged_in()
    }

    pub fn sign_out(&mut self) {
        let api = self.be.api.clone();
        self.be.handle.spawn(async move {
            let _ = api.logout().await;
        });
        // Clear locally right away; keep the device identity so the next login looks the same.
        self.be.api.sign_out_local();
        self.account = None;
        self.browse.my = Loadable::Idle;
        self.persist_session();
        self.toasts.push(ToastKind::Info, "已退出登录");
    }

    /// Rebuild client and engine (used after the proxy changes); keeps the session.
    pub fn rebuild_backend(&mut self) {
        self.be.engine.cancel_all();
        self.be.engine.shutdown();
        let session = self.be.api.session();
        let proxy = Some(self.settings.proxy.clone()).filter(|p| !p.trim().is_empty());
        match Backend::new(self.be.handle.clone(), self.tx.clone(), self.ctx.clone(), session, proxy) {
            Ok(be) => {
                self.be = be;
                self.toasts.push(ToastKind::Success, "网络设置已应用");
            }
            Err(e) => self.toasts.push(ToastKind::Error, format!("网络设置无效：{e}")),
        }
    }

    // --------------------------------------------------------------------- messages

    fn pump(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.handle(msg);
        }
    }

    fn handle(&mut self, msg: UiMsg) {
        match msg {
            UiMsg::Engine(ev) => self.on_engine(ev),
            UiMsg::AccountChecked(Ok(a)) => {
                self.account = Some(a.clone());
                self.persist_session();
                if matches!(self.browse.my, Loadable::Idle | Loadable::Failed(_)) {
                    self.browse.my = Loadable::Loading;
                    self.be.load_my_playlists(a.user_id);
                }
            }
            UiMsg::AccountChecked(Err(AccountError::NeedLogin)) => {
                self.be.api.sign_out_local();
                self.account = None;
                self.persist_session();
                self.toasts.push(ToastKind::Warning, "登录已失效，请重新登录");
            }
            UiMsg::AccountChecked(Err(AccountError::Other(e))) => {
                tracing::warn!("could not verify the saved login: {e}");
                self.toasts.push(ToastKind::Warning, "暂时无法验证登录状态（网络问题？），已沿用保存的登录");
            }
            UiMsg::MyPlaylists(Ok(list)) => self.browse.my = Loadable::Ready(list),
            UiMsg::MyPlaylists(Err(e)) => self.browse.my = Loadable::Failed(e),
            UiMsg::Collection { seq, result } => {
                if seq != self.browse.request_seq {
                    return;
                }
                self.browse.loading = false;
                match result {
                    Ok(col) => {
                        self.browse.error = None;
                        let mut view = CollectionView::new(col);
                        if let Some(url) = view.col.cover_url.clone() {
                            let sized = ncm_core::CoverSize::Px500.apply(&url);
                            view.cover_url = Some(sized.clone());
                            self.be.fetch_cover(sized);
                        }
                        self.browse.view = Some(view);
                    }
                    Err(e) => self.browse.error = Some(e),
                }
            }
            UiMsg::Cover { url, bytes } => {
                if let (Some(bytes), Some(view)) =
                    (bytes, self.browse.view.as_mut().filter(|v| v.cover_url.as_deref() == Some(url.as_str())))
                    && let Ok(img) = image::load_from_memory(&bytes)
                {
                    let img = img.to_rgba8();
                    let size = [img.width() as usize, img.height() as usize];
                    view.cover = Some(self.ctx.load_texture(
                        "collection-cover",
                        egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()),
                        egui::TextureOptions::LINEAR,
                    ));
                }
            }
            UiMsg::Login { generation, msg } => self.on_login(generation, msg),
            UiMsg::UpdateChecked { manual, result } => match result {
                Ok(Some(info)) => {
                    let skipped =
                        self.settings.update.skipped_version.as_deref() == Some(info.release.version.to_string().as_str());
                    if manual || !skipped {
                        self.update = UpdateState::Available(Box::new(info));
                    } else {
                        self.update = UpdateState::Idle;
                    }
                    self.settings.update.last_check = now_unix();
                    self.save_settings();
                }
                Ok(None) => {
                    self.settings.update.last_check = now_unix();
                    self.save_settings();
                    self.update = if manual { UpdateState::UpToDate } else { UpdateState::Idle };
                }
                Err(e) => {
                    tracing::warn!("update check failed: {e}");
                    if manual {
                        self.update = UpdateState::Failed(e);
                    } else {
                        self.update = UpdateState::Idle;
                    }
                }
            },
            UiMsg::FolderPicked(path) => {
                self.settings.download.output_dir = path;
                self.save_settings();
            }
            UiMsg::UpdateProgress { done, total } => self.update = UpdateState::Downloading { done, total },
            UiMsg::UpdateReady(Ok(prepared)) => match ncm_update::apply(prepared) {
                Ok(_) => {
                    self.force_quit = true;
                    self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                Err(e) => {
                    self.update = UpdateState::Failed(e.to_string());
                    self.toasts.push(ToastKind::Error, format!("更新失败：{e}"));
                }
            },
            UiMsg::UpdateReady(Err(e)) => {
                self.update = UpdateState::Failed(e.clone());
                self.toasts.push(ToastKind::Error, format!("下载更新失败：{e}"));
            }
        }
    }

    fn on_login(&mut self, generation: u64, msg: LoginMsg) {
        if generation != self.login.qr.generation {
            return;
        }
        match msg {
            LoginMsg::QrKey(Ok(key)) => {
                let url = ncm_api::Client::qr_login_url(&key);
                match qrcode::QrCode::new(url.as_bytes()) {
                    Ok(code) => {
                        let size = code.width();
                        self.login.qr.modules = code.to_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect();
                        self.login.qr.size = size;
                        self.login.qr.phase = QrPhase::Waiting;
                    }
                    Err(e) => self.login.qr.phase = QrPhase::Failed(format!("生成二维码失败：{e}")),
                }
            }
            LoginMsg::QrKey(Err(e)) => self.login.qr.phase = QrPhase::Failed(e),
            LoginMsg::QrState(state) => match state {
                ncm_api::QrState::Waiting => self.login.qr.phase = QrPhase::Waiting,
                ncm_api::QrState::Scanned { nickname, .. } => self.login.qr.phase = QrPhase::Scanned(nickname),
                ncm_api::QrState::Expired => self.login.qr.phase = QrPhase::Expired,
                ncm_api::QrState::Other { message, code } => self.login.qr.phase = QrPhase::Failed(format!("{message} ({code})")),
                ncm_api::QrState::Confirmed => {}
            },
            LoginMsg::QrDone(result) | LoginMsg::Done(result) => {
                self.login.busy = false;
                match result {
                    Ok(account) => self.finish_login(account),
                    Err(e) => {
                        self.login.error = Some(e.clone());
                        self.login.qr.phase = QrPhase::Failed(e);
                    }
                }
            }
            LoginMsg::SmsSent(result) => {
                self.login.sending = false;
                match result {
                    Ok(()) => self.toasts.push(ToastKind::Success, "验证码已发送"),
                    Err(e) => self.login.error = Some(e),
                }
            }
        }
    }

    pub fn finish_login(&mut self, account: Account) {
        self.login.open = false;
        self.login.qr.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        self.login.secret.clear();
        self.login.error = None;
        self.toasts.push(ToastKind::Success, format!("欢迎回来，{}", account.nickname));
        self.browse.my = Loadable::Loading;
        self.be.load_my_playlists(account.user_id);
        self.account = Some(account);
        self.persist_session();
    }

    fn on_engine(&mut self, ev: Event) {
        match ev {
            Event::Net(n) => {
                self.net = n;
                self.net_history.push_back(n.bytes_per_sec as f32);
                while self.net_history.len() > 90 {
                    self.net_history.pop_front();
                }
            }
            Event::Track { batch, id, update } => {
                let Some(&i) = self.queue.lookup.get(&(batch, id)) else { return };
                let it = &mut self.queue.items[i];
                match update {
                    TrackUpdate::Stage(s) => it.state = QState::Working(s),
                    TrackUpdate::Delivered { level, ext, size } => {
                        let label = Level::parse(&level).map(|l| l.label()).unwrap_or(level.as_str()).to_owned();
                        it.quality = Some(format!("{label} · {}", ext.to_uppercase()));
                        it.total = size;
                    }
                    TrackUpdate::PartFile(_) => {}
                    TrackUpdate::Progress { done, total } => {
                        it.done = done;
                        it.total = total;
                    }
                    TrackUpdate::Done { path, bytes, warnings, .. } => {
                        it.state = QState::Done;
                        it.done = bytes;
                        it.total = bytes;
                        it.path = Some(path);
                        it.message = (!warnings.is_empty()).then(|| warnings.join("；"));
                    }
                    TrackUpdate::Skipped { existing } => {
                        it.state = QState::Skipped;
                        it.path = Some(existing);
                        it.message = Some("文件已存在，已跳过".into());
                    }
                    TrackUpdate::Failed { kind, message } => {
                        it.state = QState::Failed(kind);
                        it.message = Some(message);
                    }
                    TrackUpdate::Cancelled => it.state = QState::Cancelled,
                }
            }
            Event::Paused(paused) => self.queue.paused = paused,
            Event::BatchFinished { batch, summary } => {
                if let Some(b) = self.queue.batches.get_mut(&batch) {
                    b.summary = Some(summary);
                }
                if summary.cancelled > 0 && summary.done == 0 && summary.failed == 0 {
                    self.toasts.push(ToastKind::Info, "下载已取消");
                } else if summary.failed > 0 {
                    self.toasts.push(
                        ToastKind::Warning,
                        format!("完成 {} 首，失败 {} 首，跳过 {} 首", summary.done, summary.failed, summary.skipped),
                    );
                    if summary.done == 0 {
                        self.mood_until = Some((Mood::Sad, Instant::now() + Duration::from_secs(8)));
                    }
                } else {
                    self.toasts.push(
                        ToastKind::Success,
                        format!(
                            "全部完成：下载 {} 首（{}），跳过 {} 首",
                            summary.done,
                            widgets::format_bytes(summary.bytes),
                            summary.skipped
                        ),
                    );
                    self.mood_until = Some((Mood::Happy, Instant::now() + Duration::from_secs(8)));
                }
            }
        }
    }

    // ----------------------------------------------------------------------- actions

    /// Queue the given tracks of the current collection.
    pub fn submit_download(&mut self, picks: Vec<usize>) {
        let Some(view) = self.browse.view.as_ref() else { return };
        if picks.is_empty() {
            return;
        }
        let tracks: Vec<TrackJob> = picks.iter().map(|&i| TrackJob { track: view.col.tracks[i].clone(), index: i + 1 }).collect();
        let req = BatchRequest {
            collection: view.col.name.clone(),
            collection_cover: view.col.cover_url.clone(),
            tracks: tracks.clone(),
            options: self.settings.download.clone(),
        };
        let batch = self.be.engine.submit(req);
        self.register_batch(batch, tracks);
        self.toasts.push(ToastKind::Info, format!("已加入下载队列：{} 首", picks.len()));
    }

    fn register_batch(&mut self, batch: ncm_core::BatchId, tracks: Vec<TrackJob>) {
        self.queue.batches.insert(batch, BatchInfo { summary: None });
        for job in tracks {
            let n = self.queue.items.len();
            self.queue.lookup.insert((batch, job.track.id), n);
            self.queue.items.push(QItem {
                batch,
                track: job.track,
                index: job.index,
                state: QState::Queued,
                done: 0,
                total: 0,
                quality: None,
                path: None,
                message: None,
            });
        }
    }

    pub fn retry_failed(&mut self) {
        let failed: Vec<TrackJob> = self
            .queue
            .items
            .iter()
            .filter(|i| matches!(i.state, QState::Failed(_) | QState::Cancelled))
            .map(|i| TrackJob { track: i.track.clone(), index: i.index })
            .collect();
        if failed.is_empty() {
            return;
        }
        let ids: std::collections::HashSet<u64> = failed.iter().map(|j| j.track.id).collect();
        self.queue.items.retain(|i| !(matches!(i.state, QState::Failed(_) | QState::Cancelled) && ids.contains(&i.track.id)));
        self.queue.lookup = self.queue.items.iter().enumerate().map(|(n, i)| ((i.batch, i.track.id), n)).collect();
        let req = BatchRequest {
            collection: "重试".into(),
            collection_cover: None,
            tracks: failed.clone(),
            options: self.settings.download.clone(),
        };
        let batch = self.be.engine.submit(req);
        self.register_batch(batch, failed);
    }

    fn mood(&self) -> Mood {
        if let Some((m, until)) = self.mood_until
            && Instant::now() < until
        {
            return m;
        }
        if self.queue.active() > 0 {
            Mood::Working
        } else if !self.is_logged_in() {
            Mood::Login
        } else if self.page == Page::Browse && self.browse.view.is_none() {
            Mood::Search
        } else {
            Mood::Idle
        }
    }

    // ------------------------------------------------------------------------ layout

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let p = pal(ui);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(egui::Image::new(&self.assets.logo).fit_to_exact_size(Vec2::splat(46.0)));
            ui.vertical(|ui| {
                ui.add_space(2.0);
                ui.label(RichText::new("云音下载姬").size(19.0).strong().color(ACCENT_DEEP));
                ui.label(RichText::new("Netease Music Downloader").size(10.5).color(p.muted));
            });
        });
        ui.add_space(14.0);

        for (page, icon, label) in [
            (Page::Browse, egui_phosphor::regular::MAGNIFYING_GLASS, "解析与选择"),
            (Page::Queue, egui_phosphor::regular::DOWNLOAD_SIMPLE, "下载队列"),
            (Page::Settings, egui_phosphor::regular::SLIDERS_HORIZONTAL, "设置"),
            (Page::About, egui_phosphor::regular::HEART, "关于"),
        ] {
            let badge = (page == Page::Queue).then(|| self.queue.active());
            if nav_button(ui, icon, label, self.page == page, badge).clicked() {
                self.page = page;
            }
        }

        // Account card pinned to the bottom, mascot stage in the space above it.
        let region = ui.available_rect_before_wrap();
        let chip_h = 158.0;
        let chip_rect = Rect::from_min_max(egui::pos2(region.left(), (region.bottom() - chip_h).max(region.top())), region.max);
        ui.scope_builder(egui::UiBuilder::new().max_rect(chip_rect).layout(Layout::top_down(Align::Min)), |ui| {
            self.account_chip(ui)
        });

        let stage_bottom = chip_rect.top() - 10.0;
        let stage_h = (stage_bottom - (region.top() + 10.0)).min(330.0);
        if stage_h >= 110.0 {
            let stage =
                Rect::from_min_size(egui::pos2(region.left(), stage_bottom - stage_h), Vec2::new(region.width(), stage_h));
            let tex = self.assets.mascot(self.mood()).clone();
            let aspect = tex.size()[1] as f32 / tex.size()[0] as f32;
            let time = self.started.elapsed().as_secs_f64();
            theme::mascot_stage(ui, stage, time);
            let mut h = stage_h - 18.0;
            let mut w = h / aspect;
            if w > stage.width() - 14.0 {
                w = stage.width() - 14.0;
                h = w * aspect;
            }
            let sprite = Rect::from_center_size(stage.center() + Vec2::new(0.0, 4.0), Vec2::new(w, h));
            ui.painter().image(tex.id(), sprite, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
    }

    fn account_chip(&mut self, ui: &mut egui::Ui) {
        let p = pal(ui);
        theme::card_frame(ui).inner_margin(Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            self.account_chip_body(ui, p);
        });
    }

    fn account_chip_body(&mut self, ui: &mut egui::Ui, p: theme::Palette) {
        {
            match self.account.clone() {
                Some(a) => {
                    ui.horizontal(|ui| {
                        let initial = a.nickname.chars().next().unwrap_or('?').to_string();
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(38.0), Sense::hover());
                        ui.painter().circle_filled(rect.center(), 19.0, ACCENT);
                        ui.painter().text(
                            rect.center(),
                            Align2::CENTER_CENTER,
                            initial,
                            FontId::proportional(18.0),
                            Color32::WHITE,
                        );
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&a.nickname).strong());
                            let (fill, fg) = if is_vip(&a) { (ACCENT_DEEP, Color32::WHITE) } else { (p.card_alt, p.muted) };
                            widgets::pill(ui, account_label(&a), fill, fg);
                        });
                    });
                    if widgets::secondary_button(ui, "退出登录").clicked() {
                        self.sign_out();
                    }
                }
                None => {
                    ui.label(RichText::new("未登录").strong());
                    widgets::hint(ui, "登录后可下载 VIP 歌曲与私有歌单");
                    if widgets::primary_button(ui, "登录账号").clicked() {
                        self.open_login();
                    }
                }
            }
        }
    }

    /// Banner with the page title on the left and the mascot peeking in on the right.
    pub fn page_header(&mut self, ui: &mut egui::Ui, title: &str, subtitle: &str) {
        let width = ui.available_width();
        let height = 136.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
        let rounding = CornerRadius::same(20);
        let banner = &self.assets.banner;
        let [bw, bh] = banner.size();
        let img_aspect = bw as f32 / bh as f32;
        // "Cover" crop of the wide banner, anchored so the mascot's face (upper right) stays in view.
        let want = width / height;
        let uv = if want > img_aspect {
            let visible_h = img_aspect / want;
            let top = (0.27 - visible_h / 2.0).clamp(0.0, 1.0 - visible_h);
            Rect::from_min_max(egui::pos2(0.0, top), egui::pos2(1.0, top + visible_h))
        } else {
            let visible_w = want / img_aspect;
            Rect::from_min_max(egui::pos2(1.0 - visible_w, 0.0), egui::pos2(1.0, 1.0))
        };
        egui::Image::new(banner).corner_radius(rounding).uv(uv).paint_at(ui, rect);
        // The banner is always light, so the text is always dark; a veil keeps it readable over
        // the busier left side.
        let veil = Rect::from_min_size(rect.min, Vec2::new(rect.width() * 0.6, rect.height()));
        theme::gradient_rect(
            &ui.painter().with_clip_rect(rect),
            veil,
            0.0,
            [
                Color32::from_white_alpha(170),
                Color32::from_white_alpha(0),
                Color32::from_white_alpha(0),
                Color32::from_white_alpha(170),
            ],
        );
        theme::sakura_petals(ui, rect, self.started.elapsed().as_secs_f64(), 14);
        let text_color = Color32::from_rgb(0x4A, 0x1F, 0x26);
        ui.painter().text(
            rect.left_top() + Vec2::new(28.0, 44.0),
            Align2::LEFT_CENTER,
            title,
            FontId::proportional(30.0),
            text_color,
        );
        ui.painter().text(
            rect.left_top() + Vec2::new(30.0, 84.0),
            Align2::LEFT_CENTER,
            subtitle,
            FontId::proportional(14.5),
            text_color.gamma_multiply(0.85),
        );
        ui.painter().rect_stroke(rect, rounding, Stroke::new(1.0, pal(ui).line), egui::StrokeKind::Inside);
    }

    fn update_banner(&mut self, ui: &mut egui::Ui) {
        let mut action: Option<UpdateAction> = None;
        match &self.update {
            UpdateState::Available(info) => {
                let version = info.release.version.to_string();
                theme::card_frame(ui)
                    .fill(theme::ACCENT_SOFT.gamma_multiply(if ui.visuals().dark_mode { 0.25 } else { 1.0 }))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(egui_phosphor::regular::SPARKLE).size(22.0).color(ACCENT_DEEP));
                            ui.vertical(|ui| {
                                ui.label(RichText::new(format!("发现新版本 v{version}（当前 v{VERSION}）")).strong());
                                let notes: String = info
                                    .release
                                    .notes
                                    .lines()
                                    .find(|l| !l.trim().is_empty())
                                    .unwrap_or("")
                                    .chars()
                                    .take(90)
                                    .collect();
                                if !notes.is_empty() {
                                    widgets::hint(ui, &notes);
                                }
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if widgets::primary_button(ui, "立即更新").clicked() {
                                    action = Some(UpdateAction::Install);
                                }
                                if widgets::secondary_button(ui, "跳过此版本").clicked() {
                                    action = Some(UpdateAction::Skip(version.clone()));
                                }
                                if widgets::secondary_button(ui, "稍后").clicked() {
                                    action = Some(UpdateAction::Later);
                                }
                            });
                        });
                    });
                ui.add_space(8.0);
            }
            UpdateState::Downloading { done, total } => {
                theme::card_frame(ui).show(ui, |ui| {
                    ui.label(
                        RichText::new(format!(
                            "正在下载更新… {} / {}",
                            widgets::format_bytes(*done),
                            widgets::format_bytes(*total)
                        ))
                        .strong(),
                    );
                    widgets::progress_bar(ui, *done as f32 / (*total).max(1) as f32, 10.0);
                });
                ui.add_space(8.0);
            }
            _ => {}
        }
        match action {
            Some(UpdateAction::Install) => {
                if let UpdateState::Available(info) =
                    std::mem::replace(&mut self.update, UpdateState::Downloading { done: 0, total: 0 })
                {
                    self.be.download_update(*info, VERSION.to_owned(), self.settings.update.include_prerelease);
                }
            }
            Some(UpdateAction::Skip(v)) => {
                self.settings.update.skipped_version = Some(v);
                self.save_settings();
                self.update = UpdateState::Idle;
            }
            Some(UpdateAction::Later) => self.update = UpdateState::Idle,
            None => {}
        }
    }

    fn toasts_ui(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.toasts.0.retain(|t| now.duration_since(t.born) < Duration::from_secs(6));
        if self.toasts.0.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(200));
        let screen = ctx.content_rect();
        let mut y = screen.bottom() - 18.0;
        for (i, t) in self.toasts.0.iter().enumerate().rev() {
            let (icon, color) = match t.kind {
                ToastKind::Info => (egui_phosphor::regular::INFO, theme::SECONDARY_DEEP),
                ToastKind::Success => (egui_phosphor::regular::CHECK_CIRCLE, theme::GOOD),
                ToastKind::Warning => (egui_phosphor::regular::WARNING, theme::WARN),
                ToastKind::Error => (egui_phosphor::regular::X_CIRCLE, theme::BAD),
            };
            let age = now.duration_since(t.born).as_secs_f32();
            let alpha = (1.0 - ((age - 5.0).max(0.0))).clamp(0.0, 1.0);
            let area = egui::Area::new(egui::Id::new(("toast", i)))
                .order(egui::Order::Tooltip)
                .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-18.0, -(screen.bottom() - y)))
                .interactable(false);
            let resp = area.show(ctx, |ui| {
                ui.set_opacity(alpha);
                theme::card_frame(ui).inner_margin(Margin::symmetric(14, 10)).stroke(Stroke::new(1.5, color)).show(ui, |ui| {
                    ui.set_max_width(380.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon).size(18.0).color(color));
                        ui.label(&t.text);
                    });
                });
            });
            y -= resp.response.rect.height() + 8.0;
        }
    }
}

enum UpdateAction {
    Install,
    Skip(String),
    Later,
}

pub fn apply_theme_preference(ctx: &egui::Context, theme: ncm_core::Theme) {
    ctx.set_theme(match theme {
        ncm_core::Theme::System => egui::ThemePreference::System,
        ncm_core::Theme::Light => egui::ThemePreference::Light,
        ncm_core::Theme::Dark => egui::ThemePreference::Dark,
    });
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump();
        if ctx.input(|i| i.viewport().close_requested())
            && !self.force_quit
            && self.queue.active() > 0
            && self.settings.confirm_quit_while_downloading
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.quit_dialog = true;
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let p = pal(ui);
        ui.painter().rect_filled(ui.max_rect(), 0.0, p.bg);

        egui::Panel::left("nav")
            .exact_size(232.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin::same(14)))
            .show(ui, |ui| self.sidebar(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(Margin { left: 4, right: 18, top: 14, bottom: 14 }))
            .show(ui, |ui| {
                self.update_banner(ui);
                match self.page {
                    Page::Browse => self.page_browse(ui),
                    Page::Queue => self.page_queue(ui),
                    Page::Settings => self.page_settings(ui),
                    Page::About => self.page_about(ui),
                }
            });

        self.login_window(&ctx);
        self.unlock_window(&ctx);
        self.passphrase_window(&ctx);
        self.quit_window(&ctx);
        self.toasts_ui(&ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.be.engine.cancel_all();
        self.save_settings();
        self.persist_session();
        self.be.engine.shutdown();
    }
}
