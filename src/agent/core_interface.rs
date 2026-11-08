//! In-process effect boundary for an experimental core integration.
//!
//! The host supplies both callbacks and retains provider, tool permission,
//! approval, cancellation and session ownership. This module does not select
//! or select a provider or tool. The CLI opts into its model-only route explicitly.

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
#[path = "../tests/core_interface.rs"]
mod tests;
