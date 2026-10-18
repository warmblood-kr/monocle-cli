//! Binary e2e for the `router` group and its deprecated root aliases: the
//! alias and the new form must share one code path (same stdout/files/exit
//! code) and the alias must print exactly one warning line on stderr.

mod common;

use std::path::Path;
use std::process::{Command, Output};

use monocle_cli::credentials::{Credentials, CredentialsData};

/// `HOME` with default-profile credentials pointing at the stub. A non-JWT
/// token plus a 2099 expiry means no refresh is attempted.
fn logged_in_home(stub: &common::Stub) -> tempfile::TempDir {
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
    home
}

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_monocle"))
        .args(args)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("MONOCLE_PROFILE")
        .current_dir(home)
        .output()
        .unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn warning(old: &str, new: &str) -> String {
    format!("Warning: `monocle {old}` is deprecated. Use `monocle {new}` instead.")
}

/// Exactly one deprecation line on stderr, equal to `expected`; none on stdout.
fn assert_one_warning(o: &Output, expected: &str) {
    let err = stderr(o);
    assert_eq!(err.matches("is deprecated").count(), 1, "stderr: {err}");
    assert!(err.contains(expected), "stderr: {err}");
    assert!(!stdout(o).contains("Warning:"));
}

#[test]
fn models_alias_stdout_identical_to_router_models() {
    let stub = common::stub(|_, method, url, _| match (method, url) {
        ("GET", "/v1/models") => (200, r#"{"data":[{"id":"m1","modality":"chat"}]}"#.into()),
        _ => (404, String::new()),
    });
    let home = logged_in_home(&stub);

    let alias = run(home.path(), &["models"]);
    let new = run(home.path(), &["router", "models"]);

    assert_eq!(alias.status.code(), Some(0), "{}", stderr(&alias));
    assert_eq!(new.status.code(), Some(0), "{}", stderr(&new));
    assert_eq!(alias.stdout, new.stdout);
    assert!(stdout(&new).contains("m1"));
    assert_one_warning(&alias, &warning("models", "router models"));
    assert!(!stderr(&new).contains("Warning:"));
}

#[test]
fn audio_speech_alias_writes_identical_file() {
    let stub = common::stub(|_, method, url, _| match (method, url) {
        ("POST", "/v1/audio/speech") => (200, "FAKEAUDIO".into()),
        _ => (404, String::new()),
    });
    let home = logged_in_home(&stub);

    let alias = run(home.path(), &["audio", "speech", "hi", "-o", "a.mp3"]);
    let new = run(
        home.path(),
        &["router", "audio", "speak", "hi", "-o", "b.mp3"],
    );

    assert_eq!(alias.status.code(), Some(0), "{}", stderr(&alias));
    assert_eq!(new.status.code(), Some(0), "{}", stderr(&new));
    let a = std::fs::read(home.path().join("a.mp3")).unwrap();
    let b = std::fs::read(home.path().join("b.mp3")).unwrap();
    assert_eq!(a, b);
    assert_eq!(a, b"FAKEAUDIO");
    assert_one_warning(&alias, &warning("audio speech", "router audio speak"));
    assert!(!stderr(&new).contains("Warning:"));
}

#[test]
fn image_generate_alias_matches_router() {
    // `write_images` wants `{data:[{b64_json}]}`; "RkFLRUlNRw==" is "FAKEIMG".
    let stub = common::stub(|_, method, url, _| match (method, url) {
        ("POST", "/v1/images/generations") => {
            (200, r#"{"data":[{"b64_json":"RkFLRUlNRw=="}]}"#.into())
        }
        _ => (404, String::new()),
    });
    let home = logged_in_home(&stub);

    let alias = run(
        home.path(),
        &["image", "generate", "a cat", "--model", "m", "-o", "a.png"],
    );
    let new = run(
        home.path(),
        &[
            "router", "image", "generate", "a cat", "--model", "m", "-o", "b.png",
        ],
    );

    assert_eq!(alias.status.code(), Some(0), "{}", stderr(&alias));
    assert_eq!(new.status.code(), Some(0), "{}", stderr(&new));
    let a = std::fs::read(home.path().join("a.png")).unwrap();
    let b = std::fs::read(home.path().join("b.png")).unwrap();
    assert_eq!(a, b);
    assert_eq!(a, b"FAKEIMG");
    assert_eq!(alias.stdout, new.stdout);
    assert_one_warning(&alias, &warning("image generate", "router image generate"));
    assert!(!stderr(&new).contains("Warning:"));
}

#[test]
fn alias_exit_code_matches_when_not_logged_in() {
    let home = tempfile::tempdir().unwrap();

    let alias = run(home.path(), &["models"]);
    let new = run(home.path(), &["router", "models"]);

    assert_eq!(alias.status.code(), Some(1));
    assert_eq!(new.status.code(), Some(1));
    assert!(alias.stdout.is_empty());
    assert!(new.stdout.is_empty());
    let err = stderr(&alias);
    let mut lines = err.lines();
    assert_eq!(
        lines.next(),
        Some(warning("models", "router models").as_str())
    );
    assert!(
        lines.next().unwrap_or("").starts_with("Not logged in"),
        "{err}"
    );
}

#[test]
fn unknown_profile_after_router_subcommand() {
    let home = tempfile::tempdir().unwrap();
    for args in [
        &["router", "models", "--profile", "nope"][..],
        &["--profile", "nope", "router", "models"],
    ] {
        let o = run(home.path(), args);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        assert!(stderr(&o).contains("Unknown profile 'nope'"), "{args:?}");
    }
}

#[test]
fn removed_commands_exit_2() {
    let home = tempfile::tempdir().unwrap();
    for args in [&["model", "list"][..], &["handoff"]] {
        let o = run(home.path(), args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(o.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn root_help_shows_router_hides_aliases() {
    let home = tempfile::tempdir().unwrap();
    let o = run(home.path(), &["--help"]);
    assert_eq!(o.status.code(), Some(0));
    let out = stdout(&o);
    assert!(out.contains("    router   "), "{out}");
    for hidden in ["handoff", "    models   ", "  Audio:", "  Image:"] {
        assert!(!out.contains(hidden), "root help must not list {hidden:?}");
    }
    assert_eq!(
        run(home.path(), &["models", "--help"]).status.code(),
        Some(0)
    );
}
