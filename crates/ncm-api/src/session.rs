use std::collections::BTreeMap;

use rand::RngExt;
use serde::{Deserialize, Serialize};

/// Cookies that carry (or refresh) the login state. Everything else the server sets is noise.
const KEPT_COOKIES: &[&str] =
    &["MUSIC_U", "MUSIC_A", "MUSIC_A_T", "MUSIC_R_T", "__csrf", "NMTID", "__remember_me", "WEVNSM", "WNMCID"];

/// Identity of the (emulated) desktop client. Stable per installation.
///
/// The values are chosen to look like one consistent Windows machine over time: the device id is
/// derived from this computer's identifier (a reinstall does not look like a brand-new device),
/// the OS string reflects the real Windows build, the resolution is a common one picked once,
/// and the build timestamp is fixed like a real build number instead of changing on every request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub device_id: String,
    pub os: String,
    pub osver: String,
    pub appver: String,
    pub versioncode: String,
    pub channel: String,
    pub resolution: String,
    #[serde(default = "fresh_buildver")]
    pub buildver: String,
}

const DEFAULT_OSVER: &str = "Microsoft-Windows-10-Professional-build-19045-64bit";

impl Device {
    /// The identity of this computer.
    pub fn generate() -> Self {
        let device_id = stable_device_id().unwrap_or_else(random_device_id);
        Self { osver: probe_osver(), resolution: pick_resolution(&device_id), device_id, ..Self::default() }
    }
}

impl Default for Device {
    fn default() -> Self {
        Self {
            device_id: String::new(),
            os: "pc".into(),
            osver: DEFAULT_OSVER.into(),
            // Matches the build string embedded in the official 3.1.23 desktop client.
            appver: "3.1.23.204814".into(),
            versioncode: "140".into(),
            channel: "netease".into(),
            resolution: "1920x1080".into(),
            buildver: fresh_buildver(),
        }
    }
}

/// A build timestamp (Unix seconds) somewhere in the last few weeks, fixed once per device.
fn fresh_buildver() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(1_780_000_000);
    (now - rand::rng().random_range(20..75u64) * 86_400).to_string()
}

fn random_device_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill(&mut bytes);
    hex::encode_upper(bytes)
}

/// 32 upper-case hex characters derived (salted, so the raw identifier is not disclosed) from the
/// operating system's machine id.
fn stable_device_id() -> Option<String> {
    use md5::{Digest, Md5};
    let id = machine_uid::get().ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| hex::encode_upper(Md5::digest(format!("ncm-dl/device/{id}").as_bytes())))
}

/// One of the resolutions most desktops have, chosen deterministically from the device id.
fn pick_resolution(seed: &str) -> String {
    use md5::{Digest, Md5};
    const COMMON: [&str; 10] = [
        "1920x1080",
        "1920x1080",
        "1920x1080",
        "1920x1080",
        "1920x1080",
        "2560x1440",
        "2560x1440",
        "1366x768",
        "1536x864",
        "3840x2160",
    ];
    let pick = Md5::digest(seed.as_bytes())[0] as usize;
    COMMON[pick % COMMON.len()].to_owned()
}

/// `Microsoft-Windows-10-<edition>-build-<build>-64bit`, from the real Windows version when running
/// on Windows (other systems present a common Windows 10 build).
#[cfg(windows)]
fn probe_osver() -> String {
    let Ok(key) = windows_registry::LOCAL_MACHINE.open(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") else {
        return DEFAULT_OSVER.into();
    };
    let build = key
        .get_string("CurrentBuildNumber")
        .or_else(|_| key.get_string("CurrentBuild"))
        .ok()
        .filter(|b| !b.is_empty() && b.chars().all(|c| c.is_ascii_digit()));
    let edition = match key.get_string("EditionID").unwrap_or_default().as_str() {
        "Core" | "CoreN" | "CoreSingleLanguage" | "CoreCountrySpecific" => "Home",
        "Enterprise" | "EnterpriseS" | "EnterpriseN" => "Enterprise",
        "Education" | "EducationN" => "Education",
        _ => "Professional",
    };
    match build {
        Some(b) => format!("Microsoft-Windows-10-{edition}-build-{b}-64bit"),
        None => DEFAULT_OSVER.into(),
    }
}

