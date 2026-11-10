use crate::types::*;

/// Identity preparation/finalization. It preserves opaque native JSON values
/// and does not execute tools, load history, or perform I/O.
#[derive(Debug, Default)]
pub struct DefaultChatTurnManager;

impl ChatTurnManager for DefaultChatTurnManager {
    fn prepare(&mut self, input: TurnInput, system_prompt: &str) -> Result<LlmRequest, AgentError> {
        Ok(LlmRequest {
            turn_id: input.turn_id,
            system_prompt: system_prompt.to_owned(),
            input: input.input,
            tools: Vec::new(),
        })
    }

    fn finish(&mut self, turn_id: &str, response: LlmResponse) -> Result<TurnRecord, AgentError> {
        Ok(TurnRecord {
            turn_id: turn_id.to_owned(),
            response: response.payload,
        })
    }
}
