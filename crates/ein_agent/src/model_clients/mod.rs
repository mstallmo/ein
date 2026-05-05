pub use ein_core::types::{CompletionRequest, CompletionResponse, Message, ToolDef};

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

    /// Release any resources held by this client (e.g. WASM store handles).
    async fn cleanup(mut self)
    where
        Self: Sized,
    {
    }
}
