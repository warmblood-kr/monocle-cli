//! Center-first acceptance for the explicit core route on current public devel.
//! Real CLI processes, native read_file and SessionStore; loopback model and
//! cleared temporary synthetic credentials. No production actor is reimplemented.
use monocle_cli::agent::providers::Message;
use monocle_cli::agent::session::SessionStore;
use monocle_cli::credentials::{Credentials, CredentialsData};
use serde_json::{json, Value};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tiny_http::{Header, Response, Server};

struct ModelFixture {
    address: String,
    server: Arc<Server>,
    requests: Arc<Mutex<Vec<Value>>>,
    auth: Arc<Mutex<Vec<Option<String>>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl ModelFixture {
    fn new() -> Self {
        Self::with_responses(vec![
            (
                200,
                completion(json!({
                    "role":"assistant", "content":"reading the original file",
                    "tool_calls":[{"id":"read-1","type":"function","function":{
                        "name":"read_file","arguments":"{\"path\":\"answer.txt\"}"
                    }}]
                })),
            ),
            (
                200,
                completion(json!({"role":"assistant","content":"first answer"})),
            ),
            (
                200,
                completion(json!({"role":"assistant","content":"resumed answer"})),
            ),
        ])
    }

    fn with_responses(responses: Vec<(u16, Value)>) -> Self {
        let server = Arc::new(Server::http("127.0.0.1:0").unwrap());
        let address = server.server_addr().to_ip().unwrap().to_string();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let auth = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let serving = server.clone();
        let seen = requests.clone();
        let headers = auth.clone();
        let stopping = stop.clone();
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(60);
            while !stopping.load(Ordering::SeqCst) && Instant::now() < deadline {
                let Ok(Some(mut request)) = serving.recv_timeout(Duration::from_millis(50)) else {
                    continue;
                };
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap();
                let index = {
                    let mut all = seen.lock().unwrap();
                    all.push(serde_json::from_str(&body).unwrap());
                    all.len() - 1
                };
                headers.lock().unwrap().push(
                    request
                        .headers()
                        .iter()
                        .find(|header| header.field.equiv("Authorization"))
                        .map(|header| header.value.as_str().to_owned()),
                );
                let valid =
                    request.method().as_str() == "POST" && request.url() == "/v1/chat/completions";
                let (status, response) = responses
                    .get(index)
                    .cloned()
                    .unwrap_or((500, json!({"error":"unexpected extra request"})));
                let _ = request.respond(
                    Response::from_string(response.to_string())
                        .with_status_code(if valid { status } else { 404 })
                        .with_header(
                            Header::from_bytes("Content-Type", "application/json").unwrap(),
                        ),
                );
            }
        });
        Self {
            address,
            server,
            requests,
            auth,
            stop,
            worker: Some(worker),
        }
    }
}

