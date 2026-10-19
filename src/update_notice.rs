//! Passive "a newer monocle is available" notice (stderr only).
//!
//! Design: at startup, if enabled and `~/.monocle/update-check.json` is older
//! than 24h, a detached thread bumps `last_checked` (backoff) and does ONE 3s
//! GitHub check, storing `latest_version`. When the command finishes, `finish`
//! waits at most ~300ms for that thread, then prints the notice from the cache.
//! So the command is never delayed by more than the grace bound, and a slow
//! check just lands in the cache for the next run. All errors are swallowed.
//!
//! `token`, `upgrade` and `acp` are excluded by [`command_eligible`].

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

/// Any unreadable/corrupt/partial cache (bad JSON, wrong types, empty file)
/// is `None`, i.e. stale: never an error or panic.
pub fn read_cache(home: &Path) -> Option<Cache> {
    serde_json::from_slice(&std::fs::read(cache_path(home)).ok()?).ok()
}

/// Atomic write: temp file in the same dir (unique per pid+counter), then
/// `rename`. No lock file: racing processes may occasionally run a duplicate
/// check, which is acceptable; the cache is never torn.
pub fn write_cache(home: &Path, cache: &Cache) {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = cache_path(home);
    let Some(dir) = path.parent() else { return };
    let _ = std::fs::create_dir_all(dir);
    let Ok(json) = serde_json::to_vec(cache) else {
        return;
    };
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".update-check.{}.{seq}.tmp", std::process::id()));
    if std::fs::write(&tmp, json).is_err() || std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Commands that never get the notice: `token` (apiKeyHelper: pristine stdout,
/// no network), `upgrade` (does its own check), `acp` (protocol stream).
pub fn command_eligible(name: &str) -> bool {
    !matches!(name, "token" | "upgrade" | "acp")
}

/// Full decision: should a background check even start for this command?
pub fn enabled_for(name: &str, env: Env) -> bool {
    command_eligible(name) && !suppressed(env)
}

/// If the cache is stale, bump `last_checked` FIRST (so a slow/offline fetch
/// that gets cut off with the process still backs off for 24h), then fetch and
/// store the new version on success. Returns whether a fetch was attempted.
pub fn refresh_if_stale(client: &Client, home: &Path, url: &str, now: i64) -> bool {
    let cache = read_cache(home);
    if !is_stale(now, cache.as_ref()) {
        return false;
    }
    let mut next = cache.unwrap_or_default();
    next.last_checked = now;
    write_cache(home, &next);
    if let Ok(latest) = fetch_latest_version(client, url, Some(CHECK_TIMEOUT)) {
        next.latest_version = Some(latest);
        write_cache(home, &next);
    }
    true
}

/// The notice to print from the cache, if any (no network).
pub fn cached_notice(home: &Path, current: &str) -> Option<String> {
    notice_message(current, read_cache(home)?.latest_version.as_deref()?)
}

/// Refresh then notice, synchronously (tests / simple callers).
pub fn check(client: &Client, home: &Path, url: &str, now: i64, current: &str) -> Option<String> {
    refresh_if_stale(client, home, url, now);
    cached_notice(home, current)
}

/// Handle for the background refresh started by [`start`].
pub struct Pending(Option<std::sync::mpsc::Receiver<()>>);

/// Called at startup: if enabled and the cache is stale, refresh on a detached
/// thread so the command itself is never delayed.
pub fn start(name: &str, home: &Path) -> Pending {
    start_with(name, home, Env::from_process(), latest_release_url())
}

pub fn start_with(name: &str, home: &Path, env: Env, url: String) -> Pending {
    let now = chrono::Utc::now().timestamp();
    if !enabled_for(name, env) || !is_stale(now, read_cache(home).as_ref()) {
        return Pending(None);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let home = home.to_path_buf();
    std::thread::spawn(move || {
        refresh_if_stale(&Client::new(), &home, &url, now);
        let _ = tx.send(());
    });
    Pending(Some(rx))
}

/// Called after the command finished: wait at most `grace` for the background
/// refresh, then print the cached notice (if enabled) to stderr.
pub fn finish(pending: Pending, name: &str, home: &Path, grace: Duration) {
    finish_with(pending, name, home, grace, Env::from_process());
}

pub fn finish_with(pending: Pending, name: &str, home: &Path, grace: Duration, env: Env) {
    if !enabled_for(name, env) {
        return;
    }
    if let Some(rx) = pending.0 {
        let _ = rx.recv_timeout(grace);
    }
    if let Some(msg) = cached_notice(home, env!("CARGO_PKG_VERSION")) {
        eprintln!("{msg}");
    }
}

/// Default bound on how long the command's exit may wait for the refresh.
pub const DEFAULT_GRACE: Duration = Duration::from_millis(300);
