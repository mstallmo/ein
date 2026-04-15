// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use futures::future::BoxFuture;
use tracing::{error, info};

// use crate::model_client::ModelClientSession;
// use crate::tools::ToolRegistry;
use crate::errors::AgentError;
use crate::model_clients::{FinishReason, Message, ModelClient, Role};
use crate::tools::AsyncTool;

use std::collections;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Session configuration
// ---------------------------------------------------------------------------

/// Per-session LLM configuration derived from the client's `SessionConfig`.
pub struct SessionParams {
    pub model: String,
    pub max_tokens: i32,
}

// ---------------------------------------------------------------------------
// Agent loop
// ---------------------------------------------------------------------------

/// Number of messages from the end of the history to always keep verbatim.
/// This covers the current tool-call cycle plus the most recent user prompt.
const KEEP_RECENT_MESSAGES: usize = 10;

/// Tool result content longer than this (in bytes) will be replaced with a
/// placeholder once it falls outside the `KEEP_RECENT_MESSAGES` window.
/// 2000 bytes ≈ 500 tokens — generous for small bash outputs, compresses
/// file reads and long command outputs.
const MAX_TOOL_RESULT_CHARS: usize = 2000;

pub type AgentResult<T> = Result<T, AgentError>;

// TODO: Fill out impl
pub enum AgentEvent {}

pub type AgentEventHandler = Arc<Box<dyn Fn(AgentEvent) -> BoxFuture<'static, ()> + Send + Sync>>;

pub struct AgentBuilder {
    num_recent_messages: usize,
    max_tool_result_chars: usize,
    event_handler: Option<AgentEventHandler>,
    model_client: Box<dyn ModelClient>,
    async_tools: collections::HashMap<String, Box<dyn AsyncTool>>,
    message_history: Vec<Message>,
}

impl AgentBuilder {
    pub fn new(client: impl ModelClient + 'static) -> Self {
        Self {
            num_recent_messages: KEEP_RECENT_MESSAGES,
            max_tool_result_chars: MAX_TOOL_RESULT_CHARS,
            event_handler: None,
            model_client: Box::new(client),
            async_tools: collections::HashMap::new(),
            message_history: Vec::new(),
        }
    }

    pub fn num_recent_messages(mut self, num: usize) -> Self {
        self.num_recent_messages = num;
        self
    }

    pub fn max_tool_result_chars(mut self, chars: usize) -> Self {
        self.max_tool_result_chars = chars;
        self
    }

    pub fn with_event_handler<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(AgentEvent) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.event_handler = Some(Arc::new(Box::new(move |event| {
            Box::pin(handler(event)) as BoxFuture<'static, ()>
        })));

        self
    }

    pub fn with_messsage_history(mut self, history: Vec<Message>) -> Self {
        self.message_history = history;

        self
    }

    pub fn add_async_tool(mut self, tool: impl AsyncTool + 'static) -> Self {
        self.async_tools
            .insert(tool.name().to_string(), Box::new(tool));

        self
    }

    pub fn build(self) -> Agent {
        Agent::new(
            self.num_recent_messages,
            self.max_tool_result_chars,
            self.model_client,
            self.event_handler,
            self.async_tools,
            self.message_history,
        )
    }
}

#[derive(Clone)]
pub struct Agent {
    num_recent_messages: usize,
    max_tool_result_chars: usize,
    model_client: Arc<Box<dyn ModelClient>>,
    event_handler: Option<AgentEventHandler>,
    async_tools: Arc<collections::HashMap<String, Box<dyn AsyncTool>>>,
    messages: Vec<Message>,
}

impl Agent {
    pub fn builder(client: impl ModelClient + 'static) -> AgentBuilder {
        AgentBuilder::new(client)
    }

