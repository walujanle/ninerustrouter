//! Update checking against the GitHub Releases of `walujanle/ninerustrouter`.
//!
//! One signal: the newest release tag compared to the running version. The
//! assets are zips, so the running executable can never be byte-compared to a
//! download and there is no second signal to compute.
//!
//! `GET /api/version` reads the cache and never blocks on the network; the
//! scheduler fills it. See `docs/RUNTIME.md`.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{LazyLock, RwLock};
use std::time::Duration;

use serde_json::Value;

use router_sse::executors::http::tls_builder;

use crate::state::{APP_VERSION, AppState};

const RELEASES_URL: &str = "https://api.github.com/repos/walujanle/ninerustrouter/releases/latest";
const USER_AGENT: &str = "ninerustrouter-update-check";
/// One check a day is the ask; a manual `GET /api/version` refreshes sooner when
/// the cache is this old.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const STALE_AFTER_MS: i64 = 6 * 60 * 60 * 1000;
const STARTUP_DELAY: Duration = Duration::from_secs(60);

/// What the version route reports. Absent fields are `null` in the JSON.
#[derive(Clone, Debug, Default)]
pub struct UpdateStatus {
    pub latest: Option<String>,
    pub has_update: bool,
    pub release_url: Option<String>,
    pub checked_at: Option<String>,
}

static CACHE: LazyLock<RwLock<Option<UpdateStatus>>> = LazyLock::new(|| RwLock::new(None));
static RUNNING: AtomicBool = AtomicBool::new(false);
static LAST_ATTEMPT_MS: AtomicI64 = AtomicI64::new(0);
/// The loop task, so `configure(false)` aborts it rather than leaving it parked
/// in a sleep that a later `start` would race with a second loop.
static TASK: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>> = std::sync::Mutex::new(None);

/// The cached status, or `None` before the first check completes.
pub fn cached() -> Option<UpdateStatus> {
    CACHE.read().ok().and_then(|c| c.clone())
}

/// Kick off a check in the background when the cache is stale. Never blocks the
/// caller, so the public version route stays cheap.
///
/// The attempt timestamp is claimed here, before the spawn, so a burst of
/// concurrent `/api/version` requests fires one check rather than one per
/// request: the second caller reads the timestamp this call just stored.
pub fn refresh_if_stale(state: AppState) {
    let now = router_db::time::now_ms();
    let last = LAST_ATTEMPT_MS.load(Ordering::SeqCst);
    if last != 0 && now - last < STALE_AFTER_MS {
        return;
    }
    if LAST_ATTEMPT_MS
        .compare_exchange(last, now, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    tokio::spawn(async move {
        check_once(&state).await;
    });
}

/// Start the daily loop while `autoUpdateCheck` is on, stop it otherwise.
pub fn configure(state: AppState, settings: &Value) {
    let enabled = settings
        .get("autoUpdateCheck")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if enabled {
        start(state);
    } else {
        RUNNING.store(false, Ordering::SeqCst);
        if let Ok(mut slot) = TASK.lock()
            && let Some(handle) = slot.take()
        {
            handle.abort();
        }
    }
}

fn start(state: AppState) {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let handle = tokio::spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;
        loop {
            if !RUNNING.load(Ordering::SeqCst) {
                break;
            }
            check_once(&state).await;
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
    if let Ok(mut slot) = TASK.lock() {
        *slot = Some(handle);
    }
}

/// One poll. Failures keep the previous cache and only log, so a rate limit or
/// an offline machine never clears a known-good status.
async fn check_once(_state: &AppState) {
    LAST_ATTEMPT_MS.store(router_db::time::now_ms(), Ordering::SeqCst);
    match fetch_latest().await {
        Ok(status) => {
            if let Ok(mut cache) = CACHE.write() {
                *cache = Some(status);
            }
        }
        Err(error) => tracing::warn!("[UpdateCheck] {error}"),
    }
}

async fn fetch_latest() -> Result<UpdateStatus, String> {
    let client = tls_builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(RELEASES_URL)
        .header("accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("release lookup failed ({})", response.status()));
    }
    let body: Value = response.json().await.map_err(|e| e.to_string())?;

    let latest = body
        .get("tag_name")
        .and_then(Value::as_str)
        .map(|tag| tag.trim_start_matches('v').to_string())
        .filter(|v| !v.is_empty());
    let release_url = body
        .get("html_url")
        .and_then(Value::as_str)
        .map(str::to_string);

    let has_update = latest.as_deref().is_some_and(|l| is_newer(l, APP_VERSION));

    Ok(UpdateStatus {
        latest,
        has_update,
        release_url,
        checked_at: Some(router_db::time::now_iso()),
    })
}

/// `x.y.z` tuple comparison. A non-numeric component sorts as 0.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    parse_version(candidate) > parse_version(current)
}

fn parse_version(v: &str) -> (u64, u64, u64) {
    let mut parts = v.trim().split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare_is_numeric_not_lexical() {
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(is_newer("0.2.1", "0.2.0"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
    }

    #[test]
    fn a_missing_component_sorts_as_zero() {
        assert!(is_newer("0.3", "0.2.9"));
        assert_eq!(parse_version("1"), (1, 0, 0));
        assert_eq!(parse_version("junk"), (0, 0, 0));
    }
}
