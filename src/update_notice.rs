//! Passive "a newer monocle is available" notice (stderr only).
//!
//! Design: once the command has finished, `run` loads `~/.monocle/update-check.json`.
//! If the cache is older than 24h it does ONE short-timeout (3s) GitHub check,
//! then rewrites the cache (also on failure, so an offline user is not retried
//! on every run). The notice is printed from the cached `latest_version`.
//! Because it runs after the command, it can never delay or fail the command's
//! own work; the worst case is one ~3s wait per day while offline/slow.
//! Everything is best-effort: all errors are swallowed.
//!
//! The caller (main.rs) excludes `token`, `upgrade` and `acp`.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::commands::upgrade::{compare_versions, fetch_latest_version, latest_release_url};
use crate::net::Client;

const CHECK_INTERVAL_SECS: i64 = 24 * 60 * 60;
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cache {
    /// Unix seconds of the last attempted check (successful or not).
    pub last_checked: i64,
    pub latest_version: Option<String>,
}

/// Environment inputs to the suppression decision.
#[derive(Debug, Clone, Copy)]
pub struct Env {
    pub stderr_is_tty: bool,
    pub ci: bool,
    pub opted_out: bool,
}

impl Env {
    pub fn from_process() -> Self {
        let set = |k: &str| std::env::var(k).is_ok_and(|v| !v.is_empty());
        Env {
            stderr_is_tty: std::io::stderr().is_terminal(),
            ci: set("CI"),
            opted_out: std::env::var("MONOCLE_NO_UPDATE_CHECK")
                .is_ok_and(|v| !v.is_empty() && v != "0"),
        }
    }
}

/// True when the notice (and any network check) must not happen.
pub fn suppressed(env: Env) -> bool {
    !env.stderr_is_tty || env.ci || env.opted_out
}

/// True when the cache is missing or at least 24h old (a clock that went
/// backwards also counts as stale).
pub fn is_stale(now: i64, cache: Option<&Cache>) -> bool {
    match cache {
        None => true,
        Some(c) => now < c.last_checked || now - c.last_checked >= CHECK_INTERVAL_SECS,
    }
}

/// The exact notice line, only when `latest` is strictly newer than `current`.
pub fn notice_message(current: &str, latest: &str) -> Option<String> {
    (compare_versions(current, latest)? == std::cmp::Ordering::Less).then(|| {
        format!("monocle {latest} is available (current {current}). Run `monocle upgrade`.")
    })
}

fn cache_path(home: &Path) -> PathBuf {
    home.join(".monocle").join("update-check.json")
}

fn read_cache(home: &Path) -> Option<Cache> {
    serde_json::from_slice(&std::fs::read(cache_path(home)).ok()?).ok()
}

fn write_cache(home: &Path, cache: &Cache) {
    let path = cache_path(home);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec(cache) {
        let _ = std::fs::write(path, json);
    }
}

/// Refresh the cache if stale (one network call at most), and return the notice
/// to print, if any. `url` is injectable for tests.
pub fn check(client: &Client, home: &Path, url: &str, now: i64, current: &str) -> Option<String> {
    let mut cache = read_cache(home);
    if is_stale(now, cache.as_ref()) {
        let latest = fetch_latest_version(client, url, Some(CHECK_TIMEOUT)).ok();
        let mut next = cache.unwrap_or_default();
        next.last_checked = now;
        if latest.is_some() {
            next.latest_version = latest;
        }
        write_cache(home, &next);
        cache = Some(next);
    }
    notice_message(current, cache?.latest_version.as_deref()?)
}

/// Thin wrapper called by `main` after a command completes successfully.
pub fn run(client: &Client, home: &Path) {
    if suppressed(Env::from_process()) {
        return;
    }
    let now = chrono::Utc::now().timestamp();
    if let Some(msg) = check(
        client,
        home,
        &latest_release_url(),
        now,
        env!("CARGO_PKG_VERSION"),
    ) {
        eprintln!("{msg}");
    }
}
