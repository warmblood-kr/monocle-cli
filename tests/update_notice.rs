//! Update notice: cache honored, suppression rules, message format, and the
//! `token` command staying untouched.
mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use monocle_cli::credentials::{Credentials, CredentialsData};
use monocle_cli::net::Client;
use monocle_cli::update_notice::{
    check, command_eligible, enabled_for, finish_with, is_stale, notice_message, read_cache,
    start_with, suppressed, write_cache, Cache, Env,
};

const DAY: i64 = 24 * 60 * 60;

fn release_stub(tag: &'static str, hits: Arc<AtomicUsize>) -> common::Stub {
    common::stub(move |_, _, _, _| {
        hits.fetch_add(1, Ordering::SeqCst);
        (200, format!(r#"{{"tag_name":"{tag}"}}"#))
    })
}

fn url(s: &common::Stub) -> String {
    format!("{}/releases/latest", s.router_url())
}

fn seed(home: &std::path::Path, last_checked: i64, latest: &str) {
    std::fs::create_dir_all(home.join(".monocle")).unwrap();
    std::fs::write(
        home.join(".monocle/update-check.json"),
        format!(r#"{{"last_checked":{last_checked},"latest_version":"{latest}"}}"#),
    )
    .unwrap();
}

#[test]
fn message_format_and_semver_gate() {
    assert_eq!(
        notice_message("1.2.3", "1.3.0").unwrap(),
        "monocle 1.3.0 is available (current 1.2.3). Run `monocle upgrade`."
    );
    assert!(notice_message("1.3.0", "1.3.0").is_none());
    assert!(notice_message("1.10.0", "1.9.0").is_none());
    assert!(notice_message("1.0.0", "garbage").is_none());
}

#[test]
fn suppression_rules_every_combination() {
    for bits in 0..8u8 {
        let env = Env {
            stderr_is_tty: bits & 1 != 0,
            ci: bits & 2 != 0,
            opted_out: bits & 4 != 0,
        };
        // Only "TTY, no CI, not opted out" shows the notice.
        assert_eq!(suppressed(env), bits != 1, "bits={bits}");
    }
}

#[test]
fn staleness_window() {
    let c = monocle_cli::update_notice::Cache {
        last_checked: 1000,
        latest_version: None,
    };
    assert!(is_stale(1000, None));
    assert!(!is_stale(1000 + DAY - 1, Some(&c)));
    assert!(is_stale(1000 + DAY, Some(&c)));
    assert!(is_stale(999, Some(&c)));
}

#[test]
fn fresh_cache_makes_no_network_hit_but_still_notifies() {
    let hits = Arc::new(AtomicUsize::new(0));
    let stub = release_stub("v9.9.9", hits.clone());
    let home = tempfile::tempdir().unwrap();
    seed(home.path(), 5000, "2.0.0");
    let msg = check(&Client::new(), home.path(), &url(&stub), 5000 + 60, "1.0.0");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    assert_eq!(
        msg.unwrap(),
        "monocle 2.0.0 is available (current 1.0.0). Run `monocle upgrade`."
    );
}

#[test]
fn stale_cache_hits_network_once_and_updates_cache() {
    let hits = Arc::new(AtomicUsize::new(0));
    let stub = release_stub("v2.5.0", hits.clone());
    let home = tempfile::tempdir().unwrap();
    seed(home.path(), 5000, "1.0.0");
    let now = 5000 + 2 * DAY;
    let c = Client::new();
    let msg = check(&c, home.path(), &url(&stub), now, "1.0.0").unwrap();
    assert!(msg.starts_with("monocle 2.5.0 is available"));
    // Second run right after: cache is fresh now.
    check(&c, home.path(), &url(&stub), now + 1, "1.0.0");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_check_bumps_last_checked_and_keeps_old_version() {
    let stub = common::stub(|_, _, _, _| (500, "boom".into()));
    let home = tempfile::tempdir().unwrap();
    seed(home.path(), 5000, "1.5.0");
    let now = 5000 + 2 * DAY;
    let msg = check(&Client::new(), home.path(), &url(&stub), now, "1.0.0");
    assert!(msg.unwrap().contains("1.5.0"));
    let saved = std::fs::read_to_string(home.path().join(".monocle/update-check.json")).unwrap();
    assert!(
        saved.contains(&format!(r#""last_checked":{now}"#)),
        "{saved}"
    );
}

#[test]
fn unreachable_server_is_swallowed_and_backs_off() {
    let home = tempfile::tempdir().unwrap();
    // Port 9 (discard) on loopback: refused immediately.
    let msg = check(
        &Client::new(),
        home.path(),
        "http://127.0.0.1:9/x",
        777,
        "1.0.0",
    );
    assert!(msg.is_none());
    let saved = std::fs::read_to_string(home.path().join(".monocle/update-check.json")).unwrap();
    assert!(saved.contains(r#""last_checked":777"#), "{saved}");
}

#[test]
fn token_command_is_untouched_by_a_stale_cache() {
    let stub = common::stub(|_, _, _, _| (404, String::new()));
    let home = tempfile::tempdir().unwrap();
    Credentials::with_home(home.path())
        .write(&CredentialsData {
            tenant_domain: stub.addr.clone(),
            tenant_name: "t".into(),
            email: "e@example.com".into(),
            access_token: "tok".into(),
            refresh_token: "r".into(),
            id_token: "i".into(),
            access_token_expires_at: "2099-01-01T00:00:00Z".into(),
            refresh_token_expires_at: "2099-01-01T00:00:00Z".into(),
            router_url: Some(stub.router_url()),
        })
        .unwrap();
    seed(home.path(), 1, "0.0.1");
    let before = std::fs::read(home.path().join(".monocle/update-check.json")).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_monocle"))
        .arg("token")
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env_remove("MONOCLE_PROFILE")
        .env_remove("CI")
        .env_remove("MONOCLE_NO_UPDATE_CHECK")
        .current_dir(home.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "tok");
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // A check would have bumped last_checked.
    let after = std::fs::read(home.path().join(".monocle/update-check.json")).unwrap();
    assert_eq!(before, after);
}

const TTY_ENV: Env = Env {
    stderr_is_tty: true,
    ci: false,
    opted_out: false,
};

#[test]
fn token_upgrade_acp_excluded_by_logic_even_on_a_tty() {
    for name in ["token", "upgrade", "acp"] {
        assert!(!command_eligible(name), "{name}");
        assert!(!enabled_for(name, TTY_ENV), "{name}");
    }
    assert!(enabled_for("chat", TTY_ENV));
    assert!(!enabled_for(
        "chat",
        Env {
            ci: true,
            ..TTY_ENV
        }
    ));
}

#[test]
fn background_refresh_updates_cache_and_does_not_start_for_token() {
    let hits = Arc::new(AtomicUsize::new(0));
    let stub = release_stub("v2.5.0", hits.clone());
    let home = tempfile::tempdir().unwrap();
    seed(home.path(), 1, "1.0.0");

    let p = start_with("token", home.path(), TTY_ENV, url(&stub));
    finish_with(
        p,
        "token",
        home.path(),
        std::time::Duration::from_secs(2),
        TTY_ENV,
    );
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    let p = start_with("chat", home.path(), TTY_ENV, url(&stub));
    finish_with(
        p,
        "chat",
        home.path(),
        std::time::Duration::from_secs(5),
        TTY_ENV,
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let saved = std::fs::read_to_string(home.path().join(".monocle/update-check.json")).unwrap();
    assert!(saved.contains("2.5.0"), "{saved}");
}

#[test]
fn slow_server_does_not_delay_beyond_grace_and_still_backs_off() {
    let stub = common::stub(|_, _, _, _| {
        std::thread::sleep(std::time::Duration::from_millis(1500));
        (200, r#"{"tag_name":"v9.0.0"}"#.into())
    });
    let home = tempfile::tempdir().unwrap();
    let t = std::time::Instant::now();
    let p = start_with("chat", home.path(), TTY_ENV, url(&stub));
    finish_with(
        p,
        "chat",
        home.path(),
        std::time::Duration::from_millis(300),
        TTY_ENV,
    );
    assert!(
        t.elapsed() < std::time::Duration::from_millis(1000),
        "{:?}",
        t.elapsed()
    );
    // Backoff was written before the fetch, so the next run will not retry.
    assert!(home.path().join(".monocle/update-check.json").exists());
}

fn cache_file(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".monocle/update-check.json")
}

#[test]
fn corrupt_cache_is_stale_one_hit_and_rewritten_valid() {
    let cases: [&[u8]; 3] = [
        b"\xff\x00garbage{{",
        b"",
        br#"{"last_checked":"yesterday","latest_version":5}"#,
    ];
    for bytes in cases {
        let hits = Arc::new(AtomicUsize::new(0));
        let stub = release_stub("v2.5.0", hits.clone());
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".monocle")).unwrap();
        std::fs::write(cache_file(home.path()), bytes).unwrap();
        assert!(read_cache(home.path()).is_none());
        let msg = check(&Client::new(), home.path(), &url(&stub), 9000, "1.0.0");
        assert!(msg.unwrap().contains("2.5.0"));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        let c = read_cache(home.path()).unwrap();
        assert_eq!(c.latest_version.as_deref(), Some("2.5.0"));
    }
}

#[test]
fn write_is_atomic_replaces_and_leaves_no_temp_files() {
    let home = tempfile::tempdir().unwrap();
    seed(home.path(), 1, "1.0.0");
    let c = Cache {
        last_checked: 42,
        latest_version: Some("3.0.0".into()),
    };
    write_cache(home.path(), &c);
    assert_eq!(read_cache(home.path()), Some(c));
    let names: Vec<_> = std::fs::read_dir(home.path().join(".monocle"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["update-check.json"]);
}

#[test]
fn concurrent_writers_leave_a_parseable_cache() {
    let home = tempfile::tempdir().unwrap();
    std::thread::scope(|s| {
        for t in 0..8 {
            let h = home.path();
            s.spawn(move || {
                for i in 0..50 {
                    let c = Cache {
                        last_checked: t * 1000 + i,
                        latest_version: Some("9.9.9".into()),
                    };
                    write_cache(h, &c);
                    if let Some(r) = read_cache(h) {
                        assert_eq!(r.latest_version.as_deref(), Some("9.9.9"));
                    }
                }
            });
        }
    });
    assert!(read_cache(home.path()).is_some());
    let n = std::fs::read_dir(home.path().join(".monocle"))
        .unwrap()
        .count();
    assert_eq!(n, 1);
}
