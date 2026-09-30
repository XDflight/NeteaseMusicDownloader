//! Recognise the various ways users refer to a playlist / album / song:
//! share links from the desktop client, mobile web links, short links, or bare ids.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    Playlist,
    Album,
    Song,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resource {
    pub kind: ResourceKind,
    pub id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedInput {
    Resource(Resource),
    /// A `163cn.tv` share link that must be resolved by following redirects.
    ShortLink(String),
    /// Only digits were given; the caller has to pick the kind.
    BareId(u64),
    Unrecognized,
}

pub fn parse_input(text: &str) -> ParsedInput {
    let text = text.trim();
    if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        return text.parse().map(ParsedInput::BareId).unwrap_or(ParsedInput::Unrecognized);
    }
    let Some(url) = extract_url(text) else { return ParsedInput::Unrecognized };
    let Ok(parsed) = url::Url::parse(&url) else { return ParsedInput::Unrecognized };
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();

    if host == "163cn.tv" || host.ends_with(".163cn.tv") {
        return ParsedInput::ShortLink(url);
    }
    if !(host == "music.163.com" || host.ends_with(".music.163.com")) {
        return ParsedInput::Unrecognized;
    }

    // Web links use hash routing: https://music.163.com/#/playlist?id=123
    let (path, query) = match parsed.fragment() {
        Some(frag) if frag.starts_with('/') => {
            let (p, q) = frag.split_once('?').unwrap_or((frag, ""));
            (p.to_owned(), q.to_owned())
        }
        _ => (parsed.path().to_owned(), parsed.query().unwrap_or_default().to_owned()),
    };

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let kind = segments.iter().rev().find_map(|s| match *s {
        "playlist" | "toplist" => Some(ResourceKind::Playlist),
        "album" => Some(ResourceKind::Album),
        "song" => Some(ResourceKind::Song),
        _ => None,
    });
    let Some(kind) = kind else { return ParsedInput::Unrecognized };

    let id = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == "id")
        .and_then(|(_, v)| v.parse::<u64>().ok())
        .or_else(|| segments.last().and_then(|s| s.parse::<u64>().ok()));
    match id {
        Some(id) => ParsedInput::Resource(Resource { kind, id }),
        None => ParsedInput::Unrecognized,
    }
}

/// Pull the first URL out of free text such as
/// `分享歌单: 我喜欢的音乐 https://music.163.com/playlist?id=1&userid=2 (来自@网易云音乐)`.
fn extract_url(text: &str) -> Option<String> {
    let start = text.find("http://").or_else(|| text.find("https://"))?;
    let url: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii() && !c.is_ascii_whitespace() && !matches!(c, '"' | '\'' | '<' | '>' | ')' | ']'))
        .collect();
    Some(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(kind: ResourceKind, id: u64) -> ParsedInput {
        ParsedInput::Resource(Resource { kind, id })
    }

    #[test]
    fn web_and_mobile_links() {
        assert_eq!(parse_input("https://music.163.com/#/playlist?id=3778678"), res(ResourceKind::Playlist, 3778678));
        assert_eq!(parse_input("https://music.163.com/playlist?id=3778678&userid=1"), res(ResourceKind::Playlist, 3778678));
        assert_eq!(parse_input("https://y.music.163.com/m/playlist?id=42&creatorId=7"), res(ResourceKind::Playlist, 42));
        assert_eq!(parse_input("https://music.163.com/#/album?id=34209"), res(ResourceKind::Album, 34209));
        assert_eq!(parse_input("https://music.163.com/song?id=347230"), res(ResourceKind::Song, 347230));
        assert_eq!(parse_input("https://music.163.com/#/discover/toplist?id=19723756"), res(ResourceKind::Playlist, 19723756));
    }

    #[test]
    fn share_text_and_short_links() {
        let share = "分享歌单: 我喜欢的音乐 https://music.163.com/playlist?id=99&userid=5 (来自@网易云音乐)";
        assert_eq!(parse_input(share), res(ResourceKind::Playlist, 99));
        assert_eq!(
            parse_input("分享 https://163cn.tv/AbC123 (来自@网易云音乐)"),
            ParsedInput::ShortLink("https://163cn.tv/AbC123".into())
        );
    }

    #[test]
    fn bare_and_garbage() {
        assert_eq!(parse_input(" 12345 "), ParsedInput::BareId(12345));
        assert_eq!(parse_input("hello"), ParsedInput::Unrecognized);
        assert_eq!(parse_input("https://example.com/playlist?id=1"), ParsedInput::Unrecognized);
    }
}
