use serde_json::json;

use monocle_cli::agent::tools::{ToolContext, ToolRegistry};

fn ctx() -> (tempfile::TempDir, ToolContext) {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolContext::new(dir.path());
    (dir, ctx)
}

#[test]
fn write_then_read_round_trips() {
    let (_d, ctx) = ctx();
    let reg = ToolRegistry::with_defaults();

    let w = reg.run(
        &ctx,
        "write_file",
        &json!({"path": "a.txt", "content": "hello"}),
    );
    assert!(!w.is_error, "{}", w.llm);

    let r = reg.run(&ctx, "read_file", &json!({"path": "a.txt"}));
    assert!(!r.is_error);
    assert_eq!(r.llm, "hello");
}

#[test]
fn read_file_is_not_side_effecting() {
    let reg = ToolRegistry::with_defaults();
    assert!(!reg.get("read_file").unwrap().is_side_effecting());
    assert!(reg.get("write_file").unwrap().is_side_effecting());
}

#[test]
fn edit_file_requires_unique_match() {
    let (_d, ctx) = ctx();
    let reg = ToolRegistry::with_defaults();
    reg.run(
        &ctx,
        "write_file",
        &json!({"path": "f.txt", "content": "x x"}),
    );

    // Non-unique → error.
    let dup = reg.run(
        &ctx,
        "edit_file",
        &json!({"path": "f.txt", "old_string": "x", "new_string": "y"}),
    );
    assert!(dup.is_error);
    assert!(dup.llm.contains("occurs 2 times"), "{}", dup.llm);

    // Unique → succeeds.
    let ok = reg.run(
        &ctx,
        "edit_file",
        &json!({"path": "f.txt", "old_string": "x x", "new_string": "z"}),
    );
    assert!(!ok.is_error, "{}", ok.llm);
    let r = reg.run(&ctx, "read_file", &json!({"path": "f.txt"}));
    assert_eq!(r.llm, "z");
}

#[test]
fn missing_argument_is_an_error_not_a_panic() {
    let (_d, ctx) = ctx();
    let reg = ToolRegistry::with_defaults();
    let r = reg.run(&ctx, "read_file", &json!({}));
    assert!(r.is_error);
    assert!(r.llm.contains("path"));
}

#[test]
fn unknown_tool_is_an_error() {
    let (_d, ctx) = ctx();
    let reg = ToolRegistry::with_defaults();
    let r = reg.run(&ctx, "nope", &json!({}));
    assert!(r.is_error);
    assert!(r.llm.contains("unknown tool"));
}

/// The shell tool is `bash` on unix / `powershell` on windows and runs in the
/// working directory.
#[test]
fn shell_runs_in_workdir() {
    let (_d, ctx) = ctx();
    let reg = ToolRegistry::with_defaults();

    #[cfg(not(windows))]
    let (tool, cmd) = ("bash", "echo agent-ran");
    #[cfg(windows)]
    let (tool, cmd) = ("powershell", "Write-Output agent-ran");

    assert!(
        reg.get(tool).is_some(),
        "shell tool `{tool}` should be registered"
    );
    let r = reg.run(&ctx, tool, &json!({ "command": cmd }));
    assert!(!r.is_error, "{}", r.llm);
    assert!(r.llm.contains("agent-ran"), "{}", r.llm);
    assert!(r.llm.contains("[exit code: 0]"), "{}", r.llm);
}

/// Dual channel (§9a): the model gets full content; the human gets a summary.
#[test]
fn read_file_splits_llm_and_ui_channels() {
    let (_d, ctx) = ctx();
    let reg = ToolRegistry::with_defaults();
    reg.run(
        &ctx,
        "write_file",
        &json!({"path": "a.txt", "content": "hello"}),
    );

    let r = reg.run(&ctx, "read_file", &json!({"path": "a.txt"}));
    assert_eq!(r.llm, "hello"); // model sees full content
    assert_ne!(r.ui_text(), r.llm); // human sees a distinct summary
    assert!(r.ui_text().contains("read a.txt"), "{}", r.ui_text());
    assert!(r.ui_text().contains("5 chars"), "{}", r.ui_text());
}

/// The shell's LLM channel is capped (re-sent every turn) while the UI stays concise.
#[test]
fn shell_llm_output_is_capped_ui_is_concise() {
    let (_d, ctx) = ctx();
    let reg = ToolRegistry::with_defaults();

    #[cfg(not(windows))]
    let (tool, cmd) = ("bash", "for i in $(seq 1 30000); do printf x; done");
    #[cfg(windows)]
    let (tool, cmd) = ("powershell", "Write-Output ('x' * 30000)");

    let r = reg.run(&ctx, tool, &json!({ "command": cmd }));
    assert!(!r.is_error, "{}", r.llm);
    assert!(
        r.llm.contains("characters omitted"),
        "llm output should be capped (len {})",
        r.llm.chars().count()
    );
    assert_eq!(r.ui_text(), "exit 0");
}
