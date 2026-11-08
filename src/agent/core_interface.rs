//! In-process effect boundary for an experimental core integration.
//!
//! The host supplies both callbacks and retains provider, tool permission,
//! approval, cancellation and session ownership. This module does not select
//! or invoke a provider or tool and is not wired into the native agent loop.

use agent_core_rust_sequence::sequencer::{self, EffectPort, EffectRequest, TurnFailure, TurnOutcome};
use serde_json::Value;

/// Adapts host-owned callbacks to the shared synchronous effect port.
pub(crate) struct HostPort<M, T> {
    model: M,
    tools: T,
}

impl<M, T> HostPort<M, T> {
    pub(crate) fn new(model: M, tools: T) -> Self { Self { model, tools } }
}

impl<M, T> EffectPort for HostPort<M, T>
where
    M: FnMut(Value) -> Result<Value, &'static str>,
    T: FnMut(Value) -> Result<Value, &'static str>,
{
    fn call(&mut self, request: EffectRequest) -> Result<Value, &'static str> {
        match request {
            EffectRequest::Model(payload) => (self.model)(payload),
            EffectRequest::Tools(payload) => (self.tools)(payload),
        }
    }
}

/// Runs a bounded turn with the explicitly supplied host port. A returned
/// failure does not prove that earlier external effects were rolled back.
pub(crate) fn run_turn<P: EffectPort + ?Sized>(
    input: Value,
    initial_response: Option<Value>,
    limit: u64,
    port: &mut P,
) -> Result<TurnOutcome, TurnFailure> {
    sequencer::sequence_with_port(input, initial_response, limit, port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core_rust_sequence::sequencer::FailurePhase;
    use serde_json::json;
    use std::cell::RefCell;

    #[test]
    fn model_only_preserves_payload_and_output() {
        let seen = RefCell::new(Vec::new());
        let mut port = HostPort::new(
            |payload| { seen.borrow_mut().push(payload); Ok(json!({"calls":[],"answer":"done"})) },
            |_| panic!("unexpected tool call"),
        );
        let outcome = run_turn(json!({"prompt":"p"}), None, 1, &mut port).unwrap();
        assert_eq!(outcome.terminal_model["answer"], "done");
        assert_eq!(seen.borrow()[0], json!({"step":0,"input":{"prompt":"p"},"previous":null}));
    }

    #[test]
    fn initial_response_routes_tools_to_host_before_next_model() {
        let order = RefCell::new(Vec::new());
        let mut port = HostPort::new(
            |payload: Value| {
                order.borrow_mut().push("model");
                assert_eq!(payload["previous"]["tools"], json!(["ok"]));
                Ok(json!({"calls":[]}))
            },
            |payload: Value| {
                order.borrow_mut().push("tools");
                assert_eq!(payload["calls"][0]["id"], "a");
                Ok(json!(["ok"]))
            },
        );
        let initial = json!({"calls":[{"id":"a","name":"fake","args":{}}]});
        assert!(run_turn(json!({}), Some(initial), 2, &mut port).is_ok());
        assert_eq!(*order.borrow(), ["tools", "model"]);
    }

    #[test]
    fn host_denial_is_returned_without_further_effects() {
        let mut port = HostPort::new(
            |_| panic!("unexpected model call"),
            |_| Err("denied"),
        );
        let initial = json!({"calls":[{"id":"a","name":"fake","args":{}}]});
        let failure = run_turn(json!({}), Some(initial), 2, &mut port).unwrap_err();
        assert_eq!(failure.phase, FailurePhase::Tools);
        assert_eq!(failure.reason, "denied");
        assert_eq!(failure.step, 0);
    }
}
