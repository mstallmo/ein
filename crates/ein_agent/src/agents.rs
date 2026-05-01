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
        display_arg: Option<String>,
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

    /// Clears the in-memory message history so the next `chat` call starts
    /// with a blank context. The caller is responsible for not persisting
    /// the cleared state if the original history should be kept in storage.
    pub fn clear_messages(&mut self) {
        self.messages.clear();
    }

    /// Summarises the current conversation using the model, then replaces the
    /// message history with the original system message(s) plus the summary
    /// injected as a new `System` message.
    ///
    /// Broadcasts a `ContentDelta` event containing the summary so the client
    /// can display it. Saves nothing — the caller (`grpc.rs`) owns persistence.
    ///
    /// Returns the summary string (empty if there was nothing to compact).
    pub async fn compact_history(&mut self) -> AgentResult<String> {
        // Nothing to compact if there are no conversational turns yet.
        if !self.messages.iter().any(|m| matches!(m.role, Role::User)) {
            return Ok(String::new());
        }

        const COMPACT_PROMPT: &str = "Please provide a detailed but concise summary of our conversation so far. \
             Include: goals discussed, files viewed or modified, code written or changed, \
             decisions made, and the current state of any ongoing tasks. \
             This summary will replace the full conversation history as context for \
             future turns — be thorough enough that work can continue without the original.";

        // Build summarization payload: full history + summary request.
        let mut summary_msgs = self.messages.clone();
        summary_msgs.push(Message {
            role: Role::User,
            content: Some(COMPACT_PROMPT.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });

        // Single non-agentic call — no tools.
        let resp = self
            .model_client
            .complete(&summary_msgs, &[])
            .await
            .map_err(|e| AgentError::ModelClient(e.to_string()))?;

        if let Some(error_obj) = &resp.error {
            let msg = error_obj
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown API error");
            return Err(AgentError::ModelClient(msg.to_string()));
        }

        let summary = resp
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .unwrap_or_default();

        // Stream the summary to the client before modifying state.
        if !summary.is_empty() {
            self.broadcast_event(AgentEvent::ContentDelta(summary.clone()))
                .await;
        }

        // Replace history: keep the original system message(s), then append the
        // summary as a new System message. Using System role means:
        //   - The LLM receives it as high-priority context on every future turn.
        //   - grpc.rs filters System messages out of the HistoryMessage replay,
        //     so on session resume the TUI shows a clean empty conversation rather
        //     than a fake "user" bubble containing the summary text.
        let system_msgs: Vec<Message> = std::mem::take(&mut self.messages)
            .into_iter()
            .filter(|m| matches!(m.role, Role::System))
            .collect();

        self.messages = system_msgs;
        self.messages.push(Message {
            role: Role::System,
            content: Some(format!("Summary of prior conversation:\n\n{summary}")),
            tool_calls: None,
            tool_call_id: None,
        });

        Ok(summary)
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
                                    let display_arg = self
                                        .tools
                                        .display_arg_for(&function.name, &function.arguments);
                                    self.broadcast_event(AgentEvent::ToolCallStart {
                                        tool_call_id: id.clone(),
                                        tool_name: function.name.clone(),
                                        arguments: function.arguments.clone(),
                                        display_arg,
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
    use ein_core::types::{Choice, CompletionResponse, ToolDef, ToolResult, Usage};

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

    // ---------------------------------------------------------------------------
    // Shared test helpers
    // ---------------------------------------------------------------------------

    fn tool_msg(id: &str, content: impl Into<String>) -> Message {
        Message {
            role: Role::Tool,
            content: Some(content.into()),
            tool_call_id: Some(id.to_string()),
            tool_calls: None,
        }
    }

    fn user_msg(content: impl Into<String>) -> Message {
        Message {
            role: Role::User,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn system_msg(content: impl Into<String>) -> Message {
        Message {
            role: Role::System,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn stop_response(content: &str) -> CompletionResponse {
        CompletionResponse {
            choices: vec![Choice {
                index: None,
                finish_reason: FinishReason::Stop,
                message: Message {
                    role: Role::Assistant,
                    content: Some(content.to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                },
            }],
            usage: None,
            error: None,
        }
    }

    // ---------------------------------------------------------------------------
    // truncate_old_tool_results — tested directly (private method, same file)
    // ---------------------------------------------------------------------------

    const TEST_THRESHOLD: usize = 50;
    const TEST_WINDOW: usize = 2;

    #[test]
    fn truncate_old_tool_results_replaces_large_stale_content() {
        let large = "x".repeat(TEST_THRESHOLD + 1);
        let history = vec![
            tool_msg("t1", &large),
            tool_msg("t2", &large),
            user_msg("recent 1"),
            user_msg("recent 2"),
        ];

        let mut agent = Agent::builder(basic_test_client())
            .num_recent_messages(TEST_WINDOW)
            .max_tool_result_chars(TEST_THRESHOLD)
            .with_message_history(history)
            .build();

        agent.truncate_old_tool_results();

        let msgs = agent.messages();
        assert!(
            msgs[0]
                .content
                .as_deref()
                .unwrap_or("")
                .starts_with("[Tool result truncated:"),
            "old large tool result must be truncated"
        );
        assert!(
            msgs[1]
                .content
                .as_deref()
                .unwrap_or("")
                .starts_with("[Tool result truncated:"),
            "old large tool result must be truncated"
        );
        assert_eq!(msgs[2].content.as_deref(), Some("recent 1"));
        assert_eq!(msgs[3].content.as_deref(), Some("recent 2"));
    }

    #[test]
    fn truncate_old_tool_results_keeps_recent_messages_intact() {
        let large = "x".repeat(TEST_THRESHOLD + 1);
        // All 3 messages are within the window of 3 — none should be truncated.
        let history = vec![
            tool_msg("t1", &large),
            tool_msg("t2", &large),
            tool_msg("t3", &large),
        ];

        let mut agent = Agent::builder(basic_test_client())
            .num_recent_messages(3)
            .max_tool_result_chars(TEST_THRESHOLD)
            .with_message_history(history)
            .build();

        agent.truncate_old_tool_results();

        for msg in agent.messages() {
            assert!(
                !msg.content
                    .as_deref()
                    .unwrap_or("")
                    .starts_with("[Tool result truncated:"),
                "recent messages must not be truncated"
            );
        }
    }

    #[test]
    fn truncate_old_tool_results_ignores_non_tool_messages() {
        let large = "x".repeat(TEST_THRESHOLD + 1);
        let history = vec![
            user_msg(&large),
            system_msg(&large),
            tool_msg("t1", "small"),
        ];

        let mut agent = Agent::builder(basic_test_client())
            .num_recent_messages(TEST_WINDOW)
            .max_tool_result_chars(TEST_THRESHOLD)
            .with_message_history(history)
            .build();

        agent.truncate_old_tool_results();

        let msgs = agent.messages();
        assert_eq!(
            msgs[0].content.as_deref(),
            Some(large.as_str()),
            "User must not be truncated"
        );
        assert_eq!(
            msgs[1].content.as_deref(),
            Some(large.as_str()),
            "System must not be truncated"
        );
    }

    #[test]
    fn truncate_old_tool_results_skips_content_at_threshold() {
        // content length == threshold is NOT truncated (condition is strictly >)
        let at_threshold = "x".repeat(TEST_THRESHOLD);
        let history = vec![
            tool_msg("t1", &at_threshold),
            user_msg("recent 1"),
            user_msg("recent 2"),
        ];

        let mut agent = Agent::builder(basic_test_client())
            .num_recent_messages(TEST_WINDOW)
            .max_tool_result_chars(TEST_THRESHOLD)
            .with_message_history(history)
            .build();

        agent.truncate_old_tool_results();

        assert_eq!(
            agent.messages()[0].content.as_deref(),
            Some(at_threshold.as_str()),
            "content exactly at threshold must not be truncated"
        );
    }

    // ---------------------------------------------------------------------------
    // compact_history
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn compact_history_returns_empty_when_no_user_messages() {
        let mut agent = Agent::builder(basic_test_client())
            .with_message_history(vec![system_msg("you are helpful")])
            .build();

        let result = agent.compact_history().await.unwrap();
        assert_eq!(result, "", "nothing to compact without user messages");
    }

    #[tokio::test]
    async fn compact_history_replaces_history_with_system_plus_summary() {
        let summary = "Goals discussed, files modified, current state.";
        let mut agent = Agent::builder(BasicTestModelClient {
            response: stop_response(summary),
        })
        .with_message_history(vec![system_msg("sys"), user_msg("do stuff")])
        .build();

        let returned = agent.compact_history().await.unwrap();
        assert_eq!(returned, summary);

        let msgs = agent.messages();
        assert_eq!(msgs.len(), 2, "original system + new summary system");
        assert!(matches!(msgs[0].role, Role::System));
        assert_eq!(msgs[0].content.as_deref(), Some("sys"));
        assert!(matches!(msgs[1].role, Role::System));
        assert!(msgs[1].content.as_deref().unwrap_or("").contains(summary));
    }

    #[tokio::test]
    async fn compact_history_broadcasts_content_delta_event() {
        use std::sync::Arc;

        let summary = "Detailed summary.";
        let captured: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let cap = captured.clone();

        let mut agent = Agent::builder(BasicTestModelClient {
            response: stop_response(summary),
        })
        .with_event_handler(move |event| {
            let cap = cap.clone();
            async move {
                cap.lock().unwrap().push(event);
            }
        })
        .with_message_history(vec![user_msg("do stuff")])
        .build();

        agent.compact_history().await.unwrap();

        let events = captured.lock().unwrap();
        let deltas: Vec<&str> = events
            .iter()
            .filter_map(|e| {
                if let AgentEvent::ContentDelta(t) = e {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(deltas, vec![summary]);
    }

    // ---------------------------------------------------------------------------
    // chat error paths
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn chat_returns_error_on_api_error_response() {
        let mut agent = Agent::builder(BasicTestModelClient {
            response: CompletionResponse {
                choices: vec![],
                usage: None,
                error: Some(serde_json::json!({"message": "insufficient credits"})),
            },
        })
        .build();

        let err = agent.chat("prompt").await.unwrap_err();
        assert!(matches!(err, AgentError::ModelClient(_)));
        assert!(err.to_string().contains("insufficient credits"));
    }

    #[tokio::test]
    async fn chat_returns_error_on_unsupported_finish_reason() {
        let mut agent = Agent::builder(BasicTestModelClient {
            response: CompletionResponse {
                choices: vec![Choice {
                    index: None,
                    finish_reason: FinishReason::Unsupported,
                    message: Message {
                        role: Role::Assistant,
                        content: None,
                        tool_calls: None,
                        tool_call_id: None,
                    },
                }],
                usage: None,
                error: None,
            },
        })
        .build();

        let err = agent.chat("prompt").await.unwrap_err();
        assert!(matches!(err, AgentError::UnsupportedFinishReason(_)));
    }

    // ---------------------------------------------------------------------------
    // Token usage events and clear
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn chat_emits_token_usage_event() {
        use std::sync::Arc;

        let captured: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let cap = captured.clone();

        let mut agent = Agent::builder(BasicTestModelClient {
            response: CompletionResponse {
                choices: vec![Choice {
                    index: None,
                    finish_reason: FinishReason::Stop,
                    message: Message {
                        role: Role::Assistant,
                        content: Some("done".to_string()),
                        tool_calls: None,
                        tool_call_id: None,
                    },
                }],
                usage: Some(Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                }),
                error: None,
            },
        })
        .with_event_handler(move |event| {
            let cap = cap.clone();
            async move {
                cap.lock().unwrap().push(event);
            }
        })
        .build();

        agent.chat("hello").await.unwrap();

        let events = captured.lock().unwrap();
        let usage = events.iter().find_map(|e| {
            if let AgentEvent::TokenUsage {
                prompt_tokens,
                completion_tokens,
                total_tokens,
            } = e
            {
                Some((*prompt_tokens, *completion_tokens, *total_tokens))
            } else {
                None
            }
        });
        assert_eq!(
            usage,
            Some((10, 5, 15)),
            "TokenUsage event must carry correct totals"
        );
    }

    #[tokio::test]
    async fn clear_messages_empties_history() {
        let mut agent = Agent::builder(basic_test_client())
            .with_message_history(vec![system_msg("sys"), user_msg("hello")])
            .build();

        assert!(!agent.messages().is_empty());
        agent.clear_messages();
        assert!(agent.messages().is_empty());
    }
}
