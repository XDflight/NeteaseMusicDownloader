use serde_json::{Value, json};

use crate::client::{Client, check_code};
use crate::crypto::md5_hex;
use crate::error::{Error, Result};
use crate::models::str_of;
use crate::session::Account;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QrState {
    /// 801 — nobody has scanned the code yet.
    Waiting,
    /// 802 — scanned; waiting for the user to confirm on the phone.
    Scanned {
        nickname: String,
        avatar_url: Option<String>,
    },
    /// 803 — confirmed; the session now holds the login cookies.
    Confirmed,
    /// 800 — the code timed out; request a new one.
    Expired,
    Other {
        code: i64,
        message: String,
    },
}

pub enum LoginSecret<'a> {
    Password(&'a str),
    Captcha(&'a str),
}

impl Client {
    // -- QR code -------------------------------------------------------------------------

    /// Ask the server for a fresh QR-login key.
    pub async fn qr_login_key(&self) -> Result<String> {
        let body = self.eapi("/login/qrcode/unikey", json!({ "type": 1 })).await?;
        body.get("unikey")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::Other("server did not return a QR key".into()))
    }

    /// The text to encode into the QR image; the mobile app understands this deep link.
    pub fn qr_login_url(key: &str) -> String {
        format!("https://music.163.com/login?codekey={key}")
    }

    /// Poll the state of a QR login. On [`QrState::Confirmed`] the session cookies are stored.
    pub async fn qr_login_check(&self, key: &str) -> Result<QrState> {
        let body = self.eapi_raw("/login/qrcode/client/login", json!({ "key": key, "type": 1 })).await?;
        let code = body.get("code").and_then(Value::as_i64).unwrap_or(-1);
        Ok(match code {
            800 => QrState::Expired,
            801 => QrState::Waiting,
            802 => QrState::Scanned {
                nickname: str_of(&body, "nickname"),
                avatar_url: body.get("avatarUrl").and_then(Value::as_str).map(str::to_owned),
            },
            803 => {
                // Cookies normally arrive via Set-Cookie; the body repeats them as a fallback.
                if let Some(cookie) = body.get("cookie").and_then(Value::as_str) {
                    self.update_session(|s| {
                        s.import_cookie_text(cookie);
                    });
                }
                QrState::Confirmed
            }
            other => QrState::Other { code: other, message: str_of(&body, "message") },
        })
    }

    // -- phone ---------------------------------------------------------------------------

    /// Request an SMS verification code.
    pub async fn send_sms_captcha(&self, phone: &str, country_code: &str) -> Result<()> {
        self.eapi("/sms/captcha/sent", json!({ "cellphone": phone, "ctcode": country_code })).await?;
        Ok(())
    }

    pub async fn login_cellphone(&self, phone: &str, country_code: &str, secret: LoginSecret<'_>) -> Result<Account> {
        let mut params = json!({
            "phone": phone,
            "countrycode": country_code,
            "rememberLogin": "true",
        });
        match secret {
            LoginSecret::Password(pw) => params["password"] = Value::String(md5_hex(pw)),
            LoginSecret::Captcha(code) => params["captcha"] = Value::String(code.to_owned()),
        }
        let body = self.eapi_raw("/w/login/cellphone", params).await?;
        let code = body.get("code").and_then(Value::as_i64).unwrap_or(-1);
        if code != 200 {
            return Err(match code {
                8821 => Error::api(code, "需要通过行为验证码；请改用扫码登录"),
                502 => Error::api(code, "账号或密码错误"),
                _ => {
                    let msg = body.get("message").or_else(|| body.get("msg")).and_then(Value::as_str).unwrap_or("登录失败");
                    Error::api(code, msg)
                }
            });
        }
        self.refresh_account().await
    }

    // -- account -------------------------------------------------------------------------

    /// Ask the server who we are. Errors with [`Error::NeedLogin`] when the cookies are not valid.
    pub async fn refresh_account(&self) -> Result<Account> {
        let body = self.eapi("/w/nuser/account/get", json!({})).await?;
        let account = parse_account(&body).ok_or(Error::NeedLogin)?;
        self.update_session(|s| s.account = Some(account.clone()));
        Ok(account)
    }

    pub async fn logout(&self) -> Result<()> {
        // Best effort: even if the server call fails the local session is dropped.
        let remote = self.eapi_raw("/logout", json!({})).await;
        self.sign_out_local();
        remote.and_then(|b| check_code(&b))
    }

    /// Register an anonymous guest identity (`MUSIC_A`), as the desktop client does on its first
    /// start, when the session has neither a login nor a guest token. Returns whether a token was
    /// obtained.
    pub async fn ensure_guest_token(&self) -> Result<bool> {
        let (missing, device_id) = self.update_session(|s| {
            let has_guest = s.cookies.get("MUSIC_A").is_some_and(|v| !v.is_empty());
            (s.music_u().is_none() && !has_guest, s.device.device_id.clone())
        });
        if !missing {
            return Ok(false);
        }
        let body =
            self.eapi_send("/register/anonimous", json!({ "username": crate::crypto::anonymous_username(&device_id) })).await?;
        check_code(&body)?;
        Ok(true)
    }
}

fn parse_account(body: &Value) -> Option<Account> {
    let profile = body.get("profile").filter(|p| !p.is_null())?;
    let account = body.get("account");
    Some(Account {
        user_id: profile.get("userId").and_then(Value::as_u64)?,
        nickname: str_of(profile, "nickname"),
        avatar_url: profile.get("avatarUrl").and_then(Value::as_str).map(str::to_owned),
        vip_type: account.and_then(|a| a.get("vipType")).or_else(|| profile.get("vipType")).and_then(Value::as_i64).unwrap_or(0),
    })
}
