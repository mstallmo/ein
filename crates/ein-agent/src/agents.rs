// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use ein_core::types::{FinishReason, FunctionCall, Message, Role, ToolCall};
use futures::future::BoxFuture;
use tracing::{error, info};

use crate::errors::{AgentError, ToolError};
use crate::model_clients::ModelClient;
use crate::tools::{NativeToolSet, Tool, ToolSet};

use std::mem;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Session configuration
// ---------------------------------------------------------------------------

/// Per-session LLM configuration.
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
pub type ToolResult<T> = Result<T, ToolError>;
pub type AgentEventHandler = Arc<dyn Fn(AgentEvent) -> BoxFuture<'static, ()> + Send + Sync>;

#[derive(Debug, Clone)]
pub enum AgentEvent {
    ContentDelta(String),
    TokenUsage {
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
    },
    ToolCallStart {
        tool_call_id: String,
        tool_name: String,
        arguments: String,
    },
    ToolOutputChunk {
        tool_call_id: String,
        output: String,
    },
    ToolCallEnd {
        tool_call_id: String,
        tool_name: String,
        result: String,
        metadata: String,
    },
}

pub struct AgentBuilder<MC: ModelClient, TS: ToolSet> {
    num_recent_messages: usize,
    max_tool_result_chars: usize,
    event_handler: Option<AgentEventHandler>,
    model_client: MC,
    tools: TS,
    message_history: Vec<Message>,
}

impl<MC: ModelClient, TS: ToolSet> AgentBuilder<MC, TS> {
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
        self.event_handler = Some(Arc::new(move |event| {
            Box::pin(handler(event)) as BoxFuture<'static, ()>
        }));

        self
    }

    pub fn with_message_history(mut self, history: Vec<Message>) -> Self {
        self.message_history = history;

        self
    }

    pub fn build(mut self) -> Agent<MC, TS> {
        if let Some(handler) = &self.event_handler {
            self.tools.set_event_handler(handler.clone());
        }

        Agent::new(
            self.num_recent_messages,
            self.max_tool_result_chars,
            self.model_client,
            self.event_handler,
            self.tools,
            self.message_history,
        )
    }
}

impl<MC: ModelClient> AgentBuilder<MC, NativeToolSet> {
    pub fn add_tool(mut self, tool: impl Tool + 'static) -> Self {
        self.tools.insert(tool);

        self
    }
}

#[derive(Clone)]
pub struct Agent<MC: ModelClient, TS: ToolSet> {
    num_recent_messages: usize,
    max_tool_result_chars: usize,
    model_client: MC,
    event_handler: Option<AgentEventHandler>,
    tools: TS,
    messages: Vec<Message>,
}

impl<MC: ModelClient, TS: ToolSet> Agent<MC, TS> {
    /// Creates a builder using a custom [`ToolSet`]. Use this when you need
    /// full control over tool execution (e.g. a WASM-backed tool set).
    pub fn builder_with_tool_set(client: MC, tool_set: TS) -> AgentBuilder<MC, TS> {
        AgentBuilder {
            num_recent_messages: KEEP_RECENT_MESSAGES,
            max_tool_result_chars: MAX_TOOL_RESULT_CHARS,
            event_handler: None,
            model_client: client,
            tools: tool_set,
            message_history: Vec::new(),
        }
    }

    pub async fn replace_model_client(&mut self, model_client: MC)
    where
        MC: Send + 'static,
    {
        let old_client = mem::replace(&mut self.model_client, model_client);
        old_client.cleanup().await
    }

    pub fn messages(&self) -> &Vec<Message> {
        &self.messages
    }