    /// Runs the agent loop for one user turn.
    ///
    /// Sends `messages` to the LLM via the model client plugin, streams events
    /// back through `tx`, executes any requested tools, and loops until the model
    /// stops. The updated message history (including assistant turns and tool
    /// results) is written back into `messages` in place so the caller's
    /// conversation state stays current.
    pub async fn run(&mut self, prompt: Message) -> AgentResult<Message> {
        let mut cumulative_prompt = 0i32;
        let mut cumulative_completion = 0i32;
        // Count consecutive empty-stop turns so we can nudge the model when it
        // produces thinking tokens but no output, and bail out if it keeps failing.
        let mut empty_stop_retries = 0u32;
        const MAX_EMPTY_STOP_RETRIES: u32 = 1;
        self.messages.push(prompt);

        loop {
            self.truncate_old_tool_results();

            // info!(
            //     "[agent] sending {} messages to {} (max_tokens={})",
            //     self.messages.len(),
            //     model_session.params().model,
            //     model_session.params().max_tokens,
            // );

            let resp = match self.model_client.complete(&self.messages).await {
                Ok(r) => r,
                Err(e) => {
                    error!("[agent] model client error: {e}");

                    return Err(AgentError::ModelClient(e.to_string()));
                }
            };

            // Check for API-level error (e.g. 402 insufficient credits).
            if let Some(error_obj) = &resp.error {
                let msg = error_obj
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown API error");
                error!("[agent] api error: {msg}");

                return Err(AgentError::ModelClient(msg.to_string()));
            }

            // Extract and accumulate token usage from this response.
            if let Some(usage) = &resp.usage {
                info!(
                    "[agent] tokens this call: prompt={}, completion={}",
                    usage.prompt_tokens, usage.completion_tokens,
                );
                cumulative_prompt += usage.prompt_tokens;
                cumulative_completion += usage.completion_tokens;

                // self.broadcast_event(Event::TokenUsage(TokenUsage {
                //     prompt_tokens: cumulative_prompt,
                //     completion_tokens: cumulative_completion,
                //     total_tokens: cumulative_prompt + cumulative_completion,
                // }))
                // .await;
            }

            let choice = resp
                .choices
                .into_iter()
                .next()
                .ok_or(AgentError::ModelClient(
                    "Response contained no choices".to_string(),
                ))?;

            let content = choice
                .message
                .content
                .as_deref()
                .unwrap_or_default()
                .to_string();

            // Some models (e.g. gemma via Ollama) emit finish_reason="stop" even
            // when they include tool calls in the response.  Normalise: if the
            // message carries tool calls, treat it as ToolCalls regardless of the
            // finish_reason field.
            let has_tool_calls = choice
                .message
                .tool_calls
                .as_ref()
                .map(|tc| !tc.is_empty())
                .unwrap_or(false);

            // Clone tool_calls before moving the message so we can iterate
            // over them later while also pushing to messages.
            let tool_calls = choice.message.tool_calls.clone();

            // Append the assistant's reply to the running history immediately so
            // tool results added in the same iteration are correctly sequenced.
            self.messages.push(choice.message.clone());
            let effective_finish = if has_tool_calls {
                FinishReason::ToolCalls
            } else {
                choice.finish_reason
            };

            info!(
                "[agent] finish_reason={:?} (effective={:?})",
                choice.finish_reason, effective_finish
            );

            match effective_finish {
                FinishReason::ToolCalls => {
                    // Stream any accompanying text before executing tools.
                    if let Some(content) = &choice.message.content
                        && !content.is_empty()
                    {
                        // self.broadcast_event(Event::ContentDelta(ContentDelta { text: content }))
                        //     .await;
                    }

                    // if let Some(tool_calls) = &tool_calls {
                    //     for tool_call in tool_calls {
                    //         match tool_call {
                    //             ToolCall::Function { id, function, .. } => {
                    //                 println!("[agent] tool call: {} (id={})", function.name, id);

                    //                 // Notify the client that a tool is starting.
                    //                 self.broadcast_event(Event::ToolCallStart(ToolCallStart {
                    //                     tool_call_id: id.clone(),
                    //                     tool_name: function.name.clone(),
                    //                     arguments: function.arguments.clone(),
                    //                 }))
                    //                 .await;

                    //                 let (result_str, metadata) = ("test", "test");
                    //                 // let (result_str, metadata) =
                    //                 //     self.handle_tool_call(tool_registry, id, function).await;

                    //                 // Notify the client that the tool finished.
                    //                 self.broadcast_event(Event::ToolCallEnd(ToolCallEnd {
                    //                     tool_call_id: id.clone(),
                    //                     tool_name: function.name.clone(),
                    //                     result: result_str.clone(),
                    //                     metadata,
                    //                 }))
                    //                 .await;

                    //                 // Append the tool result so the LLM sees it on
                    //                 // the next iteration.
                    //                 messages.push(Message {
                    //                     role: Role::Tool,
                    //                     content: Some(result_str),
                    //                     tool_call_id: Some(id.clone()),
                    //                     tool_calls: None,
                    //                 });
                    //             }
                    //         }
                    //     }
                    // }

                    // Loop: send the updated history back to the LLM.
                }
                FinishReason::Stop => {
                    // self.broadcast_event(Event::AgentFinished(AgentFinished {
                    //     final_content: content,
                    // }))
                    // .await;
                    return Ok(choice.message);
                }
                FinishReason::Unsupported => {
                    let error_msg = "The model stopped with an unsupported finish reason. \
                                                    This model may not support tool calling.\n\n\
                                                    Try switching to a model that supports function calling \
                                                    (e.g. anthropic/claude-haiku-4-5) by setting `model` \
                                                    in ~/.ein/config.json."
                                                .to_string();
                    // self.broadcast_event(Event::AgentError(AgentError {
                    //     message: error_msg,
                    // }))
                    // .await;

                    return Err(AgentError::UnsupportedFinishReason(error_msg));
                }
            }
        }
    }
}

