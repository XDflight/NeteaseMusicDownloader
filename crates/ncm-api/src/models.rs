use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------- audio quality

/// Audio quality tiers offered by the service, lowest to highest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Standard,
    Higher,
    Exhigh,
    Lossless,
    Hires,
    Jyeffect,
    Sky,
    Jymaster,
}

impl Level {
    pub const ALL: [Level; 8] = [
        Level::Standard,
        Level::Higher,
        Level::Exhigh,
        Level::Lossless,
        Level::Hires,
        Level::Jyeffect,
        Level::Sky,
        Level::Jymaster,
    ];

    /// The value of the `level` request parameter.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Standard => "standard",
            Level::Higher => "higher",
            Level::Exhigh => "exhigh",
            Level::Lossless => "lossless",
            Level::Hires => "hires",
            Level::Jyeffect => "jyeffect",
            Level::Sky => "sky",
            Level::Jymaster => "jymaster",
        }
    }

    pub fn parse(s: &str) -> Option<Level> {
        Level::ALL.into_iter().find(|l| l.as_str().eq_ignore_ascii_case(s.trim()))
    }

    pub fn label(self) -> &'static str {
        match self {
            Level::Standard => "标准",
            Level::Higher => "较高",
            Level::Exhigh => "极高",
            Level::Lossless => "无损",
            Level::Hires => "Hi-Res",
            Level::Jyeffect => "高清环绕声",
            Level::Sky => "沉浸环绕声",
            Level::Jymaster => "超清母带",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Level::Standard => "MP3 · 128 kbps",
            Level::Higher => "MP3 · 192 kbps",
            Level::Exhigh => "MP3 · 320 kbps",
            Level::Lossless => "FLAC · 16 bit / 44.1–48 kHz",
            Level::Hires => "FLAC · 24 bit / 96 kHz 及以上",
            Level::Jyeffect => "空间音频 · 需 VIP",
            Level::Sky => "沉浸声 · 需 SVIP",
            Level::Jymaster => "母带音质 · 需 SVIP",
        }
    }

    /// The next lower tier to try when this one is unavailable for a track.
    pub fn downgrade(self) -> Option<Level> {
        match self {
            Level::Jymaster => Some(Level::Sky),
            Level::Sky => Some(Level::Jyeffect),
            Level::Jyeffect => Some(Level::Hires),
            Level::Hires => Some(Level::Lossless),
            Level::Lossless => Some(Level::Exhigh),
            Level::Exhigh => Some(Level::Higher),
            Level::Higher => Some(Level::Standard),
            Level::Standard => None,
        }
    }
}

