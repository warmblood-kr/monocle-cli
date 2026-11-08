    use super::*;
    use crate::agent::providers::{FunctionCall, Message, TokenUsage, ToolCall, ToolDef};
    use crate::error::AppError;
    use std::cell::RefCell;

    struct Fake {
        seen: RefCell<Vec<ChatRequest>>,
        reply: RefCell<Option<Result<ChatResponse>>>,
        cancel_on_call: Option<Cancel>,
    }
    impl LlmProvider for Fake {
        fn chat(&self, request: &ChatRequest) -> Result<ChatResponse> {
            self.seen.borrow_mut().push(request.clone());
            if let Some(cancel) = &self.cancel_on_call { cancel.cancel(); }
            self.reply.borrow_mut().take().expect("provider called twice")
        }
        fn chat_stream(&self, _: &ChatRequest, _: &mut dyn FnMut(&str)) -> Result<ChatResponse> {
            panic!("model-only lane must not stream")
        }
    }
    fn response() -> ChatResponse {
        ChatResponse {
            content: "answer".into(), tool_calls: vec![], model: Some("served".into()),
            finish_reason: Some("stop".into()), truncated: false,
            usage: Some(TokenUsage { prompt_tokens: 2, completion_tokens: 3, total_tokens: 5 }),
        }
    }
    fn fake(reply: Result<ChatResponse>) -> Fake {
        Fake { seen: RefCell::new(vec![]), reply: RefCell::new(Some(reply)), cancel_on_call: None }
    }
    fn request() -> ChatRequest {
        ChatRequest { model: "requested".into(), messages: vec![Message::user("hello")],
            max_tokens: Some(17), ..Default::default() }
    }
    #[test]
    fn one_nonstream_call_preserves_request_and_native_response() {
        let p = fake(Ok(response()));
        let req = request();
        let got = run_model_only(&p, &req, &Cancel::new()).unwrap();
        let seen = p.seen.borrow();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].model, "requested");
        assert_eq!(serde_json::to_value(&seen[0].messages).unwrap(), serde_json::to_value(&req.messages).unwrap());
        assert_eq!(seen[0].max_tokens, Some(17));
        assert!(seen[0].tools.is_empty() && seen[0].images.is_empty() && seen[0].files.is_empty());
        assert_eq!(got.content, "answer");
        assert_eq!(got.model.as_deref(), Some("served"));
        assert_eq!(got.finish_reason.as_deref(), Some("stop"));
        assert_eq!(got.usage, response().usage);
        assert!(!got.truncated && got.tool_calls.is_empty());
    }
    #[test]
    fn declared_tools_or_tool_history_fail_before_provider() {
        let p = fake(Ok(response()));
        let mut req = request();
        req.tools.push(ToolDef::function("write", "unsupported", serde_json::json!({})));
        assert_eq!(run_model_only(&p, &req, &Cancel::new()).err().unwrap().to_string(), "core model-only: unsupported request");
        req.tools.clear();
        req.messages.push(Message::tool("old", "result"));
        assert!(run_model_only(&p, &req, &Cancel::new()).is_err());
        assert!(p.seen.borrow().is_empty());
    }
    #[test]
    fn unexpected_tool_response_never_becomes_success_or_retry() {
        let mut r = response();
        r.tool_calls.push(ToolCall { id: "x".into(), kind: "function".into(),
            function: FunctionCall { name: "shell".into(), arguments: "not JSON".into() } });
        let p = fake(Ok(r));
        assert_eq!(run_model_only(&p, &request(), &Cancel::new()).err().unwrap().to_string(), "core model-only: unsupported response");
        assert_eq!(p.seen.borrow().len(), 1);
    }
    #[test]
    fn provider_error_is_preserved_without_retry() {
        let p = fake(Err(AppError::new("provider failed")));
        assert_eq!(run_model_only(&p, &request(), &Cancel::new()).err().unwrap().to_string(), "provider failed");
        assert_eq!(p.seen.borrow().len(), 1);
    }
    #[test]
    fn cancellation_before_or_after_provider_never_returns_answer() {
        let cancel = Cancel::new(); cancel.cancel();
        let p = fake(Ok(response()));
        assert_eq!(run_model_only(&p, &request(), &cancel).err().unwrap().to_string(), "core model-only: cancelled");
        assert!(p.seen.borrow().is_empty());
        let cancel = Cancel::new();
        let mut p = fake(Ok(response())); p.cancel_on_call = Some(cancel.clone());
        assert_eq!(run_model_only(&p, &request(), &cancel).err().unwrap().to_string(), "core model-only: cancelled");
        assert_eq!(p.seen.borrow().len(), 1);
    }
    #[test]
    fn attachments_and_other_tool_history_fail_before_provider() {
        use crate::agent::providers::{ImageAttachment, FileAttachment};
        let mut cases = Vec::new();
        let mut req = request(); req.images.push(ImageAttachment { url: "https://example.invalid/image".into() }); cases.push(req);
        let mut req = request(); req.files.push(FileAttachment { filename: "x".into(), content_type: "text/plain".into(), data: vec![] }); cases.push(req);
        let mut req = request(); req.messages[0].tool_call_id = Some("old".into()); cases.push(req);
        let mut req = request(); req.messages.push(Message::assistant_with_tool_calls("", vec![ToolCall {
            id: "old".into(), kind: "function".into(), function: FunctionCall { name: "shell".into(), arguments: "{}".into() }
        }])); cases.push(req);
        for req in cases {
            let p = fake(Ok(response()));
            assert_eq!(run_model_only(&p, &req, &Cancel::new()).err().unwrap().to_string(), "core model-only: unsupported request");
            assert!(p.seen.borrow().is_empty());
        }
    }
    #[test]
    fn truncated_fails_but_empty_terminal_content_succeeds() {
        let mut r = response(); r.truncated = true;
        let p = fake(Ok(r));
        assert_eq!(run_model_only(&p, &request(), &Cancel::new()).err().unwrap().to_string(), "core model-only: unsupported response");
        assert_eq!(p.seen.borrow().len(), 1);
        let mut r = response(); r.content.clear();
        let p = fake(Ok(r));
        assert_eq!(run_model_only(&p, &request(), &Cancel::new()).unwrap().content, "");
        assert_eq!(p.seen.borrow().len(), 1);
    }
    #[test]
    fn provider_error_has_precedence_over_post_call_cancellation() {
        let cancel = Cancel::new();
        let mut p = fake(Err(AppError::new("original error"))); p.cancel_on_call = Some(cancel.clone());
        assert_eq!(run_model_only(&p, &request(), &cancel).err().unwrap().to_string(), "original error");
        assert_eq!(p.seen.borrow().len(), 1);
    }
