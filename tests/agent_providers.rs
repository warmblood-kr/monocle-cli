mod common;

use common::stub;
use serde_json::Value;

use monocle_cli::agent::providers::{ChatRequest, LlmProvider, Message, MonocleProvider};

/// Stub that echoes back the requested model id, so tests can prove the provider
/// (a) passed the model through and (b) parsed the OpenAI-compatible response.
fn echo_model_stub() -> common::Stub {
    stub(|_addr, _method, url, body| {
        if url.starts_with("/v1/chat/completions") {
            let model = serde_json::from_str::<Value>(body)
                .ok()
                .and_then(|v| v["model"].as_str().map(String::from))
                .unwrap_or_default();
            (
                200,
                format!(
                    r#"{{"model":"{model}","choices":[{{"message":{{"role":"assistant","content":"echo:{model}"}},"finish_reason":"stop"}}]}}"#
                ),
            )
        } else {
            (404, String::new())
        }
    })
}

#[test]
fn non_streaming_turn_returns_assistant_content() {
    let s = echo_model_stub();
    let provider = MonocleProvider::new("tok", s.router_url());

    let resp = provider
        .chat(&ChatRequest {
            model: "claude-sonnet-4-6".into(),
            messages: vec![Message::system("be terse"), Message::user("hi")],
            max_tokens: Some(64),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(resp.content, "echo:claude-sonnet-4-6");
    assert_eq!(resp.model.as_deref(), Some("claude-sonnet-4-6"));
    assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
}

/// G1 — model-choice freedom: the SAME provider/loop routes to different models
/// (different vendors) just by changing the model id. Vendor-agnostic.
#[test]
fn same_provider_swaps_model_vendor_agnostically() {
    let s = echo_model_stub();
    let provider = MonocleProvider::new("tok", s.router_url());

    for model in ["claude-sonnet-4-6", "gpt-4o", "gemini-2.5-pro"] {
        let resp = provider
            .chat(&ChatRequest {
                model: model.into(),
                messages: vec![Message::user("hi")],
                max_tokens: None,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(resp.content, format!("echo:{model}"));
        assert_eq!(resp.model.as_deref(), Some(model));
    }
}

#[test]
fn non_200_is_an_error() {
    let s = stub(|_a, _m, _u, _b| (500, "boom".to_string()));
    let provider = MonocleProvider::new("tok", s.router_url());

    let err = provider
        .chat(&ChatRequest {
            model: "any".into(),
            messages: vec![Message::user("hi")],
            max_tokens: None,
            ..Default::default()
        })
        .unwrap_err()
        .to_string();
    assert!(err.contains("API error 500"), "got: {err}");
}
