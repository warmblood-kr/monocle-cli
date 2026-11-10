use std::process::Command;

#[test]
fn agent_auth_failure_keeps_exit_one_before_provider_turn() {
    let home = tempfile::tempdir().expect("isolated HOME");
    let output = Command::new(env!("CARGO_BIN_EXE_monocle"))
        .env_clear()
        .env("HOME", home.path())
        // CI suppresses the CLI's passive update check; it does not itself
        // isolate network access. Run this test inside a network-disabled sandbox.
        .env("CI", "1")
        .arg("agent")
        .arg("--experimental-core-model-only")
        .arg("--session")
        .arg("auth")
        .arg("synthetic prompt")
        .output()
        .expect("run isolated CLI subprocess");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("experimental")); // agent_command was reached
    assert!(stderr.contains("Not logged in")); // Repl::run_turn auth path exited
    assert!(output.stdout.is_empty());
    assert!(!home.path().join(".monocle/credentials.json").exists());
}
