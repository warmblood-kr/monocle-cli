mod common;

use common::stub_sse;
use monocle_cli::agent::providers::{ChatRequest, LlmProvider, Message, MonocleProvider};

fn req() -> ChatRequest {
    ChatRequest {
        model: "m".into(),
        messages: vec![Message::user("hi")],
        ..Default::default()
    }
}

#[test]
fn streams_content_deltas_and_assembles_final() {
    let s = stub_sse(|_a, _m, url, _b| {
        if !url.starts_with("/v1/chat/completions") {
            return (404, String::new());
        }
        let body = "\
data: {\"model\":\"m\",\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";
        (200, body.to_string())
    });
    let provider = MonocleProvider::new("tok", s.router_url());

    let mut deltas: Vec<String> = Vec::new();
    let resp = provider
        .chat_stream(&req(), &mut |d| deltas.push(d.to_string()))
        .unwrap();

    // Deltas arrive incrementally, and the final content is their assembly.
    assert_eq!(deltas, vec!["Hel", "lo"]);
    assert_eq!(resp.content, "Hello");
    assert_eq!(resp.finish_reason.as_deref(), Some("stop"));
    assert!(resp.tool_calls.is_empty());
}

#[test]
fn assembles_streamed_tool_call_from_fragments() {
    let s = stub_sse(|_a, _m, url, _b| {
        if !url.starts_with("/v1/chat/completions") {
            return (404, String::new());
        }
        // A tool call whose id/name come first, then `arguments` in fragments.
        let body = "\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"write_file\",\"arguments\":\"\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"a.txt\\\"}\"}}]}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
data: [DONE]\n\n";
        (200, body.to_string())
    });
    let provider = MonocleProvider::new("tok", s.router_url());

    let resp = provider.chat_stream(&req(), &mut |_d| {}).unwrap();

    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].id, "call_1");
    assert_eq!(resp.tool_calls[0].function.name, "write_file");
    assert_eq!(
        resp.tool_calls[0].function.arguments,
        "{\"path\":\"a.txt\"}"
    );
    assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
}