    /// Runs the agent loop for one user turn.
    ///
    /// Sends `messages` to the LLM via the model client plugin, streams events
    /// back through `tx`, executes any requested tools, and loops until the model
    /// stops. The updated message history (including assistant turns and tool
    /// results) is written back into `messages` in place so the caller's
    /// conversation state stays current.
    pub async fn chat(&mut self, prompt: impl ToString) -> AgentResult<Message> {
        let mut cumulative_prompt = 0;
        let mut cumulative_completion = 0;

        let message = Message {
            role: Role::User,
            content: Some(prompt.to_string()),
            tool_call_id: None,
            tool_calls: None,
        };

        self.messages.push(message);

        loop {
            self.truncate_old_tool_results();

            // info!(
            //     "[agent] sending {} messages to {} (max_tokens={})",
            //     self.messages.len(),
            //     model_session.params().model,
            //     model_session.params().max_tokens,
            // );

            let resp = match self
                .model_client
                .complete(&self.messages, &self.tools.schemas())
                .await
            {
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

                self.broadcast_event(AgentEvent::TokenUsage {
                    prompt_tokens: cumulative_prompt,
                    completion_tokens: cumulative_completion,
                    total_tokens: cumulative_prompt + cumulative_completion,
                })
                .await;
            }

            let choice = resp
                .choices
                .into_iter()
                .next()
                .ok_or(AgentError::ModelClient(
                    "Response contained no choices".to_string(),
                ))?;

            // Clone tool_calls before moving the message so we can iterate
            // over them later while also pushing to messages.
            let tool_calls = choice.message.tool_calls.clone();

            // Append the assistant's reply to the running history immediately so
            // tool results added in the same iteration are correctly sequenced.
            self.messages.push(choice.message.clone());

            info!("[agent] finish_reason={:?}", choice.finish_reason);

            match choice.finish_reason {
                FinishReason::Stop => return Ok(choice.message),
                FinishReason::ToolCalls => {
                    // Stream any accompanying text before executing tools.
                    if let Some(content) = &choice.message.content
                        && !content.is_empty()
                    {
                        self.broadcast_event(AgentEvent::ContentDelta(content.to_owned()))
                            .await;
                    }

                    if let Some(tool_calls) = &tool_calls {
                        for tool_call in tool_calls {
                            match tool_call {
                                ToolCall::Function { id, function, .. } => {
                                    info!("[agent] tool call: {} (id={})", function.name, id);

                                    // Notify the client that a tool is starting.
                                    self.broadcast_event(AgentEvent::ToolCallStart {
                                        tool_call_id: id.clone(),
                                        tool_name: function.name.clone(),
                                        arguments: function.arguments.clone(),
                                    })
                                    .await;

                                    let (result_str, metadata) =
                                        self.handle_tool_call(id, function).await?;

                                    // Notify the client that the tool finished.
                                    self.broadcast_event(AgentEvent::ToolCallEnd {
                                        tool_call_id: id.clone(),
                                        tool_name: function.name.clone(),
                                        result: result_str.clone(),
                                        metadata,
                                    })
                                    .await;

                                    // Append the tool result so the LLM sees it on
                                    // the next iteration.
                                    self.messages.push(Message {
                                        role: Role::Tool,
                                        content: Some(result_str),
                                        tool_call_id: Some(id.clone()),
                                        tool_calls: None,
                                    });
                                }
                            }
                        }
                    }

                    // Loop: send the updated history back to the LLM.
                }
                FinishReason::Unsupported => {
                    let error_msg = "The model stopped with an unsupported finish reason. \
                                                    This model may not support tool calling.\n\n\
                                                    Try switching to a model that supports function calling \
                                                    (e.g. anthropic/claude-haiku-4-5) by setting `model` \
                                                    in ~/.ein/config.json."
                                                .to_string();

                    return Err(AgentError::UnsupportedFinishReason(error_msg));
                }
            }
        }
    }

    pub async fn cleanup(self)
    where
        MC: Send + 'static,
        TS: Send + 'static,
    {
        self.model_client.cleanup().await;
        self.tools.cleanup().await;
    }
}

// Private methods
impl<MC: ModelClient, TS: ToolSet> Agent<MC, TS> {
    fn new(
        num_recent_messages: usize,
        max_tool_result_chars: usize,
        model_client: MC,
        event_handler: Option<AgentEventHandler>,
        tools: TS,
        messages: Vec<Message>,
    ) -> Self {
        Self {
            num_recent_messages,
            max_tool_result_chars,
            model_client,
            event_handler,
            tools,
            messages,
        }
    }

    fn broadcast_event(&self, event: AgentEvent) -> BoxFuture<'static, ()> {
        let handler = self.event_handler.clone();

        Box::pin(async move {
            if let Some(event_handler) = handler {
                event_handler(event).await;
            }
        })
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

    async fn handle_tool_call(
        &mut self,
        id: &str,
        function: &FunctionCall,
    ) -> ToolResult<(String, String)> {
        let res = self
            .tools
            .call_tool(&function.name, id, &function.arguments)
            .await
            .map_err(|err| {
                if err.to_string().contains("tool not found") {
                    error!("[agent] unknown tool '{}'", function.name);
                    ToolError::Unknown(function.name.clone())
                } else {
                    ToolError::Execution(format!("{err}"))
                }
            })?;

        let meta = res
            .metadata
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_default();

        Ok((res.content, meta))
    }
}