#[cfg(not(windows))]
fn probe_osver() -> String {
    DEFAULT_OSVER.into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Account {
    pub user_id: u64,
    pub nickname: String,
    pub avatar_url: Option<String>,
    /// 0 = none, 10 = VIP, 11 = SVIP (as reported by the account API).
    pub vip_type: i64,
}

/// Everything that identifies this client to the service; persisted (encrypted) between runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub device: Device,
    pub cookies: BTreeMap<String, String>,
    pub account: Option<Account>,
}

impl Session {
    pub fn new() -> Self {
        Self { device: Device::generate(), cookies: BTreeMap::new(), account: None }
    }

    pub fn is_logged_in(&self) -> bool {
        self.cookies.get("MUSIC_U").is_some_and(|v| !v.is_empty())
    }

    pub fn music_u(&self) -> Option<&str> {
        self.cookies.get("MUSIC_U").map(String::as_str).filter(|v| !v.is_empty())
    }

    /// Drop the login (keeps the device identity so the next login looks like the same client).
    pub fn sign_out(&mut self) {
        self.cookies.clear();
        self.account = None;
    }

    /// Adopt a cookie pasted by the user, e.g. `MUSIC_U=xxxx` or just the raw token.
    pub fn import_cookie_text(&mut self, text: &str) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return false;
        }
        if !text.contains('=') {
            self.cookies.insert("MUSIC_U".into(), text.to_owned());
            return true;
        }
        let mut found = false;
        for part in text.split(';') {
            if let Some((k, v)) = part.trim().split_once('=') {
                let (k, v) = (k.trim(), v.trim());
                if KEPT_COOKIES.contains(&k) && !v.is_empty() {
                    self.cookies.insert(k.to_owned(), v.to_owned());
                    found |= k == "MUSIC_U";
                }
            }
        }
        found
    }

    /// Merge `Set-Cookie` header values from a response.
    pub fn absorb_set_cookie<'a>(&mut self, headers: impl IntoIterator<Item = &'a str>) {
        for raw in headers {
            let mut attrs = raw.split(';');
            let Some((name, value)) = attrs.next().and_then(|kv| kv.split_once('=')) else { continue };
            let name = name.trim();
            if !KEPT_COOKIES.contains(&name) {
                continue;
            }
            let expired = attrs.any(|a| {
                let a = a.trim().to_ascii_lowercase();
                a == "max-age=0" || a.starts_with("max-age=-")
            });
            let value = value.trim();
            if expired || value.is_empty() {
                self.cookies.remove(name);
            } else {
                self.cookies.insert(name.to_owned(), value.to_owned());
            }
        }
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_identity_is_stable_and_plausible() {
        let a = Device::generate();
        let b = Device::generate();
        assert_eq!(a.device_id, b.device_id, "same computer, same device id");
        assert_eq!(a.device_id.len(), 32);
        assert!(a.device_id.chars().all(|c| c.is_ascii_digit() || ('A'..='F').contains(&c)));
        assert_eq!(a.resolution, b.resolution);
        assert!(a.osver.starts_with("Microsoft-Windows-10-") && a.osver.ends_with("-64bit"), "{}", a.osver);
        let build: u64 = a.buildver.parse().expect("unix seconds");
        assert!((1_500_000_000..4_000_000_000).contains(&build));
    }

    #[test]
    fn old_saved_devices_without_buildver_still_load() {
        let json = r#"{"device_id":"AB","os":"pc","osver":"x","appver":"3.1.23.204814","versioncode":"140","channel":"netease","resolution":"1920x1080"}"#;
        let d: Device = serde_json::from_str(json).unwrap();
        assert_eq!(d.device_id, "AB");
        assert!(d.buildver.parse::<u64>().is_ok());
    }
}