// ---------------------------------------------------------------- catalog

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Artist {
    pub id: u64,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AlbumRef {
    pub id: u64,
    pub name: String,
    pub pic_url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Privilege {
    pub fee: i32,
    pub payed: i32,
    /// Negative when the current account cannot use the song (`-100` for VIP tracks seen
    /// anonymously, other negatives for takedowns).
    pub st: i32,
    /// Highest bit-rate the current account may stream (0 = not playable).
    pub pl: i64,
    /// Highest bit-rate the current account may download (0 = not downloadable).
    pub dl: i64,
    pub max_br: i64,
    /// `freeTrialPrivilege.cannotListenReason`; `1` means "VIP required".
    pub cannot_listen_reason: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Availability {
    Ok,
    VipRequired,
    PurchaseRequired,
    Unavailable,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: u64,
    pub name: String,
    pub aliases: Vec<String>,
    pub translated_names: Vec<String>,
    pub artists: Vec<Artist>,
    pub album: AlbumRef,
    pub duration_ms: u64,
    pub track_no: u32,
    pub disc: String,
    pub publish_time_ms: Option<i64>,
    pub fee: i32,
    pub privilege: Privilege,
}

impl Track {
    pub fn artist_names(&self, sep: &str) -> String {
        self.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(sep)
    }

    /// `fee == 1` marks songs that need a VIP subscription.
    pub fn is_vip_only(&self) -> bool {
        self.fee == 1 || self.privilege.fee == 1
    }

    /// Best-effort verdict from catalog data; the authoritative answer is the URL lookup.
    pub fn availability(&self) -> Availability {
        let p = &self.privilege;
        if p.pl > 0 || p.dl > 0 {
            return Availability::Ok;
        }
        let fee = p.fee.max(self.fee);
        if fee == 1 || p.cannot_listen_reason == Some(1) {
            Availability::VipRequired
        } else if fee == 4 {
            Availability::PurchaseRequired
        } else if p.st < 0 {
            Availability::Unavailable
        } else {
            Availability::Ok
        }
    }

    pub fn year(&self) -> Option<i32> {
        self.publish_time_ms.filter(|t| *t > 0).map(year_from_unix_ms)
    }

    /// Parse one entry of a `songs` / `tracks` array (plus its privilege, when given).
    pub fn from_json(song: &Value, privilege: Option<&Value>) -> Option<Track> {
        let id = song.get("id")?.as_u64()?;
        let artists = first_array(song, &["ar", "artists"]).map(|a| a.iter().map(parse_artist).collect()).unwrap_or_default();
        let al = song.get("al").or_else(|| song.get("album"));
        let album = al
            .map(|al| AlbumRef {
                id: al.get("id").and_then(Value::as_u64).unwrap_or(0),
                name: str_of(al, "name"),
                pic_url: al.get("picUrl").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned),
            })
            .unwrap_or_default();
        let priv_json = privilege.or_else(|| song.get("privilege"));
        Some(Track {
            id,
            name: str_of(song, "name"),
            aliases: string_list(song, &["alia", "alias"]),
            translated_names: string_list(song, &["tns", "transNames"]),
            artists,
            album,
            duration_ms: song.get("dt").or_else(|| song.get("duration")).and_then(Value::as_u64).unwrap_or(0),
            track_no: song.get("no").and_then(Value::as_u64).unwrap_or(0) as u32,
            disc: str_of(song, "cd"),
            publish_time_ms: song.get("publishTime").and_then(Value::as_i64),
            fee: song.get("fee").and_then(Value::as_i64).unwrap_or(0) as i32,
            privilege: priv_json.map(parse_privilege).unwrap_or_default(),
        })
    }
}

fn parse_artist(a: &Value) -> Artist {
    Artist { id: a.get("id").and_then(Value::as_u64).unwrap_or(0), name: str_of(a, "name") }
}

fn parse_privilege(p: &Value) -> Privilege {
    let i = |k: &str| p.get(k).and_then(Value::as_i64).unwrap_or(0);
    Privilege {
        fee: i("fee") as i32,
        payed: i("payed") as i32,
        st: i("st") as i32,
        pl: i("pl"),
        dl: i("dl"),
        max_br: i("maxbr"),
        cannot_listen_reason: p.get("freeTrialPrivilege").and_then(|f| f.get("cannotListenReason")).and_then(Value::as_i64),
    }
}

fn first_array<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Vec<Value>> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_array))
}

fn string_list(v: &Value, keys: &[&str]) -> Vec<String> {
    first_array(v, keys).map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect()).unwrap_or_default()
}

pub(crate) fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// Calendar year of a Unix timestamp in milliseconds (proleptic Gregorian, UTC).
fn year_from_unix_ms(ms: i64) -> i32 {
    let days = ms.div_euclid(86_400_000);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }) as i32
}

// ---------------------------------------------------------------- collections

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlaylistSummary {
    pub id: u64,
    pub name: String,
    pub cover_url: Option<String>,
    pub track_count: u32,
    pub creator_id: u64,
    pub creator_name: String,
    pub description: String,
    /// `true` when the playlist belongs to someone else and was only bookmarked.
    pub subscribed: bool,
    /// 0 = public, 10 = private.
    pub privacy: i32,
    /// 5 = the user's "liked songs" playlist.
    pub special_type: i32,
}

impl PlaylistSummary {
    pub fn is_private(&self) -> bool {
        self.privacy == 10
    }

    pub fn from_json(p: &Value) -> Option<Self> {
        let creator = p.get("creator");
        Some(Self {
            id: p.get("id")?.as_u64()?,
            name: str_of(p, "name"),
            cover_url: p.get("coverImgUrl").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned),
            track_count: p.get("trackCount").and_then(Value::as_u64).unwrap_or(0) as u32,
            creator_id: creator.and_then(|c| c.get("userId")).and_then(Value::as_u64).unwrap_or(0),
            creator_name: creator.map(|c| str_of(c, "nickname")).unwrap_or_default(),
            description: str_of(p, "description"),
            subscribed: p.get("subscribed").and_then(Value::as_bool).unwrap_or(false),
            privacy: p.get("privacy").and_then(Value::as_i64).unwrap_or(0) as i32,
            special_type: p.get("specialType").and_then(Value::as_i64).unwrap_or(0) as i32,
        })
    }
}

/// A resolved list of tracks (playlist, album or single song) ready for download.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Collection {
    pub kind: CollectionKind,
    pub id: u64,
    pub name: String,
    pub cover_url: Option<String>,
    pub creator: String,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CollectionKind {
    #[default]
    Playlist,
    Album,
    Song,
}

