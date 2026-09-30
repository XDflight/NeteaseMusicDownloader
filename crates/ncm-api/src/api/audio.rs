use serde_json::{Value, json};

use crate::client::{Client, ids_json};
use crate::error::Result;
use crate::models::{Level, Lyrics, SongUrl};

impl Client {
    /// Streaming URLs for `ids` at `level`. The server serves the best tier the account may use
    /// up to `level`; check [`SongUrl::level`] / [`SongUrl::is_trial`] before trusting the result.
    pub async fn song_urls(&self, ids: &[u64], level: Level) -> Result<Vec<SongUrl>> {
        let mut params = json!({
            "ids": ids_json(ids),
            "level": level.as_str(),
            "encodeType": "flac",
        });
        if level == Level::Sky {
            params["immerseType"] = Value::String("c51".into());
        }
        let body = self.eapi("/song/enhance/player/url/v1", params).await?;
        Ok(body
            .get("data")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(SongUrl::from_json).collect())
            .unwrap_or_default())
    }

    /// The URL the official client's "download" button uses; sometimes succeeds where the
    /// streaming endpoint only offers a trial.
    pub async fn download_url(&self, id: u64, level: Level) -> Result<Option<SongUrl>> {
        let mut params = json!({ "id": id, "level": level.as_str(), "encodeType": "flac" });
        if level == Level::Sky {
            params["immerseType"] = Value::String("c51".into());
        }
        let body = self.eapi("/song/enhance/download/url/v1", params).await?;
        Ok(body.get("data").and_then(|d| {
            let mut d = d.clone();
            if d.get("id").is_none() {
                d["id"] = json!(id);
            }
            SongUrl::from_json(&d)
        }))
    }

    pub async fn lyrics(&self, id: u64) -> Result<Lyrics> {
        let body = self
            .eapi(
                "/song/lyric/v1",
                json!({ "id": id, "cp": false, "tv": -1, "lv": -1, "rv": -1, "kv": -1, "yv": -1, "ytv": -1, "yrv": -1 }),
            )
            .await?;
        Ok(Lyrics::from_json(&body))
    }
}
