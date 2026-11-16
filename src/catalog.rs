//! Discord's public list of games it can detect, used to find the official
//! Discord application for a Steam game.
//!
//! Each entry of the list names the stores the game is sold on, including
//! its Steam app ID. Showing a presence under the game's own application
//! makes Discord display its real title and icon, with no setup.
//!
//! The list is about 13 MB, so only the Steam ID to application ID mapping
//! is kept, in a cache file that is refreshed once a week.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};

pub const DETECTABLE_URL: &str = "https://discord.com/api/v10/applications/detectable";
const CACHE_FILE: &str = "discord-games.json";
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(10 * 60);

/// Downloads the raw list. Injectable so tests never use the network.
pub type Fetcher = Box<dyn FnMut() -> Result<Vec<u8>>>;

/// Steam app ID to Discord application ID.
pub type Index = BTreeMap<u64, String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// Discord has an application for this game.
    Found(String),
    /// The list is available and the game is not in it.
    NotListed,
    /// The list has not been downloaded yet.
    Unavailable,
}

#[derive(Deserialize)]
struct Entry {
    id: String,
    #[serde(default)]
    third_party_skus: Option<Vec<Sku>>,
}

#[derive(Deserialize)]
struct Sku {
    distributor: Option<String>,
    id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    steam: Index,
}

/// Builds the index from the JSON Discord serves.
pub fn index_from_detectable(json: &[u8]) -> Result<Index> {
    let entries: Vec<Entry> =
        serde_json::from_slice(json).context("unexpected format of Discord's game list")?;
    let mut index = Index::new();
    for entry in entries {
        if entry.id.is_empty() || !entry.id.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        for sku in entry.third_party_skus.unwrap_or_default() {
            if sku.distributor.as_deref() != Some("steam") {
                continue;
            }
            // Some IDs in the list carry stray whitespace.
            let Some(steam_id) = sku.id.and_then(|id| id.trim().parse::<u64>().ok()) else {
                continue;
            };
            // The first application listed for a Steam ID wins.
            index.entry(steam_id).or_insert_with(|| entry.id.clone());
        }
    }
    if index.is_empty() {
        bail!("Discord's game list contains no Steam games");
    }
    Ok(index)
}

/// Downloads the list with `curl`, which every Arch-based system has.
pub fn curl_fetch() -> Result<Vec<u8>> {
    let output = Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location"])
        .args([
            "--compressed",
            "--connect-timeout",
            "10",
            "--max-time",
            "60",
        ])
        .arg(DETECTABLE_URL)
        .stdin(Stdio::null())
        .output()
        .context("cannot run curl")?;
    if !output.status.success() {
        bail!(
            "curl: {}",
            String::from_utf8_lossy(&output.stderr)
                .trim()
                .trim_start_matches("curl: ")
        );
    }
    Ok(output.stdout)
}

pub fn cache_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join(CACHE_FILE)
}

fn read_cache(path: &Path) -> Option<Index> {
    let file: CacheFile = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    Some(file.steam)
}

fn lookup_in(index: &Index, steam_app_id: u64) -> Lookup {
    match index.get(&steam_app_id) {
        Some(id) => Lookup::Found(id.clone()),
        None => Lookup::NotListed,
    }
}

/// Looks a game up in the cache only, without downloading anything.
pub fn cached_lookup(cache_dir: &Path, steam_app_id: u64) -> Lookup {
    match read_cache(&cache_path(cache_dir)) {
        Some(index) => lookup_in(&index, steam_app_id),
        None => Lookup::Unavailable,
    }
}

pub struct Catalog {
    cache: PathBuf,
    fetch: Fetcher,
    /// No download before this instant, after one failed.
    retry_at: Option<Instant>,
    failure_logged: bool,
}

impl Catalog {
    pub fn new(cache_dir: &Path, fetch: Fetcher) -> Catalog {
        Catalog {
            cache: cache_path(cache_dir),
            fetch,
            retry_at: None,
            failure_logged: false,
        }
    }

    /// Finds the Discord application for a Steam game, downloading the
    /// list first if the cache is missing or older than a week. `now_ms`
    /// is Unix time in milliseconds.
    pub fn lookup(&mut self, steam_app_id: u64, now: Instant, now_ms: u64) -> Lookup {
        let mut index = read_cache(&self.cache);
        let outdated = index.is_none() || self.is_stale(now_ms);
        if outdated && self.retry_at.is_none_or(|due| now >= due) && self.refresh(now) {
            index = read_cache(&self.cache);
        }
        // A stale cache is still better than nothing.
        match index {
            Some(index) => lookup_in(&index, steam_app_id),
            None => Lookup::Unavailable,
        }
    }

