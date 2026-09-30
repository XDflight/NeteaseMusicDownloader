use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rand::Rng;
use reqwest::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::crypto::{eapi_decrypt, eapi_params};
use crate::error::{Error, Result};
use crate::session::Session;

const EAPI_HOST: &str = "https://interface.music.163.com";
/// The User-Agent of the official 3.1.23 desktop client (a 32-bit process, hence `WOW64`), taken
/// verbatim from the client. Also used for the CDN downloads so that one identity is presented.
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; WOW64) AppleWebKit/537.36 (KHTML, like Gecko) Safari/537.36 Chrome/91.0.4472.164 NeteaseMusicDesktop/3.1.23.204814";

static INSTALL_TLS: Once = Once::new();

/// Install the process-wide rustls crypto provider (idempotent).
pub fn install_tls_provider() {
    INSTALL_TLS.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// `http://`, `https://` or `socks5://` proxy for API traffic.
    pub proxy: Option<String>,
    /// Minimum spacing between two API calls; keeps bulk operations under the risk-control radar.
    pub min_interval: Duration,
    pub timeout: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self { proxy: None, min_interval: Duration::from_millis(120), timeout: Duration::from_secs(20) }
    }
}

/// Cheaply clonable handle to the API. All clones share the same session.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    session: RwLock<Session>,
    gate: Mutex<Instant>,
    min_interval: Duration,
    /// Whether the guest-token check has already run for the current login state.
    guest_checked: AtomicBool,
}

impl Client {
    pub fn new(session: Session, opts: ClientOptions) -> Result<Self> {
        install_tls_provider();
        let mut builder = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(10))
            .timeout(opts.timeout)
            .pool_idle_timeout(Duration::from_secs(30));
        if let Some(proxy) = opts.proxy.as_deref().filter(|p| !p.trim().is_empty()) {
            builder = builder.proxy(reqwest::Proxy::all(proxy.trim())?);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                http: builder.build()?,
                session: RwLock::new(session),
                gate: Mutex::new(Instant::now()),
                min_interval: opts.min_interval,
                guest_checked: AtomicBool::new(false),
            }),
        })
    }

    /// A copy of the current session (cookies, device, account).
    pub fn session(&self) -> Session {
        self.inner.session.read().unwrap().clone()
    }

    pub fn update_session<R>(&self, f: impl FnOnce(&mut Session) -> R) -> R {
        f(&mut self.inner.session.write().unwrap())
    }

    pub fn is_logged_in(&self) -> bool {
        self.inner.session.read().unwrap().is_logged_in()
    }

    /// Forget the login (keeping the device identity). The next request registers a fresh guest
    /// token, like the desktop client does after signing out.
    pub fn sign_out_local(&self) {
        self.update_session(|s| s.sign_out());
        self.inner.guest_checked.store(false, Ordering::Release);
    }

    /// The desktop client always carries either a login (`MUSIC_U`) or an anonymous guest token
    /// (`MUSIC_A`); a session with neither is unlike anything it produces. Obtain the guest token
    /// once, before the first request that needs it.
    async fn ensure_guest_first(&self, path: &str) {
        if path == "/register/anonimous" || self.inner.guest_checked.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Err(e) = self.ensure_guest_token().await {
            tracing::debug!("guest token registration failed: {e}");
        }
    }

    /// Plain HTTP client sharing this client's proxy/TLS setup (used to resolve share short links).
    pub fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    /// The `header` object the desktop client embeds in every request (also sent as cookies).
    fn client_header(&self) -> Vec<(String, String)> {
        let s = self.inner.session.read().unwrap();
        let d = &s.device;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let mut h = vec![
            ("osver".to_owned(), d.osver.clone()),
            ("deviceId".to_owned(), d.device_id.clone()),
            ("os".to_owned(), d.os.clone()),
            ("appver".to_owned(), d.appver.clone()),
            ("versioncode".to_owned(), d.versioncode.clone()),
            ("mobilename".to_owned(), String::new()),
            ("buildver".to_owned(), d.buildver.clone()),
            ("resolution".to_owned(), d.resolution.clone()),
            ("__csrf".to_owned(), s.cookies.get("__csrf").cloned().unwrap_or_default()),
            ("channel".to_owned(), d.channel.clone()),
            ("requestId".to_owned(), format!("{}_{:04}", now.as_millis(), rand::rng().random_range(0..10000))),
        ];
        for key in ["MUSIC_U", "MUSIC_A"] {
            if let Some(v) = s.cookies.get(key).filter(|v| !v.is_empty()) {
                h.push((key.to_owned(), v.clone()));
            }
        }
        h
    }

    async fn throttle(&self) {
        let mut last = self.inner.gate.lock().await;
        // Jitter: a metronome-regular request rhythm is the least human thing there is.
        let interval = self.inner.min_interval.mul_f64(rand::rng().random_range(0.75..1.5));
        let next = *last + interval;
        let now = Instant::now();
        if next > now {
            tokio::time::sleep(next - now).await;
            *last = next;
        } else {
            *last = now;
        }
    }

    /// Call an eapi endpoint and return the decoded body without judging its `code`.
    ///
    /// `path` is the endpoint without the `/eapi` prefix, e.g. `/song/enhance/player/url/v1`.
    pub async fn eapi_raw(&self, path: &str, params: Value) -> Result<Value> {
        self.ensure_guest_first(path).await;
        self.eapi_send(path, params).await
    }

    /// [`eapi_raw`](Self::eapi_raw) without the guest-token hook (which itself sends requests).
    pub(crate) async fn eapi_send(&self, path: &str, params: Value) -> Result<Value> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match self.eapi_once(path, &params).await {
                Err(e) if e.is_transient() && attempt < 3 => {
                    tracing::debug!("eapi {path} transient failure ({e}); retry {attempt}");
                    tokio::time::sleep(Duration::from_millis(400 * u64::from(attempt))).await;
                }
                other => return other,
            }
        }
    }

    /// Like [`eapi_raw`](Self::eapi_raw) but turns any `code != 200` into an error.
    pub async fn eapi(&self, path: &str, params: Value) -> Result<Value> {
        let body = self.eapi_raw(path, params).await?;
        check_code(&body)?;
        Ok(body)
    }

    async fn eapi_once(&self, path: &str, params: &Value) -> Result<Value> {
        self.throttle().await;

        let header = self.client_header();
        let header_obj: Map<String, Value> = header.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
        let mut body = match params {
            Value::Object(m) => m.clone(),
            Value::Null => Map::new(),
            other => return Err(Error::Other(format!("eapi params must be an object, got {other}"))),
        };
        body.insert("header".into(), Value::Object(header_obj));
        let json_text = serde_json::to_string(&Value::Object(body))?;
        let form = format!("params={}", eapi_params(&format!("/api{path}"), &json_text));

        let cookie = header.iter().map(|(k, v)| format!("{k}={}", cookie_escape(v))).collect::<Vec<_>>().join("; ");

        let resp = self
            .inner
            .http
            .post(format!("{EAPI_HOST}/eapi{path}"))
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(COOKIE, cookie)
            .body(form)
            .send()
            .await?;

        let set_cookies: Vec<String> =
            resp.headers().get_all(SET_COOKIE).iter().filter_map(|v| v.to_str().ok().map(str::to_owned)).collect();
        if !set_cookies.is_empty() {
            self.update_session(|s| s.absorb_set_cookie(set_cookies.iter().map(String::as_str)));
        }

        let status = resp.status();
        let bytes = resp.bytes().await?;
        if !status.is_success() && bytes.is_empty() {
            return Err(Error::api(i64::from(status.as_u16()), format!("HTTP {status}")));
        }
        decode_body(&bytes)
    }
}

