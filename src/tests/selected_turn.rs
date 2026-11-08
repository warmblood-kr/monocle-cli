    use super::*;
    use crate::agent::providers::ChatResponse;
    use crate::error::AppError;
    use std::cell::Cell;

    struct Provider { calls: Cell<usize>, fail: bool }
    impl LlmProvider for Provider {
        fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
            self.calls.set(self.calls.get() + 1);
            assert!(req.tools.is_empty());
            if self.fail { return Err(AppError::new("provider failure")); }
            Ok(ChatResponse { content: "answer".into(), tool_calls: vec![], model: Some("served".into()),
                finish_reason: Some("stop".into()), truncated: false, usage: None })
        }
        fn chat_stream(&self, _: &ChatRequest, _: &mut dyn FnMut(&str)) -> Result<ChatResponse> {
            panic!("unexpected streaming call")
        }
    }
    #[derive(Default)]
    struct Events(Vec<String>);
    impl Observer for Events {
        fn on_turn_step(&mut self, r: &ChatResponse) { self.0.push(format!("step:{}", r.model.as_deref().unwrap())); }
        fn on_text_delta(&mut self, text: &str) { self.0.push(format!("text:{text}")); }
        fn on_notice(&mut self, _: &str) { panic!("unexpected notice"); }
        fn on_tool_call(&mut self, _: &str, _: &str, _: &serde_json::Value) { panic!("unexpected tool event"); }
        fn on_tool_result(&mut self, _: &str, _: &str, _: &crate::agent::tools::ToolOutcome) { panic!("unexpected tool result"); }
    }
    #[test]
    fn default_native_branch_calls_original_runner_once() {
        let p = Provider { calls: Cell::new(0), fail: false };
        let req = ChatRequest::default();
        let mut convo = vec![Message::user("q")]; let mut events = Events::default();
        let native_calls = Cell::new(0);
        let stop = run_selected_turn(false, &p, &req, &mut convo, &mut events, &Cancel::new(), |c,o| {
            native_calls.set(native_calls.get()+1); o.on_text_delta("native");
            c.push(Message::assistant("native")); Ok(RunStop::MaxSteps)
        }).unwrap();
        assert_eq!(stop, RunStop::MaxSteps); assert_eq!(native_calls.get(), 1);
        assert_eq!(p.calls.get(), 0); assert_eq!(events.0, ["text:native"]);
        assert_eq!(convo.len(), 2); assert_eq!(convo[1].content.as_deref(), Some("native"));
    }
    #[test]
    fn selected_core_uses_adapter_then_observer_and_single_assistant_append() {
        let p = Provider { calls: Cell::new(0), fail: false };
        let mut convo = vec![Message::user("q")];
        let req = ChatRequest { model: "requested".into(), messages: convo.clone(), ..Default::default() };
        let mut events = Events::default();
        let stop = run_selected_turn(true, &p, &req, &mut convo, &mut events, &Cancel::new(), |_,_| panic!("native fallback")).unwrap();
        assert_eq!(stop, RunStop::EndTurn); assert_eq!(p.calls.get(), 1);
        assert_eq!(events.0, ["step:served", "text:answer"]);
        assert_eq!(convo.len(), 2); assert_eq!(convo[1].content.as_deref(), Some("answer"));
        assert!(convo[1].tool_calls.is_empty());
    }
    #[test]
    fn selected_provider_error_leaves_host_messages_and_observer_untouched() {
        let p = Provider { calls: Cell::new(0), fail: true };
        let mut convo = vec![Message::user("q")]; let mut events = Events::default();
        let req = ChatRequest { messages: convo.clone(), ..Default::default() };
        let before = serde_json::to_value(&convo).unwrap();
        let err = run_selected_turn(true, &p, &req, &mut convo, &mut events, &Cancel::new(), |_,_| panic!("native fallback")).err().unwrap();
        assert_eq!(err.to_string(), "provider failure"); assert_eq!(p.calls.get(), 1);
        assert!(events.0.is_empty()); assert_eq!(serde_json::to_value(convo).unwrap(), before);
    }
    #[test]
    fn selected_history_preflight_rejects_tool_state_and_accepts_text() {
        assert!(validate_core_history(&[Message::system("guide"), Message::user("q"), Message::assistant("a")]).is_ok());
        assert!(validate_core_history(&[Message::tool("old", "result")]).is_err());
        let mut tagged = Message::assistant("a"); tagged.tool_call_id = Some("old".into());
        assert!(validate_core_history(&[tagged]).is_err());
        let call = crate::agent::providers::ToolCall { id:"old".into(), kind:"function".into(),
            function:crate::agent::providers::FunctionCall { name:"shell".into(), arguments:"{}".into() } };
        assert!(validate_core_history(&[Message::assistant_with_tool_calls("",vec![call])]).is_err());
    }
    #[test]
    fn buffered_route_has_no_streaming_ttfb_and_native_retains_it() {
        let started = std::time::Instant::now();
        let first = started + std::time::Duration::from_millis(7);
        assert_eq!(turn_ttfb(true, Some(first), started), None);
        assert_eq!(turn_ttfb(false, Some(first), started), Some(7));
        assert_eq!(turn_ttfb(false, None, started), None);
    }
    #[test]
    fn pre_cancel_and_unsolicited_tool_reply_never_emit_or_append() {
        let p = Provider { calls: Cell::new(0), fail:false };
        let cancel = Cancel::new(); cancel.cancel();
        let mut convo = vec![Message::user("q")];
        let req = ChatRequest { messages:convo.clone(), ..Default::default() };
        let mut events = Events::default();
        let error = run_selected_turn(true,&p,&req,&mut convo,&mut events,&cancel,|_,_|panic!("native fallback")).err().unwrap();
        assert_eq!(error.to_string(),"core model-only: cancelled");
        assert_eq!(p.calls.get(),0); assert_eq!(convo.len(),1); assert!(events.0.is_empty());
        struct ToolReply(Cell<usize>);
        impl LlmProvider for ToolReply {
            fn chat(&self, _: &ChatRequest) -> Result<ChatResponse> {
                self.0.set(self.0.get()+1);
                Ok(ChatResponse { content:"unapproved".into(), tool_calls:vec![crate::agent::providers::ToolCall {
                    id:"x".into(),kind:"function".into(),function:crate::agent::providers::FunctionCall { name:"shell".into(), arguments:"not JSON".into() }
                }], model:None,finish_reason:None,truncated:false,usage:None })
            }
        }
        let p=ToolReply(Cell::new(0));
        let error=run_selected_turn(true,&p,&req,&mut convo,&mut events,&Cancel::new(),|_,_|panic!("native fallback")).err().unwrap();
        assert_eq!(error.to_string(),"core model-only: unsupported response");
        assert_eq!(p.0.get(),1); assert_eq!(convo.len(),1); assert!(events.0.is_empty());
    }
    #[test]
    fn empty_terminal_content_appends_once_without_text_event() {
        struct Empty;
        impl LlmProvider for Empty {
            fn chat(&self, _: &ChatRequest) -> Result<ChatResponse> {
                Ok(ChatResponse { content:String::new(),tool_calls:vec![],model:Some("served".into()),finish_reason:Some("stop".into()),truncated:false,usage:None })
            }
        }
        let mut convo=vec![Message::user("q")];let mut events=Events::default();
        let req=ChatRequest {messages:convo.clone(),..Default::default()};
        assert_eq!(run_selected_turn(true,&Empty,&req,&mut convo,&mut events,&Cancel::new(),|_,_|panic!("native fallback")).unwrap(),RunStop::EndTurn);
        assert_eq!(events.0,["step:served"]);assert_eq!(convo.len(),2);assert_eq!(convo[1].content.as_deref(),Some(""));
    }
