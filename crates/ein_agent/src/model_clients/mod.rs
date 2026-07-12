pub use ein_core::types::{CompletionRequest, CompletionResponse, Message, ToolDef};

use crate::AgentEventHandler;
use async_trait::async_trait;

/// Async interface for sending a completion request to an LLM.
///
/// The agent loop holds a `Box<dyn ModelClient>` (or a concrete type) and
/// calls [`complete`](Self::complete) on each turn. Implement this trait to
/// connect Ein's agent loop to a new model provider without touching the
/// server-side WASM machinery.
#[async_trait]
pub trait ModelClient {
    /// Send the current conversation history and available tools to the model
    /// and return its response.
    async fn complete(
        &mut self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> anyhow::Result<CompletionResponse>;

    /// Register the event sink the client emits streaming events through — e.g.
    /// a `ContentDelta` per chunk as tokens arrive. Mirrors
    /// [`ToolSet::set_event_handler`](crate::tools::ToolSet::set_event_handler);
    /// the agent builder wires it. A non-streaming client can ignore it (the
    /// default) and its text is surfaced by the caller at turn end instead.
    fn set_event_handler(&mut self, _handler: AgentEventHandler) {}

    /// Whether the **most recent** [`complete`](Self::complete) streamed its
    /// assistant text through the event handler. When `true`, the agent loop
    /// must not re-broadcast that text as a `ContentDelta` — it already went out
    /// incrementally, and doing so would double it in the client.
    fn content_streamed(&self) -> bool {
        false
    }

    /// Release any resources held by this client (e.g. WASM store handles).
    async fn cleanup(mut self)
    where
        Self: Sized,
    {
    }
}