// ---------------------------------------------------------------- audio / lyrics

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SongUrl {
    pub id: u64,
    pub url: Option<String>,
    pub code: i64,
    pub br: u32,
    pub size: u64,
    pub md5: Option<String>,
    /// File type reported by the server: `mp3`, `flac`, `m4a`, ...
    pub kind: Option<String>,
    /// The quality level actually served (may be lower than requested).
    pub level: Option<String>,
    pub fee: i32,
    /// `true` if this is only a preview clip of a VIP track.
    pub is_trial: bool,
}

impl SongUrl {
    pub fn from_json(d: &Value) -> Option<Self> {
        let opt_str = |k: &str| d.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned);
        Some(Self {
            id: d.get("id")?.as_u64()?,
            url: opt_str("url"),
            code: d.get("code").and_then(Value::as_i64).unwrap_or(0),
            br: d.get("br").and_then(Value::as_u64).unwrap_or(0) as u32,
            size: d.get("size").and_then(Value::as_u64).unwrap_or(0),
            md5: opt_str("md5"),
            kind: opt_str("type").map(|t| t.to_ascii_lowercase()),
            level: opt_str("level"),
            fee: d.get("fee").and_then(Value::as_i64).unwrap_or(0) as i32,
            is_trial: d.get("freeTrialInfo").is_some_and(|v| !v.is_null())
                || d.get("freeTrialPrivilege").and_then(|p| p.get("cannotListenReason")).is_some_and(|v| !v.is_null()),
        })
    }

    pub fn is_usable(&self) -> bool {
        self.url.is_some() && !self.is_trial
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Lyrics {
    /// Original lyrics in LRC format (empty for instrumentals).
    pub lrc: Option<String>,
    /// Translated lyrics (LRC).
    pub translation: Option<String>,
    /// Romanised lyrics (LRC).
    pub romaji: Option<String>,
    /// Word-by-word ("yrc") lyrics, if the song has them.
    pub word_by_word: Option<String>,
    pub instrumental: bool,
}

impl Lyrics {
    pub fn from_json(v: &Value) -> Self {
        let text = |k: &str| {
            v.get(k)
                .and_then(|o| o.get("lyric"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        Self {
            lrc: text("lrc"),
            translation: text("tlyric"),
            romaji: text("romalrc"),
            word_by_word: text("yrc"),
            instrumental: v.get("pureMusic").and_then(Value::as_bool).unwrap_or(false),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lrc.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn year_conversion() {
        assert_eq!(year_from_unix_ms(0), 1970);
        assert_eq!(year_from_unix_ms(1_577_836_800_000), 2020); // 2020-01-01
        assert_eq!(year_from_unix_ms(1_577_836_799_999), 2019);
        assert_eq!(year_from_unix_ms(-86_400_000), 1969);
    }

    #[test]
    fn level_roundtrip() {
        for l in Level::ALL {
            assert_eq!(Level::parse(l.as_str()), Some(l));
        }
        assert_eq!(Level::Jymaster.downgrade(), Some(Level::Sky));
        assert_eq!(Level::Standard.downgrade(), None);
    }

    #[test]
    fn parses_song() {
        let song = serde_json::json!({
            "id": 347230, "name": "海阔天空", "dt": 326000, "no": 3, "cd": "1", "fee": 8,
            "publishTime": 1_577_836_800_000i64,
            "ar": [{"id": 11127, "name": "Beyond"}],
            "al": {"id": 34209, "name": "海阔天空", "picUrl": "http://p1.music.126.net/x.jpg"},
            "alia": [], "tns": []
        });
        let p = serde_json::json!({"fee": 8, "st": 0, "pl": 320000, "dl": 320000, "maxbr": 999000});
        let t = Track::from_json(&song, Some(&p)).unwrap();
        assert_eq!(t.artist_names("/"), "Beyond");
        assert_eq!(t.year(), Some(2020));
        assert_eq!(t.availability(), Availability::Ok);
    }

    #[test]
    fn vip_track_seen_anonymously_is_vip_required() {
        // Shape observed from the live service for a VIP song without a VIP session.
        let song = serde_json::json!({"id": 347230, "name": "x", "fee": 1, "ar": [], "al": {}});
        let p = serde_json::json!({
            "fee": 0, "st": -100, "pl": 0, "dl": 0, "maxbr": 999000,
            "freeTrialPrivilege": {"cannotListenReason": 1, "resConsumable": true}
        });
        let t = Track::from_json(&song, Some(&p)).unwrap();
        assert_eq!(t.availability(), Availability::VipRequired);
        assert!(t.is_vip_only());
    }
}