impl<MC: ModelClient> Agent<MC, NativeToolSet> {
    /// Creates a builder using the default tool set. Tools can be added with
    /// [`AgentBuilder::add_tool`]. This is the entry point for most users.
    pub fn builder(client: MC) -> AgentBuilder<MC, NativeToolSet> {
        AgentBuilder {
            num_recent_messages: KEEP_RECENT_MESSAGES,
            max_tool_result_chars: MAX_TOOL_RESULT_CHARS,
            event_handler: None,
            model_client: client,
            tools: NativeToolSet::default(),
            message_history: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use ein_core::types::{Choice, CompletionResponse, ToolDef, ToolResult};

    use super::*;

    struct BasicTestModelClient {
        response: CompletionResponse,
    }

    #[async_trait]
    impl ModelClient for BasicTestModelClient {
        async fn complete(
            &mut self,
            _messages: &[Message],
            _tools: &[ToolDef],
        ) -> anyhow::Result<CompletionResponse> {
            Ok(self.response.clone())
        }
    }

    fn basic_test_client() -> BasicTestModelClient {
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

        BasicTestModelClient { response: res }
    }

    struct ToolTestModelClient {
        call_counter: Mutex<u8>,
        tool_response: CompletionResponse,
        finish_response: CompletionResponse,
    }

    impl ToolTestModelClient {
        fn new(tool_response: CompletionResponse, finish_response: CompletionResponse) -> Self {
            Self {
                call_counter: Mutex::new(0),
                tool_response,
                finish_response,
            }
        }
    }

    #[async_trait]
    impl ModelClient for ToolTestModelClient {
        async fn complete(
            &mut self,
            _messages: &[Message],
            _tools: &[ToolDef],
        ) -> anyhow::Result<CompletionResponse> {
            let mut call_counter = self.call_counter.lock().unwrap();
            if *call_counter == 0 {
                *call_counter += 1;

                Ok(self.tool_response.clone())
            } else {
                Ok(self.finish_response.clone())
            }
        }
    }

    fn tool_test_client() -> ToolTestModelClient {
        let tool_res = CompletionResponse {
            choices: vec![Choice {
                index: None,
                finish_reason: FinishReason::ToolCalls,
                message: Message {
                    role: Role::Tool,
                    content: None,
                    tool_calls: Some(vec![ToolCall::Function {
                        id: "tool_id".to_string(),
                        index: 0,
                        function: FunctionCall {
                            name: "test_tool".to_string(),
                            arguments: "{\"test_arg\": \"test_val\"}".to_string(),
                        },
                    }]),
                    tool_call_id: Some("tool_call_id".to_string()),
                },
            }],
            usage: None,
            error: None,
        };

        let finish_res = CompletionResponse {
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

        ToolTestModelClient::new(tool_res, finish_res)
    }

    #[tokio::test]
    async fn test_basic_agent() {
        let model_client = basic_test_client();
        let mut agent = Agent::builder(model_client).build();

        let res = agent.chat("user message").await.unwrap();
        assert_eq!(res.role, Role::Assistant);
        assert_eq!(res.content, Some("assistant response".to_string()))
    }

    #[tokio::test]
    async fn test_tool_calling_agent() {
        use std::sync::Arc;

        #[derive(Clone)]
        struct TestTool {
            called_arg: Arc<Mutex<String>>,
        }

        impl TestTool {
            fn new() -> Self {
                Self {
                    called_arg: Arc::new(Mutex::new(String::new())),
                }
            }
        }

        #[async_trait]
        impl Tool for TestTool {
            fn name(&self) -> &str {
                "test_tool"
            }

            fn schema(&self) -> ToolDef {
                ToolDef::function(self.name(), "Tool for testing library agent tool calling")
                    .param("test_arg", "string", "Test agument passing", true)
                    .build()
            }

            async fn call(&self, id: &str, args: &str) -> anyhow::Result<ToolResult> {
                let args: serde_json::Value = serde_json::from_str(args)?;

                if let Some(test_arg) = args["test_arg"].as_str() {
                    let mut guard = self.called_arg.lock().unwrap();
                    *guard = test_arg.to_string();
                }

                Ok(ToolResult::new(id, "tool result".to_string()))
            }
        }

        let model_client = tool_test_client();
        let tool = TestTool::new();
        let mut agent = Agent::builder(model_client).add_tool(tool.clone()).build();

        let _ = agent.chat("user message").await.unwrap();

        assert_eq!(*tool.called_arg.lock().unwrap(), "test_val".to_string());
    }
}