/// The service answers either plain JSON or (when `e_r` is on) eapi-encrypted JSON.
fn decode_body(bytes: &[u8]) -> Result<Value> {
    let first = bytes.iter().find(|b| !b.is_ascii_whitespace()).copied();
    if matches!(first, Some(b'{') | Some(b'[')) {
        return Ok(serde_json::from_slice(bytes)?);
    }
    match eapi_decrypt(bytes).and_then(|plain| Ok(serde_json::from_slice(&plain)?)) {
        Ok(v) => Ok(v),
        Err(_) => {
            let snippet: String = String::from_utf8_lossy(&bytes[..bytes.len().min(160)]).into_owned();
            Err(Error::Other(format!("unrecognised response body: {snippet:?}")))
        }
    }
}

pub(crate) fn check_code(body: &Value) -> Result<()> {
    let code = body.get("code").and_then(Value::as_i64).unwrap_or(200);
    if code == 200 {
        return Ok(());
    }
    if code == 301 {
        return Err(Error::NeedLogin);
    }
    let message = ["message", "msg"].iter().find_map(|k| body.get(*k).and_then(Value::as_str)).unwrap_or_default();
    Err(Error::api(code, message))
}

/// Percent-escape the few characters that would break a `Cookie` header value.
fn cookie_escape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            ';' | ',' | ' ' | '"' | '\\' | '%' => out.push_str(&format!("%{:02X}", c as u32)),
            c if c.is_ascii_control() || !c.is_ascii() => {
                let mut buf = [0u8; 4];
                for b in c.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
            c => out.push(c),
        }
    }
    out
}

pub(crate) fn ids_json(ids: &[u64]) -> String {
    json!(ids).to_string()
}