    fn is_stale(&self, now_ms: u64) -> bool {
        let modified = fs::metadata(&self.cache)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok());
        match modified {
            Some(modified) => Duration::from_millis(now_ms).saturating_sub(modified) > MAX_AGE,
            None => true,
        }
    }

    /// Downloads and caches the list. Returns whether it succeeded.
    fn refresh(&mut self, now: Instant) -> bool {
        match (self.fetch)().and_then(|json| self.store(&json)) {
            Ok(games) => {
                info!("Downloaded Discord's game list ({games} Steam games)");
                self.retry_at = None;
                self.failure_logged = false;
                true
            }
            Err(e) => {
                self.retry_at = Some(now + RETRY_AFTER_FAILURE);
                if self.failure_logged {
                    debug!("Cannot download Discord's game list: {e:#}");
                } else {
                    warn!("Cannot download Discord's game list: {e:#}; retrying every 10 minutes");
                    self.failure_logged = true;
                }
                false
            }
        }
    }

    fn store(&self, json: &[u8]) -> Result<usize> {
        let steam = index_from_detectable(json)?;
        let games = steam.len();
        let dir = self.cache.parent().context("cache path has no parent")?;
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        // Write to a temporary file first so a crash cannot leave half a cache.
        let tmp = self.cache.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(&CacheFile { steam })?)
            .and_then(|()| fs::rename(&tmp, &self.cache))
            .with_context(|| format!("cannot write {}", self.cache.display()))?;
        Ok(games)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{counting_fetcher, TempDir, DETECTABLE_SAMPLE};
    use std::sync::atomic::Ordering;

    const WEEK_MS: u64 = 7 * 24 * 60 * 60 * 1000;

    fn real_now_ms() -> u64 {
        crate::daemon::unix_ms()
    }

    #[test]
    fn indexes_steam_games_from_discords_format() {
        let index = index_from_detectable(DETECTABLE_SAMPLE.as_bytes()).unwrap();
        assert_eq!(index[&529340], "1124352195157045371", "Victoria 3");
        assert_eq!(index[&1245620], "1402418436809953330", "ELDEN RING");
        assert_eq!(index[&207490], "111", "ID with stray whitespace");
        assert_eq!(index[&777], "222", "first application wins");
        assert_eq!(
            index.len(),
            4,
            "non-Steam and malformed entries are skipped"
        );
    }

    #[test]
    fn rejects_lists_it_cannot_use() {
        assert!(index_from_detectable(b"not json").is_err());
        assert!(index_from_detectable(b"{\"message\": \"rate limited\"}").is_err());
        assert!(index_from_detectable(b"[]").is_err());
        let no_steam =
            br#"[{"id": "1", "third_party_skus": [{"distributor": "xbox", "id": "X"}]}]"#;
        assert!(index_from_detectable(no_steam).is_err());
    }

    #[test]
    fn downloads_once_and_then_uses_the_cache() {
        let tmp = TempDir::new("catalog");
        let (fetch, calls) = counting_fetcher(Ok(DETECTABLE_SAMPLE));
        let mut catalog = Catalog::new(tmp.path(), fetch);
        let now = Instant::now();

        assert_eq!(cached_lookup(tmp.path(), 529340), Lookup::Unavailable);
        assert_eq!(
            catalog.lookup(529340, now, real_now_ms()),
            Lookup::Found("1124352195157045371".into())
        );
        assert_eq!(catalog.lookup(42, now, real_now_ms()), Lookup::NotListed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        assert_eq!(
            cached_lookup(tmp.path(), 529340),
            Lookup::Found("1124352195157045371".into())
        );
        assert_eq!(cached_lookup(tmp.path(), 42), Lookup::NotListed);
        assert!(!tmp.path().join("discord-games.json.tmp").exists());
    }

    #[test]
    fn refreshes_a_cache_older_than_a_week() {
        let tmp = TempDir::new("catalog");
        let (fetch, calls) = counting_fetcher(Ok(DETECTABLE_SAMPLE));
        let mut catalog = Catalog::new(tmp.path(), fetch);
        let now = Instant::now();

        catalog.lookup(1, now, real_now_ms());
        catalog.lookup(1, now, real_now_ms() + WEEK_MS - 60_000);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "still fresh");
        catalog.lookup(1, now, real_now_ms() + WEEK_MS + 60_000);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "stale");
    }

    #[test]
    fn failed_download_is_retried_only_after_ten_minutes() {
        let tmp = TempDir::new("catalog");
        let (fetch, calls) = counting_fetcher(Err("offline"));
        let mut catalog = Catalog::new(tmp.path(), fetch);
        let start = Instant::now();
        let at = |secs| start + Duration::from_secs(secs);

        assert_eq!(catalog.lookup(529340, at(0), 0), Lookup::Unavailable);
        assert_eq!(catalog.lookup(529340, at(5), 0), Lookup::Unavailable);
        assert_eq!(catalog.lookup(529340, at(599), 0), Lookup::Unavailable);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(catalog.lookup(529340, at(600), 0), Lookup::Unavailable);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn keeps_the_old_cache_when_a_refresh_fails() {
        let tmp = TempDir::new("catalog");
        let (fetch, _) = counting_fetcher(Ok(DETECTABLE_SAMPLE));
        Catalog::new(tmp.path(), fetch).lookup(1, Instant::now(), real_now_ms());

        for bad in [Err("offline"), Ok("<html>error</html>"), Ok("[]")] {
            let (fetch, calls) = counting_fetcher(bad);
            let mut catalog = Catalog::new(tmp.path(), fetch);
            let stale = real_now_ms() + WEEK_MS + 60_000;
            assert_eq!(
                catalog.lookup(529340, Instant::now(), stale),
                Lookup::Found("1124352195157045371".into())
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn unreadable_cache_is_replaced() {
        let tmp = TempDir::new("catalog");
        fs::write(cache_path(tmp.path()), "garbage").unwrap();
        assert_eq!(cached_lookup(tmp.path(), 1), Lookup::Unavailable);

        let (fetch, calls) = counting_fetcher(Ok(DETECTABLE_SAMPLE));
        let mut catalog = Catalog::new(tmp.path(), fetch);
        assert_eq!(
            catalog.lookup(1245620, Instant::now(), real_now_ms()),
            Lookup::Found("1402418436809953330".into())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
