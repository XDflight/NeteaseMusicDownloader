use std::collections::HashMap;

use serde_json::{Value, json};

use crate::client::Client;
use crate::error::{Error, Result};
use crate::link::{ParsedInput, Resource, ResourceKind, parse_input};
use crate::models::{Collection, CollectionKind, PlaylistSummary, Track, str_of};

/// Song ids per `song/detail` request. The service accepts large batches; this stays well below.
const DETAIL_BATCH: usize = 300;

impl Client {
    /// Playlists created or bookmarked by `user_id`. Private ones are included for the logged-in user.
    pub async fn user_playlists(&self, user_id: u64) -> Result<Vec<PlaylistSummary>> {
        let mut all = Vec::new();
        let mut offset = 0usize;
        loop {
            let body = self
                .eapi("/user/playlist", json!({ "uid": user_id, "limit": 100, "offset": offset, "includeVideo": true }))
                .await?;
            let page: Vec<PlaylistSummary> = body
                .get("playlist")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(PlaylistSummary::from_json).collect())
                .unwrap_or_default();
            let n = page.len();
            all.extend(page);
            let more = body.get("more").and_then(Value::as_bool).unwrap_or(false);
            if !more || n == 0 {
                return Ok(all);
            }
            offset += n;
        }
    }

    /// Full contents of a playlist (public, or private when the session owns it).
    pub async fn playlist(&self, id: u64) -> Result<Collection> {
        let body = self.eapi("/v6/playlist/detail", json!({ "id": id, "n": 100000, "s": 8 })).await?;
        let pl = body.get("playlist").filter(|p| !p.is_null()).ok_or_else(|| Error::api(404, "歌单不存在或无权访问"))?;
        let summary = PlaylistSummary::from_json(pl).ok_or_else(|| Error::Other("malformed playlist".into()))?;

        let ids: Vec<u64> = pl
            .get("trackIds")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|t| t.get("id").and_then(Value::as_u64)).collect())
            .unwrap_or_default();
        let tracks = self.tracks(&ids).await?;
        Ok(Collection {
            kind: CollectionKind::Playlist,
            id,
            name: summary.name,
            cover_url: summary.cover_url,
            creator: summary.creator_name,
            tracks,
        })
    }

    pub async fn album(&self, id: u64) -> Result<Collection> {
        let body = self.eapi(&format!("/v1/album/{id}"), json!({})).await?;
        let album = body.get("album").filter(|a| !a.is_null()).ok_or_else(|| Error::api(404, "专辑不存在"))?;
        let privileges: HashMap<u64, &Value> = body
            .get("songs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|s| Some((s.get("id")?.as_u64()?, s.get("privilege")?)))
            .collect();
        let tracks: Vec<Track> = body
            .get("songs")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|s| {
                        let id = s.get("id")?.as_u64()?;
                        Track::from_json(s, privileges.get(&id).copied())
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Collection {
            kind: CollectionKind::Album,
            id,
            name: str_of(album, "name"),
            cover_url: album.get("picUrl").and_then(Value::as_str).map(str::to_owned),
            creator: album.get("artist").map(|a| str_of(a, "name")).filter(|s| !s.is_empty()).unwrap_or_default(),
            tracks,
        })
    }

    pub async fn song(&self, id: u64) -> Result<Collection> {
        let tracks = self.tracks(&[id]).await?;
        let track = tracks.into_iter().next().ok_or_else(|| Error::api(404, "歌曲不存在"))?;
        Ok(Collection {
            kind: CollectionKind::Song,
            id,
            name: track.name.clone(),
            cover_url: track.album.pic_url.clone(),
            creator: track.artist_names("/"),
            tracks: vec![track],
        })
    }

    /// Track metadata for `ids`, in the given order. Ids the service no longer knows are dropped.
    pub async fn tracks(&self, ids: &[u64]) -> Result<Vec<Track>> {
        let mut by_id: HashMap<u64, Track> = HashMap::with_capacity(ids.len());
        for batch in ids.chunks(DETAIL_BATCH) {
            let c = Value::Array(batch.iter().map(|id| json!({ "id": id })).collect()).to_string();
            let body = self.eapi("/v3/song/detail", json!({ "c": c })).await?;
            let privileges: HashMap<u64, &Value> = body
                .get("privileges")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|p| Some((p.get("id")?.as_u64()?, p)))
                .collect();
            for song in body.get("songs").and_then(Value::as_array).into_iter().flatten() {
                let Some(id) = song.get("id").and_then(Value::as_u64) else { continue };
                if let Some(track) = Track::from_json(song, privileges.get(&id).copied()) {
                    by_id.insert(id, track);
                }
            }
        }
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// Resolve any user-supplied text (link, share text, short link) into a track collection.
    /// `bare_id_kind` decides what a bare number means.
    pub async fn resolve_input(&self, text: &str, bare_id_kind: ResourceKind) -> Result<Collection> {
        let resource = match parse_input(text) {
            ParsedInput::Resource(r) => r,
            ParsedInput::BareId(id) => Resource { kind: bare_id_kind, id },
            ParsedInput::ShortLink(url) => {
                let final_url = self.http().get(&url).send().await?.url().to_string();
                match parse_input(&final_url) {
                    ParsedInput::Resource(r) => r,
                    _ => return Err(Error::Other("无法识别该短链接指向的内容".into())),
                }
            }
            ParsedInput::Unrecognized => return Err(Error::Other("无法识别的链接或 ID".into())),
        };
        match resource.kind {
            ResourceKind::Playlist => self.playlist(resource.id).await,
            ResourceKind::Album => self.album(resource.id).await,
            ResourceKind::Song => self.song(resource.id).await,
        }
    }
}