fn completion(message: Value) -> Value {
    let finish = if message.get("tool_calls").is_some() {
        "tool_calls"
    } else {
        "stop"
    };
    json!({"model":"fixture-model", "choices":[{"message":message,"finish_reason":finish}],
        "usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}})
}

impl Drop for ModelFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.server.unblock();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_selected_agent(home: &std::path::Path, workdir: &std::path::Path, prompt: &str) -> Output {
    run_agent(home, workdir, prompt, Some("core-named-contract"))
}

fn run_agent(
    home: &std::path::Path,
    workdir: &std::path::Path,
    prompt: &str,
    session: Option<&str>,
) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_monocle"));
    command.args([
        "agent",
        prompt,
        "--workdir",
        workdir.to_str().unwrap(),
        "--experimental-core-tool-workflow",
    ]);
    if let Some(session) = session {
        command.args(["--session", session]);
    }
    let mut child = command
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("CI", "1")
        .current_dir(workdir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "selected CLI process exceeded 20s: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn install_login(home: &std::path::Path, address: &str) {
    Credentials::with_home(home)
        .write(&CredentialsData {
            tenant_domain: address.into(),
            tenant_name: "loopback-fixture".into(),
            email: "test@example.invalid".into(),
            access_token: "synthetic-loopback-token".into(),
            refresh_token: "unused-refresh-token".into(),
            id_token: "unused-id-token".into(),
            access_token_expires_at: "2099-01-01T00:00:00Z".into(),
            refresh_token_expires_at: "2099-01-01T00:00:00Z".into(),
            router_url: Some(format!("http://{address}")),
        })
        .unwrap();
}

#[test]
fn selected_core_named_session_reads_file_and_resumes_committed_native_tool_result() {
    let fixture = ModelFixture::new();
    let home = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();
    let answer_file = workdir.path().join("answer.txt");
    std::fs::write(&answer_file, "original file contents").unwrap();
    Credentials::with_home(home.path())
        .write(&CredentialsData {
            tenant_domain: fixture.address.clone(),
            tenant_name: "loopback-fixture".into(),
            email: "test@example.invalid".into(),
            access_token: "synthetic-loopback-token".into(),
            refresh_token: "unused-refresh-token".into(),
            id_token: "unused-id-token".into(),
            access_token_expires_at: "2099-01-01T00:00:00Z".into(),
            refresh_token_expires_at: "2099-01-01T00:00:00Z".into(),
            router_url: Some(format!("http://{}", fixture.address)),
        })
        .unwrap();

    let first = run_selected_agent(home.path(), workdir.path(), "read the file");
    assert!(
        first.status.success(),
        "selected core named-session request must succeed: stdout={} stderr={}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(String::from_utf8_lossy(&first.stdout).contains("first answer"));
    let session_path = home.path().join(".monocle/agent/core-named-contract.jsonl");
    let reopened = SessionStore::new(&session_path).load().unwrap();
    assert_eq!(reopened.len(), 5);
    assert_eq!(
        reopened
            .iter()
            .map(|message| message.role.as_str())
            .collect::<Vec<_>>(),
        ["system", "user", "assistant", "tool", "assistant"]
    );
    assert_eq!(
        reopened[2].content.as_deref(),
        Some("reading the original file")
    );
    assert_eq!(reopened[2].tool_calls.len(), 1);
    assert_eq!(reopened[2].tool_calls[0].id, "read-1");
    assert_eq!(reopened[2].tool_calls[0].kind, "function");
    assert_eq!(reopened[2].tool_calls[0].function.name, "read_file");
    assert_eq!(
        reopened[2].tool_calls[0].function.arguments,
        "{\"path\":\"answer.txt\"}"
    );
    assert_eq!(reopened[3].tool_call_id.as_deref(), Some("read-1"));
    assert_eq!(
        reopened[3].content.as_deref(),
        Some("original file contents")
    );
    assert_eq!(reopened[4].content.as_deref(), Some("first answer"));

    std::fs::write(&answer_file, "changed after first process").unwrap();
    let second = run_selected_agent(home.path(), workdir.path(), "continue the answer");
    assert!(
        second.status.success(),
        "selected core resume must succeed: stdout={} stderr={}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(String::from_utf8_lossy(&second.stdout).contains("resumed answer"));
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(
        requests.len(),
        3,
        "first tool step, terminal, resumed terminal"
    );
    assert!(requests.iter().all(|request| request["stream"] == false));
    assert!(requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["function"]["name"] == "read_file"));
    assert_eq!(
        requests[1]["messages"][2],
        serde_json::to_value(&reopened[2]).unwrap()
    );
    assert_eq!(
        requests[1]["messages"][3],
        serde_json::to_value(&reopened[3]).unwrap()
    );
    let resumed = requests[2]["messages"].as_array().unwrap();
    assert_eq!(resumed.len(), 6);
    for (observed, saved) in resumed.iter().zip(&reopened) {
        assert_eq!(observed, &serde_json::to_value(saved).unwrap());
    }
    assert_eq!(
        resumed[5],
        serde_json::to_value(Message::user("continue the answer")).unwrap()
    );
    assert_eq!(
        *fixture.auth.lock().unwrap(),
        vec![Some("Bearer synthetic-loopback-token".into()); 3]
    );
    let saved = SessionStore::new(&session_path).load().unwrap();
    assert_eq!(saved.len(), 7);
    assert_eq!(
        saved
            .iter()
            .filter(|message| message.role == "system")
            .count(),
        1
    );
    assert_eq!(saved[3].content.as_deref(), Some("original file contents"));
    assert_eq!(saved[6].content.as_deref(), Some("resumed answer"));
    assert_eq!(
        std::fs::read_to_string(answer_file).unwrap(),
        "changed after first process"
    );
}

#[test]
fn selected_core_rejects_write_and_shell_before_effect_append_or_fallback() {
    let shell = if cfg!(windows) { "powershell" } else { "bash" };
    let shell_command = if cfg!(windows) {
        "Set-Content -Path sentinel.txt -Value changed"
    } else {
        "printf changed > sentinel.txt"
    };
    for (name, arguments) in [
        (
            "write_file",
            json!({"path":"sentinel.txt","content":"changed"}),
        ),
        (shell, json!({"command":shell_command})),
    ] {
        let fixture = ModelFixture::with_responses(vec![(
            200,
            completion(json!({
                "role":"assistant", "content":"must not execute",
                "tool_calls":[{"id":"unsupported-1","type":"function","function":{
                    "name":name,"arguments":arguments.to_string()
                }}]
            })),
        )]);
        let home = tempfile::tempdir().unwrap();
        let workdir = tempfile::tempdir().unwrap();
        install_login(home.path(), &fixture.address);
        let sentinel = workdir.path().join("sentinel.txt");
        std::fs::write(&sentinel, "unchanged").unwrap();
        let output = run_selected_agent(home.path(), workdir.path(), "unsupported mutation");
        assert!(!output.status.success(), "selected route must deny {name}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("tool was not disclosed"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("must not execute"));
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "unchanged");
        assert_eq!(
            fixture.requests.lock().unwrap().len(),
            1,
            "no continuation, fallback or retry"
        );
        let session = home.path().join(".monocle/agent/core-named-contract.jsonl");
        assert!(
            !session.exists(),
            "denied tool must not append a partial turn"
        );
        let requests = fixture.requests.lock().unwrap();
        let tools = requests[0]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "read_file");
    }
}

#[test]
fn selected_core_requires_named_session_before_provider_request() {
    let fixture = ModelFixture::with_responses(vec![(
        200,
        completion(json!({
            "role":"assistant","content":"unexpected provider call"
        })),
    )]);
    let home = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();
    install_login(home.path(), &fixture.address);
    let output = run_agent(home.path(), workdir.path(), "must not run", None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("requires a nonblank named session"));
    assert_eq!(fixture.requests.lock().unwrap().len(), 0);
    assert!(fixture.auth.lock().unwrap().is_empty());
    assert!(!home.path().join(".monocle/agent").exists());
}

#[test]
fn selected_core_preserves_native_provider_error_without_append_or_fallback() {
    let fixture = ModelFixture::with_responses(vec![(
        403,
        json!({
            "detail":{"code":"model_access_denied"}
        }),
    )]);
    let home = tempfile::tempdir().unwrap();
    let workdir = tempfile::tempdir().unwrap();
    install_login(home.path(), &fixture.address);
    let output = run_selected_agent(home.path(), workdir.path(), "denied model");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("API error: HTTP 403 (model_access_denied)"));
    assert!(output.stdout.is_empty());
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    assert!(!home
        .path()
        .join(".monocle/agent/core-named-contract.jsonl")
        .exists());
}
