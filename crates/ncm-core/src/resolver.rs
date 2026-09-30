//! Resolving track ids to download URLs in batches, with a short-lived cache.

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use ncm_api::{Client, Error as ApiErr, Level, SongUrl};
use tokio::sync::Mutex;

use crate::tuning as t;

pub struct UrlResolver {
    api: Client,
    cache: StdMutex<HashMap<(u64, Level), (Instant, SongUrl)>>,
    /// Serialises lookups: pacing for the API and a chance to hit the cache after waiting.
    lock: Mutex<()>,
}

impl UrlResolver {
    pub fn new(api: Client) -> Self {
        Self { api, cache: StdMutex::new(HashMap::new()), lock: Mutex::new(()) }
    }

    fn cached(&self, level: Level, id: u64) -> Option<SongUrl> {
        let cache = self.cache.lock().unwrap();
        cache.get(&(id, level)).filter(|(at, _)| at.elapsed() < t::URL_CACHE_TTL).map(|(_, u)| u.clone())
    }

    /// The URL for `id`; also looks up up to `URL_BATCH - 1` of the `upcoming` ids in the same
    /// request so that later tracks are served from the cache.
    pub async fn resolve(&self, level: Level, id: u64, upcoming: &[u64], fresh: bool) -> Result<SongUrl, ApiErr> {
        if !fresh && let Some(u) = self.cached(level, id) {
            return Ok(u);
        }
        let _guard = self.lock.lock().await;
        if !fresh && let Some(u) = self.cached(level, id) {
            return Ok(u);
        }

        let mut ids = vec![id];
        ids.extend(
            upcoming
                .iter()
                .copied()
                .filter(|i| *i != id && self.cached(level, *i).is_none())
                .take(t::URL_BATCH.saturating_sub(1)),
        );

        let results = self.lookup_with_backoff(&ids, level).await?;
        let now = Instant::now();
        let mut wanted = None;
        {
            let mut cache = self.cache.lock().unwrap();
            for u in results {
                if u.id == id {
                    wanted = Some(u.clone());
                }
                if u.is_usable() {
                    cache.insert((u.id, level), (now, u));
                }
            }
            cache.retain(|_, (at, _)| at.elapsed() < t::URL_CACHE_TTL);
        }
        Ok(wanted.unwrap_or(SongUrl { id, ..SongUrl::default() }))
    }

    async fn lookup_with_backoff(&self, ids: &[u64], level: Level) -> Result<Vec<SongUrl>, ApiErr> {
        let mut delay = Duration::from_secs(3);
        for attempt in 0..t::API_THROTTLE_RETRIES {
            match self.api.song_urls(ids, level).await {
                Err(e) if e.is_throttled() && attempt + 1 < t::API_THROTTLE_RETRIES => {
                    tracing::warn!("URL lookup throttled ({e}); waiting {delay:?}");
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
                other => return other,
            }
        }
        unreachable!("loop returns on the last attempt")
    }
}
