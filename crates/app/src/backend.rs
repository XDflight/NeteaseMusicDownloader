//! Bridge between the async world (API client, download engine, updater) and the UI thread.
//! Background tasks send [`UiMsg`] values through a channel and wake the UI with a repaint.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use eframe::egui;
use ncm_api::{Account, Client, ClientOptions, Collection, LoginSecret, PlaylistSummary, QrState, ResourceKind, Session};
use ncm_core::{Engine, Event, tuning};
use rand::Rng;
use tokio::runtime::Handle;
use tokio::sync::mpsc::UnboundedSender;

pub enum LoginMsg {
    QrKey(Result<String, String>),
    QrState(QrState),
    /// Confirmed on the phone and the account has been fetched.
    QrDone(Result<Account, String>),
    SmsSent(Result<(), String>),
    Done(Result<Account, String>),
}

pub enum AccountError {
    /// The server says the saved cookies are no longer valid.
    NeedLogin,
    /// Anything else (typically being offline); the saved login is kept.
    Other(String),
}

// Messages are short-lived and moved once, so the size difference between variants is harmless.
#[allow(clippy::large_enum_variant)]
pub enum UiMsg {
    Engine(Event),
    /// Result of validating the saved login at start-up.
    AccountChecked(Result<Account, AccountError>),
    MyPlaylists(Result<Vec<PlaylistSummary>, String>),
    Collection {
        seq: u64,
        result: Result<Collection, String>,
    },
    Cover {
        url: String,
        bytes: Option<Vec<u8>>,
    },
    Login {
        generation: u64,
        msg: LoginMsg,
    },
    UpdateChecked {
        manual: bool,
        result: Result<Option<ncm_update::UpdateInfo>, String>,
    },
    UpdateProgress {
        done: u64,
        total: u64,
    },
    UpdateReady(Result<ncm_update::Prepared, String>),
    FolderPicked(std::path::PathBuf),
}

#[derive(Clone)]
pub struct Backend {
    pub handle: Handle,
    pub tx: UnboundedSender<UiMsg>,
    pub ctx: egui::Context,
    pub api: Client,
    pub engine: Engine,
    pub proxy: Option<String>,
}

