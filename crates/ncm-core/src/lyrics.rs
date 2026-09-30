//! Lyric handling: normalising what the service returns into standard LRC, merging a
//! translation or romanisation line by line, and producing the text to embed / save.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct LrcLine {
    pub time_ms: u64,
    pub text: String,
}

/// Which second line to merge below each original line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LyricsExtra {
    #[default]
    None,
    Translation,
    Romaji,
}

/// Which separate lyrics file to save next to the audio file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LyricsFile {
    Off,
    /// `<name>.lrc`, only for songs whose lyrics have a timeline.
    #[default]
    Lrc,
    /// `<name>.txt` with plain text, for every song that has lyrics.
    Txt,
    /// `<name>.lrc` when the lyrics have a timeline, `<name>.txt` when they do not.
    LrcOrTxt,
}

impl LyricsFile {
    pub const ALL: [LyricsFile; 4] = [LyricsFile::Off, LyricsFile::Lrc, LyricsFile::Txt, LyricsFile::LrcOrTxt];

    pub fn label(self) -> &'static str {
        match self {
            LyricsFile::Off => "不保存",
            LyricsFile::Lrc => "LRC（仅歌词带时间轴时）",
            LyricsFile::Txt => "TXT（纯文本）",
            LyricsFile::LrcOrTxt => "LRC，没有时间轴时改存 TXT",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedLyrics {
    /// Time-tagged lines in playback order.
    pub timed: Vec<LrcLine>,
    /// Lines without a time tag (plain-text lyrics or header notes).
    pub untimed: Vec<String>,
}

impl ParsedLyrics {
    pub fn has_timeline(&self) -> bool {
        !self.timed.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.timed.is_empty() && self.untimed.iter().all(|l| l.trim().is_empty())
    }
}

/// Parse LRC text. Besides classic `[mm:ss.xx]` lines this understands the JSON credit lines
/// (`{"t":1000,"c":[{"tx":"作曲: "},{"tx":"某人"}]}`) that the service puts at the top.
pub fn parse(raw: &str) -> ParsedLyrics {
    let mut out = ParsedLyrics::default();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('{') {
            if let Some(l) = parse_json_line(line) {
                out.timed.push(l);
            }
            continue;
        }
        let (times, text) = split_time_tags(line);
        if times.is_empty() {
            // `[ar:xx]`-style metadata is dropped; anything else is kept as plain text.
            if !is_meta_tag(line) {
                out.untimed.push(line.to_owned());
            }
        } else {
            for t in times {
                out.timed.push(LrcLine { time_ms: t, text: text.to_owned() });
            }
        }
    }
    out.timed.sort_by_key(|l| l.time_ms);
    out
}

fn parse_json_line(line: &str) -> Option<LrcLine> {
    let v: Value = serde_json::from_str(line).ok()?;
    let t = v.get("t")?.as_u64()?;
    let text: String = v.get("c")?.as_array()?.iter().filter_map(|seg| seg.get("tx").and_then(Value::as_str)).collect();
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(LrcLine { time_ms: t, text })
}

fn is_meta_tag(line: &str) -> bool {
    line.starts_with('[')
        && line.ends_with(']')
        && line[1..].split(':').next().is_some_and(|k| k.chars().all(|c| c.is_ascii_alphabetic()) && !k.is_empty())
}

/// Peel leading `[mm:ss.xx]` tags off a line; returns the times and the remaining text.
fn split_time_tags(line: &str) -> (Vec<u64>, &str) {
    let mut times = Vec::new();
    let mut rest = line;
    while let Some(stripped) = rest.strip_prefix('[') {
        let Some(end) = stripped.find(']') else { break };
        let Some(ms) = parse_timestamp(&stripped[..end]) else { break };
        times.push(ms);
        rest = stripped[end + 1..].trim_start_matches([' ', '\t']);
        // Keep leading spaces off, but a line like `[00:01.00][00:05.00]text` loops again.
        if !rest.starts_with('[') {
            break;
        }
    }
    (times, rest.trim())
}

fn parse_timestamp(tag: &str) -> Option<u64> {
    let (min, rest) = tag.split_once(':')?;
    let min: u64 = min.trim().parse().ok()?;
    let (sec, frac) = match rest.split_once(['.', ':']) {
        Some((s, f)) => (s, f),
        None => (rest, ""),
    };
    let sec: u64 = sec.trim().parse().ok()?;
    if sec >= 100 {
        return None;
    }
    let frac_ms = if frac.is_empty() {
        0
    } else {
        let digits: String = frac.chars().take(3).collect();
        if !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let v: u64 = digits.parse().ok()?;
        match digits.len() {
            1 => v * 100,
            2 => v * 10,
            _ => v,
        }
    };
    Some(min * 60_000 + sec * 1000 + frac_ms)
}

pub fn format_timestamp(ms: u64) -> String {
    let cs = ms / 10;
    format!("[{:02}:{:02}.{:02}]", cs / 6000, (cs / 100) % 60, cs % 100)
}

/// Render lines as classic LRC (centisecond precision).
pub fn render_lrc(lines: &[LrcLine]) -> String {
    let mut s = String::new();
    for l in lines {
        s.push_str(&format_timestamp(l.time_ms));
        s.push_str(&l.text);
        s.push('\n');
    }
    s
}

/// Insert `extra` (translation / romanisation) after the original line with the same time.
pub fn merge(original: &[LrcLine], extra: &[LrcLine]) -> Vec<LrcLine> {
    let mut by_time: HashMap<u64, &LrcLine> = HashMap::new();
    for l in extra.iter().filter(|l| !l.text.trim().is_empty()) {
        by_time.entry(l.time_ms).or_insert(l);
    }
    let mut used: Vec<u64> = Vec::new();
    let mut out = Vec::with_capacity(original.len() * 2);
    for l in original {
        out.push(l.clone());
        let hit = by_time.get(&l.time_ms).copied().or_else(|| {
            // Tolerate small timestamp differences between the two tracks.
            extra
                .iter()
                .filter(|e| !used.contains(&e.time_ms) && !e.text.trim().is_empty())
                .find(|e| e.time_ms.abs_diff(l.time_ms) <= 300)
        });
        if let Some(e) = hit
            && e.text.trim() != l.text.trim()
        {
            used.push(e.time_ms);
            out.push(LrcLine { time_ms: l.time_ms, text: e.text.clone() });
        }
    }
    out
}

/// What the pipeline needs from the lyrics of one song.
#[derive(Debug, Clone, Default)]
pub struct PreparedLyrics {
    /// Time-tagged LRC (when the song has a timeline).
    pub lrc: Option<String>,
    /// Plain text without timestamps.
    pub plain: String,
}

impl PreparedLyrics {
    /// The extension and contents of the separate lyrics file to write for `mode`, if any.
    pub fn sidecar(&self, mode: LyricsFile) -> Option<(&'static str, String)> {
        let lrc = || self.lrc.clone().map(|text| ("lrc", text));
        let txt = || Some(("txt", format!("{}\n", self.plain.trim_end())));
        match mode {
            LyricsFile::Off => None,
            LyricsFile::Lrc => lrc(),
            LyricsFile::Txt => txt(),
            LyricsFile::LrcOrTxt => lrc().or_else(txt),
        }
        .filter(|(_, text)| !text.trim().is_empty())
    }
}

pub fn prepare(lrc: Option<&str>, extra_raw: Option<&str>, extra: LyricsExtra) -> Option<PreparedLyrics> {
    let original = parse(lrc?);
    if original.is_empty() {
        return None;
    }
    if !original.has_timeline() {
        let plain = original.untimed.join("\n");
        return Some(PreparedLyrics { lrc: None, plain });
    }
    let merged = match (extra, extra_raw) {
        (LyricsExtra::None, _) | (_, None) => original.timed.clone(),
        (_, Some(raw)) => {
            let e = parse(raw);
            if e.has_timeline() { merge(&original.timed, &e.timed) } else { original.timed.clone() }
        }
    };
    let plain = merged.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n");
    Some(PreparedLyrics { lrc: Some(render_lrc(&merged)), plain })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timestamps_of_every_precision() {
        assert_eq!(parse_timestamp("00:01"), Some(1000));
        assert_eq!(parse_timestamp("00:01.5"), Some(1500));
        assert_eq!(parse_timestamp("00:01.50"), Some(1500));
        assert_eq!(parse_timestamp("01:02.345"), Some(62_345));
        assert_eq!(parse_timestamp("ar:Someone"), None);
    }

    #[test]
    fn converts_json_credit_lines_and_keeps_lrc() {
        let raw = r#"{"t":0,"c":[{"tx":"作词: "},{"tx":"黄家驹","li":"http://x"}]}
{"t":2000,"c":[{"tx":"编曲: "},{"tx":"Beyond"},{"tx":"/"},{"tx":"梁邦彦"}]}
[ar:Beyond]
[00:16.240]今天我 寒夜里看雪飘过
[00:20.5]怀著冷却了的心窝漂远方
"#;
        let p = parse(raw);
        assert_eq!(p.timed.len(), 4);
        assert_eq!(p.timed[0], LrcLine { time_ms: 0, text: "作词: 黄家驹".into() });
        assert_eq!(p.timed[1].text, "编曲: Beyond/梁邦彦");
        assert_eq!(p.timed[3].time_ms, 20_500);
        assert!(p.untimed.is_empty());
        let out = render_lrc(&p.timed);
        assert!(out.starts_with("[00:00.00]作词: 黄家驹\n"), "{out}");
        assert!(out.contains("[00:16.24]今天我"));
    }

    #[test]
    fn multiple_time_tags_per_line() {
        let p = parse("[00:10.00][01:10.00]chorus");
        assert_eq!(p.timed.len(), 2);
        assert_eq!(p.timed[1].time_ms, 70_000);
        assert_eq!(p.timed[1].text, "chorus");
    }

    #[test]
    fn merges_translation_below_original() {
        let orig = parse("[00:01.00]hello\n[00:05.00]world");
        let tr = parse("[00:01.00]你好\n[00:05.10]世界");
        let m = merge(&orig.timed, &tr.timed);
        let texts: Vec<_> = m.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["hello", "你好", "world", "世界"]);
    }

