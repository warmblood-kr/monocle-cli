use crate::types::*;

/// Adapter around a host-owned native inference callback. It forwards the
/// complete request and the same cancellation reference, and preserves the
/// callback's response/error verbatim. `Agent` normalizes error phases.
pub struct MonocleLlmProvider<F> {
    infer: F,
}

impl<F> MonocleLlmProvider<F> {
    /// Store the callback without calling it.
    pub fn new(infer: F) -> Self {
        Self { infer }
    }
}

impl<F> LlmProvider for MonocleLlmProvider<F>
where
    F: FnMut(LlmRequest, &dyn Cancellation) -> Result<LlmResponse, AgentError>,
{
    fn infer(
        &mut self,
        request: LlmRequest,
        cancel: &dyn Cancellation,
    ) -> Result<LlmResponse, AgentError> {
        (self.infer)(request, cancel)
    }
}
