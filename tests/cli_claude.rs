//! `monocle claude` must hand the selected profile to the child so bare
//! `monocle token` calls inside the session (plugins, scripts) resolve the
//! same credentials as the session's apiKeyHelper.

#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use monocle_cli::credentials::{Credentials, CredentialsData};

#[test]
fn claude_child_gets_selected_profile() {
    let stub = common::stub(|_, _, _, _| (200, "{}".into()));
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

    // Fake `claude` that records the env it was launched with.
    let bin = home.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let out = home.path().join("child_profile");
    let fake = bin.join("claude");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf %s \"$MONOCLE_PROFILE\" > {}\n",
            out.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let status = Command::new(env!("CARGO_BIN_EXE_monocle"))
        .args(["--profile", "default", "claude"])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("PATH", path)
        .env_remove("MONOCLE_PROFILE")
        .current_dir(home.path())
        .status()
        .unwrap();

    assert!(status.success());
    assert_eq!(std::fs::read_to_string(out).unwrap(), "default");
}
