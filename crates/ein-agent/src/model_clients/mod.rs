pub use ein_core::types::{CompletionRequest, CompletionResponse, Message, ToolDef};

use async_trait::async_trait;

#[async_trait]
pub trait ModelClient {
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> anyhow::Result<CompletionResponse>;
}
