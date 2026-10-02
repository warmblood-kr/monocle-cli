//! End-to-end coverage for non-image `--file` attachments going through
//! `--responses` (`ResponsesClient::respond`): upload to jarvice's real file
//! endpoint first, then reference the returned id as an `input_file` block
//! in `/api/responses`. Uses this repo's own stub HTTP server
//! (`tests/common/mod.rs`) — no live network, no mocking crate.

mod common;

use std::sync::{Arc, Mutex};

use serde_json::Value;

use common::stub;
use monocle_cli::agent::providers::FileAttachment;
use monocle_cli::net::Client;
use monocle_cli::responses_api::ResponsesClient;

#[test]
fn non_image_file_is_uploaded_then_attached_as_input_file() {
    // Captures the `/api/responses` request body so the test can assert on
    // the `input` array it built — the actual JSON-over-the-wire proof that
    // `respond()` wired the uploaded file id through to `build_input`.
    let captured_body: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_body_for_handler = captured_body.clone();
    // Also capture the upload request itself, to prove the multipart POST
    // actually happened (not just that the id magically appears).
    let upload_seen: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let upload_seen_for_handler = upload_seen.clone();

    let s = stub(move |_addr, method, url, body| {
        if url.starts_with("/api/v1/files/") {
            assert_eq!(method, "POST");
            *upload_seen_for_handler.lock().unwrap() = true;
            (200, r#"{"id":"fake-file-id-123"}"#.to_string())
        } else if url.starts_with("/api/responses") {
            *captured_body_for_handler.lock().unwrap() = Some(body.to_string());
            (
                200,
                r#"{"choices":[{"message":{"content":"here's what's in it"}}]}"#.to_string(),
            )
        } else {
            (404, String::new())
        }
    });

    let client = Client::new();
    let rc = ResponsesClient::new(&client, "test-token", s.router_url());
    let file = FileAttachment {
        filename: "notes.csv".to_string(),
        content_type: "text/csv".to_string(),
        data: b"a,b\n1,2\n".to_vec(),
    };

    let reply = rc
        .respond("m", "what's in this file?", &[], &[file], None, &[])
        .expect("respond should succeed");
    assert_eq!(reply.content, "here's what's in it");
    assert!(*upload_seen.lock().unwrap(), "file upload was never sent");

    let body_str = captured_body
        .lock()
        .unwrap()
        .clone()
        .expect("/api/responses body should have been captured");
    let body: Value = serde_json::from_str(&body_str).unwrap();
    let input = body["input"]
        .as_array()
        .expect("input should be a content-block array once a file is attached");
    let file_block = input
        .iter()
        .find(|part| part["type"] == "input_file")
        .expect("input array should contain an input_file block");
    assert_eq!(file_block["file_id"], "fake-file-id-123");
}

#[test]
fn upload_failure_surfaces_a_clear_error_and_never_calls_responses() {
    let responses_called: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let responses_called_for_handler = responses_called.clone();

    let s = stub(move |_addr, _method, url, _body| {
        if url.starts_with("/api/v1/files/") {
            (500, "upload broke".to_string())
        } else if url.starts_with("/api/responses") {
            *responses_called_for_handler.lock().unwrap() = true;
            (
                200,
                r#"{"choices":[{"message":{"content":"x"}}]}"#.to_string(),
            )
        } else {
            (404, String::new())
        }
    });

    let client = Client::new();
    let rc = ResponsesClient::new(&client, "test-token", s.router_url());
    let file = FileAttachment {
        filename: "notes.csv".to_string(),
        content_type: "text/csv".to_string(),
        data: b"a,b".to_vec(),
    };

    let err = match rc.respond("m", "check this", &[], &[file], None, &[]) {
        Ok(_) => panic!("expected the upload failure to surface as an error"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("File upload failed"), "got: {err}");
    assert!(err.contains("500"), "got: {err}");
    assert!(
        !*responses_called.lock().unwrap(),
        "must not call /api/responses when the upload itself failed"
    );
}

#[test]
fn missing_id_in_upload_response_is_a_clear_error() {
    let s = stub(move |_addr, _method, url, _body| {
        if url.starts_with("/api/v1/files/") {
            (200, r#"{"status":"ok"}"#.to_string())
        } else {
            (404, String::new())
        }
    });

    let client = Client::new();
    let rc = ResponsesClient::new(&client, "test-token", s.router_url());
    let file = FileAttachment {
        filename: "notes.csv".to_string(),
        content_type: "text/csv".to_string(),
        data: b"a,b".to_vec(),
    };

    let err = match rc.respond("m", "check this", &[], &[file], None, &[]) {
        Ok(_) => panic!("expected the missing-id response to surface as an error"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("missing"), "got: {err}");
    assert!(err.contains("\"id\""), "got: {err}");
}