    #[test]
    fn plain_lyrics_have_no_timeline() {
        let p = prepare(Some("first line\nsecond line"), None, LyricsExtra::None).unwrap();
        assert!(p.lrc.is_none());
        assert_eq!(p.plain, "first line\nsecond line");
    }

    #[test]
    fn sidecar_follows_the_chosen_mode() {
        let timed = prepare(Some("[00:01.00]hello\n[00:05.00]world"), None, LyricsExtra::None).unwrap();
        let plain = prepare(Some("first line\nsecond line"), None, LyricsExtra::None).unwrap();

        assert_eq!(timed.sidecar(LyricsFile::Off), None);
        assert_eq!(timed.sidecar(LyricsFile::Lrc), Some(("lrc", "[00:01.00]hello\n[00:05.00]world\n".to_owned())));
        assert_eq!(timed.sidecar(LyricsFile::Txt), Some(("txt", "hello\nworld\n".to_owned())));
        assert_eq!(timed.sidecar(LyricsFile::LrcOrTxt).map(|s| s.0), Some("lrc"));

        // Without a timeline there is no LRC: only TXT, or the fallback, produces a file.
        assert_eq!(plain.sidecar(LyricsFile::Lrc), None);
        assert_eq!(plain.sidecar(LyricsFile::Txt), Some(("txt", "first line\nsecond line\n".to_owned())));
        assert_eq!(plain.sidecar(LyricsFile::LrcOrTxt), Some(("txt", "first line\nsecond line\n".to_owned())));
    }

    #[test]
    fn txt_sidecar_includes_the_translation_when_one_is_merged() {
        let p = prepare(Some("[00:01.00]hello"), Some("[00:01.00]你好"), LyricsExtra::Translation).unwrap();
        assert_eq!(p.sidecar(LyricsFile::Txt), Some(("txt", "hello\n你好\n".to_owned())));
    }

    #[test]
    fn prepare_bilingual() {
        let p = prepare(Some("[00:01.00]hello"), Some("[00:01.00]你好"), LyricsExtra::Translation).unwrap();
        assert_eq!(p.lrc.unwrap(), "[00:01.00]hello\n[00:01.00]你好\n");
        assert_eq!(p.plain, "hello\n你好");
    }

    #[test]
    fn empty_input_yields_none() {
        assert!(prepare(Some("  \n"), None, LyricsExtra::None).is_none());
        assert!(prepare(None, None, LyricsExtra::None).is_none());
    }
}
