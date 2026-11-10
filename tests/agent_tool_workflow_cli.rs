//! End-to-end consumer test for the explicit tool-workflow agent CLI route.
mod common;

use monocle_cli::credentials::{Credentials, CredentialsData};
use serde_json::{json, Value};
use std::process::Command;
use std::sync::{Arc, Mutex};

fn completion_tool_call() -> String {
    json!({
        "model": "served-model",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "checking the file",
                "tool_calls": [{
                    "id": "call-read-1",
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "arguments": "{\"path\":\"answer.txt\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 11, "completion_tokens": 3, "total_tokens": 14}
    })
    .to_string()
}

fn completion_terminal() -> String {
    json!({
        "model": "served-model",
        "choices": [{
            "message": {"role": "assistant", "content": "answer from loopback"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 17, "completion_tokens": 4, "total_tokens": 21}
    })
    .to_string()
}

#[test]
fn explicit_cli_tool_workflow_runs_native_provider_tool_and_session_path() {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen = requests.clone();
    let stub = common::stub(move |_addr, method, url, body| {
        assert_eq!(method, "POST");
        assert_eq!(url, "/v1/chat/completions");
        let request: Value = serde_json::from_str(body).unwrap();
        let call_index = {
            let mut all = seen.lock().unwrap();
            all.push(request);
            all.len()
        };
        match call_index {
            1 => (200, completion_tool_call()),
            2 => (200, completion_terminal()),
            _ => (500, "unexpected extra call".into()),
        }
    });

    let home = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();
    std::fs::write(
        workdir.path().join("answer.txt"),
        "file contents for workflow",
    )
    .unwrap();
    Credentials::with_home(home.path())
        .write(&CredentialsData {
            tenant_domain: stub.addr.clone(),
            tenant_name: "local-test".into(),
            email: "test@example.invalid".into(),
            access_token: "loopback-token".into(),
            refresh_token: "unused-refresh".into(),
            id_token: "unused-id".into(),
            access_token_expires_at: "2099-01-01T00:00:00Z".into(),
            refresh_token_expires_at: "2099-01-01T00:00:00Z".into(),
            router_url: Some(stub.router_url()),
        })
        .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_monocle"))
        .args([
            "agent",
            "hello",
            "--workdir",
            workdir.path().to_str().unwrap(),
            "--session",
            "workflow-e2e",
            "--experimental-core-tool-workflow",
        ])
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(workdir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "CLI failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("answer from loopback"));

    let seen = requests.lock().unwrap();
    assert_eq!(
        seen.len(),
        2,
        "one workflow tool step and one terminal step"
    );
    assert_eq!(seen[0]["stream"], false);
    assert_eq!(seen[0]["tools"][0]["function"]["name"], "read_file");
    assert_eq!(
        seen[1]["messages"].as_array().unwrap()[2]["content"],
        "checking the file",
        "assistant text from a tool-call step must survive transcript replay"
    );
    assert_eq!(
        seen[1]["messages"].as_array().unwrap().last().unwrap()["role"],
        "tool"
    );
    assert_eq!(
        seen[1]["messages"].as_array().unwrap().last().unwrap()["content"],
        "file contents for workflow"
    );
    assert_eq!(
        seen[1]["messages"].as_array().unwrap().last().unwrap()["tool_call_id"],
        "call-read-1"
    );

    let session_path = home.path().join(".monocle/agent/workflow-e2e.jsonl");
    assert!(
        !std::path::PathBuf::from(format!(
            "{}.core-tool-workflow-pending",
            session_path.display()
        ))
        .exists(),
        "successful JSONL commit clears dispatch journal"
    );
    let lines: Vec<Value> = std::fs::read_to_string(&session_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[0]["role"], "system");
    assert_eq!(
        lines.iter().filter(|line| line["role"] == "system").count(),
        1
    );
    assert_eq!(lines[1]["role"], "user");
    assert_eq!(lines[1]["content"], "hello");
    assert_eq!(lines[2]["content"], "checking the file");
    assert_eq!(lines[2]["content"], "checking the file");
    assert_eq!(lines[2]["tool_calls"][0]["id"], "call-read-1");
    assert_eq!(lines[3]["role"], "tool");
    assert_eq!(lines[4]["content"], "answer from loopback");
}