// Private methods
impl Agent {
    fn new(
        num_recent_messages: usize,
        max_tool_result_chars: usize,
        model_client: Box<dyn ModelClient>,
        event_handler: Option<AgentEventHandler>,
        async_tools: collections::HashMap<String, Box<dyn AsyncTool>>,
        messages: Vec<Message>,
    ) -> Self {
        Self {
            num_recent_messages,
            max_tool_result_chars,
            model_client: Arc::new(model_client),
            event_handler,
            async_tools: Arc::new(async_tools),
            messages,
        }
    }

    async fn broadcast_event(&self, event: AgentEvent) {
        if let Some(event_handler) = &self.event_handler {
            event_handler(event).await;
        }
    }

    /// Replaces the `content` of stale, large tool result messages with a compact
    /// placeholder so they no longer consume significant context budget.
    ///
    /// A message is eligible if:
    /// - its `role` is `"tool"`
    /// - it is more than `KEEP_RECENT_MESSAGES` positions from the end of `messages`
    /// - its `content` length exceeds `MAX_TOOL_RESULT_CHARS`
    fn truncate_old_tool_results(&mut self) {
        let len = self.messages.len();
        let truncate_before = len.saturating_sub(self.num_recent_messages);

        for msg in self.messages[..truncate_before].iter_mut() {
            if !matches!(msg.role, Role::Tool) {
                continue;
            }

            let content_len = msg.content.as_deref().map(|s| s.len()).unwrap_or(0);
            if content_len > self.max_tool_result_chars {
                msg.content = Some(format!("[Tool result truncated: {content_len} chars]"));
            }
        }
    }

    // TODO: Cleanup error/success handling here
    // async fn handle_tool_call(
    //     &self,
    //     tool_registry: &mut ToolRegistry,
    //     id: &str,
    //     function: &FunctionCall,
    // ) -> (String, String) {
    //     match tool_registry.get(function.name.as_str()) {
    //         Some(tool) => {
    //             match tool.enable_chunk_sender().await {
    //                 Ok(should_enable_chunk_sender) => {
    //                     if should_enable_chunk_sender && let Some(handler) = &self.event_handler {
    //                         tool.set_chunk_sender(handler.clone(), id.to_owned())
    //                     }
    //                 }
    //                 Err(err) => {
    //                     eprintln!("[agent] tool '{}' error: {err}", function.name);

    //                     return (format!("Error: {err}"), String::new());
    //                 }
    //             };

    //             match tool.call(id, &function.arguments).await {
    //                 Ok(res) => {
    //                     let meta = res
    //                         .metadata
    //                         .as_ref()
    //                         .map(|v| v.to_string())
    //                         .unwrap_or_default();

    //                     (res.content, meta)
    //                 }
    //                 Err(e) => {
    //                     eprintln!("[agent] tool '{}' error: {e}", function.name);

    //                     (format!("Error: {e}"), String::new())
    //                 }
    //             }
    //         }
    //         None => {
    //             eprintln!("[agent] unknown tool '{}'", function.name);

    //             (
    //                 format!("Error: tool '{}' not found", function.name),
    //                 String::new(),
    //             )
    //         }
    //     }
    // }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;

    use super::*;
    use crate::model_clients::{Choice, CompletionResponse};

    struct TestModelClient {
        response: CompletionResponse,
    }

    #[async_trait]
    impl ModelClient for TestModelClient {
        async fn complete(&self, _messages: &[Message]) -> anyhow::Result<CompletionResponse> {
            Ok(self.response.clone())
        }
    }

    fn default_test_client() -> TestModelClient {
        let res = CompletionResponse {
            choices: vec![Choice {
                index: None,
                finish_reason: FinishReason::Stop,
                message: Message {
                    role: Role::Assistant,
                    content: Some("assistant response".to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                },
            }],
            usage: None,
            error: None,
        };

        TestModelClient { response: res }
    }

    #[tokio::test]
    async fn test_run_agent() {
        let model_client = default_test_client();
        let mut agent = Agent::builder(model_client).build();

        let message = Message {
            role: Role::User,
            content: Some("user message".to_string()),
            tool_calls: None,
            tool_call_id: None,
        };

        let res = agent.run(message).await.unwrap();
        assert_eq!(res.role, Role::Assistant);
        assert_eq!(res.content, Some("assistant response".to_string()))
    }
}
