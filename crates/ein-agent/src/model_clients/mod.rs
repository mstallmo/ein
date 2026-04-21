pub use ein_core::types::{CompletionRequest, CompletionResponse, Message, ToolDef};

use async_trait::async_trait;

#[async_trait]
pub trait ModelClient {
    async fn complete(
        &mut self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> anyhow::Result<CompletionResponse>;

    async fn cleanup(mut self)
    where
        Self: Sized,
    {
    }
}
