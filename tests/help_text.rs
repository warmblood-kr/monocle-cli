//! Help text and wording rules (PR1): groups, wrapper help, pipe hint, no product names.

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    let home = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_monocle"))
        .args(args)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env_remove("MONOCLE_PROFILE")
        .current_dir(home.path())
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn root_help_has_the_five_groups_in_order() {
    let h = out(&run(&["--help"]));
    let at = |s: &str| h.find(s).unwrap_or_else(|| panic!("missing {s}: {h}"));
    let order = [
        "Auth & profile:",
        "Chat & agents:",
        "Workspace data:",
        "Router:",
        "Maintenance:",
    ];
    assert!(order.windows(2).all(|w| at(w[0]) < at(w[1])), "{h}");
    assert!(h.contains("-v, --verbose"), "{h}");
}

#[test]
fn no_internal_product_names_in_help() {
    for args in [
        vec!["--help"],
        vec!["chat", "--help"],
        vec!["craft", "turn", "--help"],
        vec!["craft", "session", "delegate", "--help"],
        vec!["whoami", "--help"],
        vec!["login", "--help"],
    ] {
        let h = out(&run(&args)).to_lowercase();
        assert!(
            !h.contains("jarvice") && !h.contains("stark"),
            "{args:?}: {h}"
        );
    }
}

#[test]
fn examples_blocks_exist() {
    for args in [
        vec!["chat", "--help"],
        vec!["knowledge", "ask", "--help"],
        vec!["schedule", "create", "--help"],
        vec!["mcp", "exec", "--help"],
        vec!["craft", "turn", "--help"],
    ] {
        assert!(out(&run(&args)).contains("Examples:"), "{args:?}");
    }
}

#[test]
fn claude_help_is_the_monocle_wrapper_help() {
    let h = out(&run(&["claude", "--help"]));
    assert!(h.contains("passed to the `claude` binary unchanged"), "{h}");
}

#[test]
fn id_arguments_are_described() {
    let h = out(&run(&["workspace-model", "show", "--help"]));
    assert!(h.contains("Workspace model ID"), "{h}");
    let h = out(&run(&["craft", "session", "show", "--help"]));
    assert!(h.contains("Session ID"), "{h}");
}

#[test]
fn chat_with_prompt_argument_hints_at_stdin() {
    let o = run(&["chat", "say ok"]);
    assert_eq!(o.status.code(), Some(2));
    let e = String::from_utf8_lossy(&o.stderr).into_owned();
    assert!(e.contains(r#"echo "hi" | monocle chat"#), "{e}");
}