impl Backend {
    pub fn new(
        handle: Handle,
        tx: UnboundedSender<UiMsg>,
        ctx: egui::Context,
        session: Session,
        proxy: Option<String>,
    ) -> Result<Self, String> {
        let options = ClientOptions { proxy: proxy.clone(), min_interval: tuning::API_MIN_INTERVAL, ..ClientOptions::default() };
        let api = Client::new(session, options).map_err(|e| e.to_string())?;
        let (engine, mut events) = Engine::new(&handle, api.clone(), proxy.as_deref()).map_err(|e| e.to_string())?;
        {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            handle.spawn(async move {
                while let Some(ev) = events.recv().await {
                    if tx.send(UiMsg::Engine(ev)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            });
        }
        Ok(Self { handle, tx, ctx, api, engine, proxy })
    }

    /// Run `fut` on the runtime and deliver its result to the UI.
    pub fn spawn<F>(&self, fut: F)
    where
        F: Future<Output = UiMsg> + Send + 'static,
    {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.handle.spawn(async move {
            let msg = fut.await;
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    fn send(tx: &UnboundedSender<UiMsg>, ctx: &egui::Context, msg: UiMsg) -> bool {
        let ok = tx.send(msg).is_ok();
        ctx.request_repaint();
        ok
    }

    // ------------------------------------------------------------------------ account

    pub fn check_account(&self) {
        let api = self.api.clone();
        self.spawn(async move {
            UiMsg::AccountChecked(api.refresh_account().await.map_err(|e| match e {
                ncm_api::Error::NeedLogin => AccountError::NeedLogin,
                e => AccountError::Other(e.to_string()),
            }))
        });
    }

    pub fn load_my_playlists(&self, user_id: u64) {
        let api = self.api.clone();
        self.spawn(async move { UiMsg::MyPlaylists(api.user_playlists(user_id).await.map_err(|e| e.to_string())) });
    }

    // ------------------------------------------------------------------------ browsing

    pub fn load_input(&self, seq: u64, text: String, bare: ResourceKind) {
        let api = self.api.clone();
        self.spawn(
            async move { UiMsg::Collection { seq, result: api.resolve_input(&text, bare).await.map_err(|e| e.to_string()) } },
        );
    }

    pub fn load_playlist(&self, seq: u64, id: u64) {
        let api = self.api.clone();
        self.spawn(async move { UiMsg::Collection { seq, result: api.playlist(id).await.map_err(|e| e.to_string()) } });
    }

    pub fn fetch_cover(&self, url: String) {
        let http = self.api.http().clone();
        self.spawn(async move {
            let bytes = match http.get(&url).timeout(Duration::from_secs(20)).send().await {
                Ok(r) if r.status().is_success() => r.bytes().await.ok().map(|b| b.to_vec()),
                _ => None,
            };
            UiMsg::Cover { url, bytes }
        });
    }

    // ---------------------------------------------------------------------- QR login

    /// Request a QR code and then poll it until it is confirmed, expires or `cancel` is set.
    pub fn start_qr_login(&self, generation: u64, cancel: Arc<AtomicBool>) {
        let (api, tx, ctx) = (self.api.clone(), self.tx.clone(), self.ctx.clone());
        self.handle.spawn(async move {
            let send = |m: LoginMsg| Backend::send(&tx, &ctx, UiMsg::Login { generation, msg: m });
            let key = match api.qr_login_key().await {
                Ok(k) => k,
                Err(e) => {
                    send(LoginMsg::QrKey(Err(e.to_string())));
                    return;
                }
            };
            send(LoginMsg::QrKey(Ok(key.clone())));
            let mut last: Option<QrState> = None;
            loop {
                // Not a fixed metronome: 1.2-1.9 s between polls.
                let delay = Duration::from_millis(rand::rng().random_range(1200..1900));
                tokio::time::sleep(delay).await;
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let state = match api.qr_login_check(&key).await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!("QR poll failed: {e}");
                        continue;
                    }
                };
                if last.as_ref() != Some(&state) {
                    last = Some(state.clone());
                    send(LoginMsg::QrState(state.clone()));
                }
                match state {
                    QrState::Confirmed => {
                        let account = api.refresh_account().await.map_err(|e| e.to_string());
                        send(LoginMsg::QrDone(account));
                        return;
                    }
                    QrState::Expired | QrState::Other { .. } => return,
                    _ => {}
                }
            }
        });
    }

    // -------------------------------------------------------------------- phone / cookie

    pub fn send_sms(&self, generation: u64, phone: String, country: String) {
        let api = self.api.clone();
        self.spawn(async move {
            UiMsg::Login {
                generation,
                msg: LoginMsg::SmsSent(api.send_sms_captcha(&phone, &country).await.map_err(|e| e.to_string())),
            }
        });
    }

    pub fn login_phone(&self, generation: u64, phone: String, country: String, secret: String, captcha: bool) {
        let api = self.api.clone();
        self.spawn(async move {
            let s = if captcha { LoginSecret::Captcha(&secret) } else { LoginSecret::Password(&secret) };
            let result = api.login_cellphone(&phone, &country, s).await.map_err(|e| e.to_string());
            UiMsg::Login { generation, msg: LoginMsg::Done(result) }
        });
    }

    pub fn login_cookie(&self, generation: u64, text: String) {
        let api = self.api.clone();
        self.spawn(async move {
            let imported = api.update_session(|s| s.import_cookie_text(&text));
            let result = if imported {
                api.refresh_account().await.map_err(|_| "Cookie 无效或已过期".to_owned())
            } else {
                Err("没有找到 MUSIC_U，请粘贴 MUSIC_U 的值或整段 Cookie".to_owned())
            };
            if result.is_err() {
                api.sign_out_local();
            }
            UiMsg::Login { generation, msg: LoginMsg::Done(result) }
        });
    }

    /// Native folder chooser on its own thread (the dialog blocks).
    pub fn pick_folder(&self, start: std::path::PathBuf) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title("选择保存位置");
            if start.is_dir() {
                dialog = dialog.set_directory(&start);
            }
            if let Some(path) = dialog.pick_folder() {
                let _ = tx.send(UiMsg::FolderPicked(path));
                ctx.request_repaint();
            }
        });
    }

    // ------------------------------------------------------------------------- updates

    pub fn check_update(&self, manual: bool, current: String, prerelease: bool) {
        let proxy = self.proxy.clone();
        self.spawn(async move {
            let mut cfg = ncm_update::UpdateConfig::new(&current);
            cfg.include_prerelease = prerelease;
            cfg.proxy = proxy;
            UiMsg::UpdateChecked { manual, result: ncm_update::check(&cfg).await.map_err(|e| e.to_string()) }
        });
    }

    pub fn download_update(&self, info: ncm_update::UpdateInfo, current: String, prerelease: bool) {
        let (proxy, tx, ctx) = (self.proxy.clone(), self.tx.clone(), self.ctx.clone());
        self.handle.spawn(async move {
            let mut cfg = ncm_update::UpdateConfig::new(&current);
            cfg.include_prerelease = prerelease;
            cfg.proxy = proxy;
            let (ptx, pctx) = (tx.clone(), ctx.clone());
            let result = ncm_update::download(&cfg, &info, move |done, total| {
                let _ = ptx.send(UiMsg::UpdateProgress { done, total });
                pctx.request_repaint();
            })
            .await
            .map_err(|e| e.to_string());
            Backend::send(&tx, &ctx, UiMsg::UpdateReady(result));
        });
    }
}
