//! A daily, bounded release check. It never runs on the GUI or stream threads.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

const CHECK_INTERVAL: u64 = 24 * 60 * 60;
const LATEST_URL: &str = "https://api.github.com/repos/whoisaldo/EternalMonitor/releases/latest";
static CHECK_RUNNING: AtomicBool = AtomicBool::new(false);
pub type AvailableUpdate = Arc<Mutex<Option<String>>>;

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    checked_at: Option<u64>,
    tag: Option<String>,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

fn newer_stable(tag: &str, current: &str) -> Option<String> {
    let mut release = semver::Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()?;
    let mut current = semver::Version::parse(current).ok()?;
    if !release.pre.is_empty() {
        return None;
    }
    release.build = semver::BuildMetadata::EMPTY;
    current.build = semver::BuildMetadata::EMPTY;
    (release > current).then(|| release.to_string())
}

fn check(cache: &mut Cache, now: u64, fetch: impl FnOnce() -> Result<Release, String>) -> bool {
    if cache
        .checked_at
        .is_some_and(|last| now.saturating_sub(last) < CHECK_INTERVAL)
    {
        return false;
    }
    // Cache failures too: repeated launches must not hammer the API offline.
    cache.checked_at = Some(now);
    match fetch() {
        Ok(release) => {
            cache.tag = (!release.prerelease && !release.draft).then_some(release.tag_name)
        }
        Err(error) => tracing::debug!(%error, "Release check unavailable"),
    }
    true
}

fn fetch_release() -> Result<Release, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(3)))
        .max_redirects(2)
        .build()
        .new_agent();
    agent
        .get(LATEST_URL)
        .header(
            "User-Agent",
            concat!("EternalMonitor/", env!("CARGO_PKG_VERSION")),
        )
        .header("Accept", "application/vnd.github+json")
        .call()
        .and_then(|mut response| {
            response
                .body_mut()
                .with_config()
                .limit(64 * 1024)
                .read_json::<Release>()
        })
        .map_err(|error| error.to_string())
}

fn load_cache(path: &Path) -> Cache {
    std::fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

/// Called only when checking is enabled. The same result is reused by the GUI.
pub fn start(result: AvailableUpdate) {
    let Some(path) = crate::settings::app_data_dir().map(|dir| dir.join("release-check.json"))
    else {
        return;
    };
    if CHECK_RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Err(error) = std::thread::Builder::new()
        .name("release-check".into())
        .spawn(move || {
            let mut cache = load_cache(&path);
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if check(&mut cache, now, fetch_release) {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(data) = serde_json::to_vec(&cache) {
                    let tmp = path.with_extension("json.tmp");
                    if std::fs::write(&tmp, data).is_ok() {
                        let _ = std::fs::rename(tmp, &path);
                    }
                }
            }
            *result.lock() = cache
                .tag
                .as_deref()
                .and_then(|tag| newer_stable(tag, env!("CARGO_PKG_VERSION")));
            CHECK_RUNNING.store(false, Ordering::SeqCst);
        })
    {
        CHECK_RUNNING.store(false, Ordering::SeqCst);
        tracing::debug!(%error, "Could not start release check");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_use_semver_ignore_previews_and_ignore_build_metadata() {
        assert_eq!(newer_stable("v1.10.0", "1.9.9"), Some("1.10.0".into()));
        assert_eq!(newer_stable("v0.3.0", "0.3.0-rc.1"), Some("0.3.0".into()));
        for tag in [
            "v0.3.0",
            "v0.2.0",
            "v0.4.0-rc.1",
            "v0.3.0+new",
            "v0.4",
            "https://example.com",
            "garbage",
        ] {
            assert_eq!(newer_stable(tag, "0.3.0"), None, "{tag}");
        }
    }

    #[test]
    fn daily_cache_reuses_success_and_throttles_failures() {
        let mut cache = Cache::default();
        assert!(check(&mut cache, 10, || Ok(Release {
            tag_name: "v0.4.0".into(),
            prerelease: false,
            draft: false
        })));
        assert!(!check(&mut cache, 10 + CHECK_INTERVAL - 1, || panic!(
            "too early"
        )));
        assert!(check(&mut cache, 10 + CHECK_INTERVAL, || Err(
            "offline".into()
        )));
        assert_eq!(cache.tag.as_deref(), Some("v0.4.0"));
        assert!(!check(&mut cache, 11 + CHECK_INTERVAL, || panic!(
            "failure must also be cached"
        )));
        let json = serde_json::to_vec(&cache).unwrap();
        let mut restored: Cache = serde_json::from_slice(&json).unwrap();
        assert!(!check(&mut restored, 11 + CHECK_INTERVAL, || panic!(
            "restart must reuse cache"
        )));
        assert!(check(&mut restored, 10 + CHECK_INTERVAL * 2, || Ok(
            Release {
                tag_name: "v0.5.0".into(),
                prerelease: true,
                draft: false
            }
        )));
        assert_eq!(restored.tag, None);
    }
}
