use std::path::Path;

use monocle_cli::agent::providers::{FunctionCall, Message, ToolCall};
use monocle_cli::agent::session::{session_path, SessionStore};

#[test]
fn load_missing_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().join("s.jsonl"));
    assert!(store.load().unwrap().is_empty());
}

#[test]
fn append_then_load_round_trips_including_tool_messages() {
    let dir = tempfile::tempdir().unwrap();
    // Nested path also exercises parent-dir creation.
    let store = SessionStore::new(dir.path().join("nested/s.jsonl"));

    let msgs = vec![
        Message::system("sys"),
        Message::user("hi"),
        Message::assistant_with_tool_calls(
            "",
            vec![ToolCall {
                id: "c1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: "{\"path\":\"a\"}".into(),
                },
            }],
        ),
        Message::tool("c1", "file contents"),
        Message::assistant("done"),
    ];
    store.append(&msgs).unwrap();

    let loaded = store.load().unwrap();
    assert_eq!(loaded.len(), 5);
    assert_eq!(loaded[0].role, "system");
    assert_eq!(loaded[2].tool_calls[0].function.name, "read_file");
    assert_eq!(loaded[2].tool_calls[0].id, "c1");
    assert_eq!(loaded[3].role, "tool");
    assert_eq!(loaded[3].tool_call_id.as_deref(), Some("c1"));
    assert_eq!(loaded[4].content.as_deref(), Some("done"));
}

#[test]
fn append_accumulates() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::new(dir.path().join("s.jsonl"));
    store.append(&[Message::user("one")]).unwrap();
    store.append(&[Message::user("two")]).unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[1].content.as_deref(), Some("two"));
}

#[test]
fn load_tolerates_truncated_final_line() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    let store = SessionStore::new(&path);
    store
        .append(&[Message::user("one"), Message::user("two")])
        .unwrap();
    // Simulate a crash mid-write: an invalid, unterminated final line.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    write!(f, "{{\"role\":\"assist").unwrap();
    drop(f);

    let loaded = store.load().unwrap();
    assert_eq!(loaded.len(), 2, "good lines survive; corrupt tail dropped");
    assert_eq!(loaded[1].content.as_deref(), Some("two"));
}

#[test]
fn load_errors_on_corrupt_non_final_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.jsonl");
    std::fs::write(
        &path,
        "{\"role\":\"user\",\"content\":\"a\"}\nNOT JSON\n{\"role\":\"user\",\"content\":\"b\"}\n",
    )
    .unwrap();
    // A corrupt line in the middle is real corruption — not silently skipped.
    assert!(SessionStore::new(&path).load().is_err());
}

#[test]
fn session_path_is_under_monocle_agent() {
    let p = session_path(Path::new("/home/x"), "work");
    assert!(p.ends_with(".monocle/agent/work.jsonl"), "{}", p.display());
}
