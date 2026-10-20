//! Stream and exit-code contract of `monocle status` and of help-by-omission:
//! `status` is a result (stdout) and exits 1 when not logged in; `monocle` and
//! a group without a subcommand print help to stdout and exit 0, while real
//! usage errors still go to stderr with exit 2.

use std::path::Path;
use std::process::{Command, Output};

use monocle_cli::credentials::{Credentials, CredentialsData};

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_monocle"))
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("MONOCLE_PROFILE")
        .env("MONOCLE_NO_UPDATE_CHECK", "1")
        .current_dir(home)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Non-JWT token + 2099 expiry → no refresh, no network.
fn logged_in_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    Credentials::with_home(home.path())
        .write(&CredentialsData {
            tenant_domain: "tenant.example.com".into(),
            tenant_name: "t".into(),
            email: "e@example.com".into(),
            access_token: "tok".into(),
            refresh_token: "r".into(),
            id_token: "i".into(),
            access_token_expires_at: "2099-01-01T00:00:00Z".into(),
            refresh_token_expires_at: "2099-01-01T00:00:00Z".into(),
            router_url: None,
        })
        .unwrap();
    home
}

#[test]
fn status_prints_its_block_to_stdout_and_exits_0_when_logged_in() {
    let home = logged_in_home();
    let o = run(home.path(), &["status"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    for line in [
        "Profile: ",
        "Environment: ",
        "Tenant: ",
        "User: e@example.com",
    ] {
        assert!(out.contains(line), "{line:?} missing from stdout: {out}");
    }
    assert!(out.contains("Access Token: Valid"), "{out}");
    assert!(out.contains("Claude Code: Not configured"), "{out}");
    assert!(o.stderr.is_empty(), "stderr: {}", text(&o.stderr));
}

#[test]
fn status_exits_1_when_not_logged_in_with_the_reason_on_stderr() {
    let home = tempfile::tempdir().unwrap();
    let o = run(home.path(), &["status"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stdout).contains("Profile: default"));
    assert!(
        text(&o.stderr).contains("Not logged in"),
        "{}",
        text(&o.stderr)
    );
    assert!(!text(&o.stdout).contains("Not logged in"));
}

#[test]
fn bare_command_and_groups_print_help_to_stdout_and_exit_0() {
    let home = tempfile::tempdir().unwrap();
    for args in [&[][..], &["craft"][..], &["knowledge"][..], &["mcp"][..]] {
        let o = run(home.path(), args);
        assert_eq!(o.status.code(), Some(0), "{args:?}: {}", text(&o.stderr));
        assert!(text(&o.stdout).contains("Usage: monocle"), "{args:?}");
        assert!(o.stderr.is_empty(), "{args:?}: {}", text(&o.stderr));
    }
}

#[test]
fn real_usage_errors_still_go_to_stderr_with_exit_2() {
    let home = tempfile::tempdir().unwrap();
    for args in [&["craft", "turn"][..], &["no-such-command"][..]] {
        let o = run(home.path(), args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(o.stdout.is_empty(), "{args:?}");
        assert!(text(&o.stderr).starts_with("error:"), "{args:?}");
    }
}
