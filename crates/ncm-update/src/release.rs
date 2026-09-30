use semver::Version;
use serde_json::Value;

use crate::target::Target;
use crate::{Result, UpdateError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct Release {
    pub tag: String,
    pub version: Version,
    pub name: String,
    pub notes: String,
    pub html_url: String,
    pub published_at: String,
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}

/// A newer release together with the asset this installation should use.
#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub release: Release,
    pub asset: Asset,
    pub checksums: Option<Asset>,
    pub target: Target,
}

/// Parse either a single release object (`/releases/latest`) or an array (`/releases`).
/// Drafts and (unless asked for) prereleases are skipped; tags that are not semver are ignored.
pub(crate) fn parse_releases(body: &Value, include_prerelease: bool) -> Result<Vec<Release>> {
    let items: Vec<&Value> = match body {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![body],
        _ => return Err(UpdateError::Parse("expected a release object or array".into())),
    };
    Ok(items
        .into_iter()
        .filter_map(|r| {
            if r.get("draft").and_then(Value::as_bool).unwrap_or(false) {
                return None;
            }
            let prerelease = r.get("prerelease").and_then(Value::as_bool).unwrap_or(false);
            if prerelease && !include_prerelease {
                return None;
            }
            let tag = r.get("tag_name")?.as_str()?.to_owned();
            let version = Version::parse(tag.trim_start_matches(['v', 'V'])).ok()?;
            let text = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
            let assets = r
                .get("assets")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| {
                            Some(Asset {
                                name: x.get("name")?.as_str()?.to_owned(),
                                url: x.get("browser_download_url")?.as_str()?.to_owned(),
                                size: x.get("size").and_then(Value::as_u64).unwrap_or(0),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(Release {
                name: text("name"),
                notes: text("body"),
                html_url: text("html_url"),
                published_at: text("published_at"),
                tag,
                version,
                prerelease,
                assets,
            })
        })
        .collect())
}
