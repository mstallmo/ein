// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

//! Core agent loop.
//!
//! [`run_agent`] drives a single conversation turn: it sends the current
//! message history to the LLM, streams back any text content as
//! [`ContentDelta`] events, executes every tool call the model requests, and
//! then repeats until the model signals [`FinishReason::Stop`].
//!
//! ## Message flow
//!
//! ```text
//! caller                   run_agent                LLM
//!   │                         │                      │
//!   │── messages ────────────►│── POST /chat/comp ──►│
//!   │                         │◄─ Choice ────────────│
//!   │                         │                      │
//!   │   FinishReason::ToolCalls:                     │
//!   │◄─ ContentDelta (opt) ───│                      │
//!   │◄─ ToolCallStart ────────│                      │
//!   │     (execute tool)      │                      │
//!   │◄─ ToolCallEnd ──────────│                      │
//!   │     (append result, loop again)                │
//!   │                         │                      │
//!   │   FinishReason::Stop:                          │
//!   │◄─ AgentFinished ────────│                      │
//!   │      (return)           │                      │
//! ```

use anyhow::anyhow;
use ein_agent::model_clients::ModelClient;
use ein_plugin::model_client::{FinishReason, FunctionCall, Message, Role, ToolCall};
use ein_proto::ein::agent_event;
use ein_proto::ein::{
    AgentError, AgentEvent, AgentFinished, ContentDelta, TokenUsage, ToolCallEnd, ToolCallStart,
    agent_event::Event,
};
use futures::future::BoxFuture;

use crate::model_client::ModelClientSession;
use crate::tools::WasmToolSet;

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

pub type AgentEventHandler = Arc<Box<dyn Fn(AgentEvent) -> BoxFuture<'static, ()> + Send + Sync>>;

pub struct AgentBuilder {
    num_recent_messages: usize,
    max_tool_result_chars: usize,
    event_handler: Option<AgentEventHandler>,
}

impl AgentBuilder {
    pub fn new() -> Self {
        Self {
            num_recent_messages: KEEP_RECENT_MESSAGES,
            max_tool_result_chars: MAX_TOOL_RESULT_CHARS,
            event_handler: None,
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

    pub fn build(self) -> Agent {
        Agent::new(
            self.num_recent_messages,
            self.max_tool_result_chars,
            self.event_handler,
        )
    }
}

impl Default for AgentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct Agent {
    num_recent_messages: usize,
    max_tool_result_chars: usize,
    event_handler: Option<AgentEventHandler>,
}

impl Agent {
    pub fn builder() -> AgentBuilder {
        AgentBuilder::default()
    }

    /// Runs the agent loop for one user turn.
    ///
    /// Sends `messages` to the LLM via the model client plugin, streams events
    /// back through `tx`, executes any requested tools, and loops until the model
    /// stops. The updated message history (including assistant turns and tool
    /// results) is written back into `messages` in place so the caller's
    /// conversation state stays current.
    pub async fn run(
        &self,
        messages: &mut Vec<Message>,
        tool_registry: &mut WasmToolSet,
        model_session: &mut ModelClientSession,
    ) -> anyhow::Result<()> {
        let mut cumulative_prompt = 0u32;
        let mut cumulative_completion = 0u32;
        // Count consecutive empty-stop turns so we can nudge the model when it
        // produces thinking tokens but no output, and bail out if it keeps failing.
        let mut empty_stop_retries = 0u32;
        const MAX_EMPTY_STOP_RETRIES: u32 = 1;

        loop {
            self.truncate_old_tool_results(messages);

            println!(
                "[agent] sending {} messages to {} (max_tokens={})",
                messages.len(),
                model_session.params().model,
                model_session.params().max_tokens,
            );

            let resp = match model_session
                .complete(messages, &tool_registry.schemas())
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[agent] model client error: {e}");

                    self.broadcast_event(Event::AgentError(AgentError {
                        message: e.to_string(),
                    }))
                    .await;

                    return Ok(());
                }
            };

            // Check for API-level error (e.g. 402 insufficient credits).
            if let Some(error_obj) = &resp.error {
                let msg = error_obj
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown API error");
                eprintln!("[agent] api error: {msg}");

                self.broadcast_event(Event::AgentError(AgentError {
                    message: msg.to_string(),
                }))
                .await;

                return Ok(());
            }

            // Extract and accumulate token usage from this response.
            if let Some(usage) = &resp.usage {
                println!(
                    "[agent] tokens this call: prompt={}, completion={}",
                    usage.prompt_tokens, usage.completion_tokens,
                );
                cumulative_prompt += usage.prompt_tokens;
                cumulative_completion += usage.completion_tokens;

                self.broadcast_event(Event::TokenUsage(TokenUsage {
                    prompt_tokens: cumulative_prompt as i32,
                    completion_tokens: cumulative_completion as i32,
                    total_tokens: (cumulative_prompt + cumulative_completion) as i32,
                }))
                .await;
            }

            let choice = resp
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("Response contained no choices"))?;

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
            messages.push(choice.message);
            let effective_finish = if has_tool_calls {
                FinishReason::ToolCalls
            } else {
                choice.finish_reason
            };

            println!(
                "[agent] finish_reason={:?} (effective={:?})",
                choice.finish_reason, effective_finish
            );

            match effective_finish {
                FinishReason::ToolCalls => {
                    // Stream any accompanying text before executing tools.
                    if !content.is_empty() {
                        self.broadcast_event(Event::ContentDelta(ContentDelta { text: content }))
                            .await;
                    }

                    if let Some(tool_calls) = &tool_calls {
                        for tool_call in tool_calls {
                            match tool_call {
                                ToolCall::Function { id, function, .. } => {
                                    println!("[agent] tool call: {} (id={})", function.name, id);

                                    // Notify the client that a tool is starting.
                                    self.broadcast_event(Event::ToolCallStart(ToolCallStart {
                                        tool_call_id: id.clone(),
                                        tool_name: function.name.clone(),
                                        arguments: function.arguments.clone(),
                                    }))
                                    .await;

                                    let (result_str, metadata) =
                                        self.handle_tool_call(tool_registry, &id, &function).await;

                                    // Notify the client that the tool finished.
                                    self.broadcast_event(Event::ToolCallEnd(ToolCallEnd {
                                        tool_call_id: id.clone(),
                                        tool_name: function.name.clone(),
                                        result: result_str.clone(),
                                        metadata,
                                    }))
                                    .await;

                                    // Append the tool result so the LLM sees it on
                                    // the next iteration.
                                    messages.push(Message {
                                        role: Role::Tool,
                                        content: Some(result_str),
                                        tool_call_id: Some(id.clone()),
                                        tool_calls: None,
                                    });
                                }
                            }
                        }
                    }

                    empty_stop_retries = 0;
                    // Loop: send the updated history back to the LLM.
                }
                FinishReason::Stop => {
                    if content.is_empty() {
                        if empty_stop_retries < MAX_EMPTY_STOP_RETRIES {
                            empty_stop_retries += 1;
                            eprintln!(
                                "[agent] empty stop (thinking-only response), nudging model \
                                 to continue (attempt {}/{})",
                                empty_stop_retries, MAX_EMPTY_STOP_RETRIES,
                            );

                            // The empty assistant turn is already in `messages`; add
                            // a user prompt to coax the model into producing output.
                            messages.push(Message {
                                role: Role::User,
                                content: Some(
                                    "Your last response was empty. Emit a tool call now to make progress."
                                        .to_string(),
                                ),
                                tool_calls: None,
                                tool_call_id: None,
                            });

                            continue;
                        }

                        eprintln!(
                            "[agent] model returned stop with empty content after {empty_stop_retries} retries"
                        );

                        self.broadcast_event(Event::AgentFinished(AgentFinished {
                            final_content: "(The model finished without producing a response.)"
                                .to_string(),
                        }))
                        .await;
                    } else {
                        self.broadcast_event(Event::AgentFinished(AgentFinished {
                            final_content: content,
                        }))
                        .await;
                    }

                    break;
                }
                FinishReason::Unsupported => {
                    self.broadcast_event(Event::AgentError(AgentError {
                        message: "The model stopped with an unsupported finish reason. \
                            This model may not support tool calling.\n\n\
                            Try switching to a model that supports function calling \
                            (e.g. anthropic/claude-haiku-4-5) by setting `model` \
                            in ~/.ein/config.json."
                            .to_string(),
                    }))
                    .await;

                    return Ok(());
                }
            }
        }

        Ok(())
    }
}

// Private methods
impl Agent {
    fn new(
        num_recent_messages: usize,
        max_tool_result_chars: usize,
        event_handler: Option<AgentEventHandler>,
    ) -> Self {
        Self {
            num_recent_messages,
            max_tool_result_chars,
            event_handler,
        }
    }

    async fn broadcast_event(&self, event: agent_event::Event) {
        if let Some(event_handler) = &self.event_handler {
            event_handler(AgentEvent { event: Some(event) }).await;
        }
    }

    /// Replaces the `content` of stale, large tool result messages with a compact
    /// placeholder so they no longer consume significant context budget.
    ///
    /// A message is eligible if:
    /// - its `role` is `"tool"`
    /// - it is more than `KEEP_RECENT_MESSAGES` positions from the end of `messages`
    /// - its `content` length exceeds `MAX_TOOL_RESULT_CHARS`
    fn truncate_old_tool_results(&self, messages: &mut [Message]) {
        let len = messages.len();
        let truncate_before = len.saturating_sub(self.num_recent_messages);

        for msg in messages[..truncate_before].iter_mut() {
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
    async fn handle_tool_call(
        &self,
        tool_registry: &mut WasmToolSet,
        id: &str,
        function: &FunctionCall,
    ) -> (String, String) {
        match tool_registry.get(function.name.as_str()) {
            Some(tool) => {
                match tool.enable_chunk_sender().await {
                    Ok(should_enable_chunk_sender) => {
                        if should_enable_chunk_sender && let Some(handler) = &self.event_handler {
                            tool.set_chunk_sender(handler.clone(), id.to_owned())
                        }
                    }
                    Err(err) => {
                        eprintln!("[agent] tool '{}' error: {err}", function.name);

                        return (format!("Error: {err}"), String::new());
                    }
                };

                match tool.call(id, &function.arguments).await {
                    Ok(res) => {
                        let meta = res
                            .metadata
                            .as_ref()
                            .map(|v| v.to_string())
                            .unwrap_or_default();

                        (res.content, meta)
                    }
                    Err(e) => {
                        eprintln!("[agent] tool '{}' error: {e}", function.name);

                        (format!("Error: {e}"), String::new())
                    }
                }
            }
            None => {
                eprintln!("[agent] unknown tool '{}'", function.name);

                (
                    format!("Error: tool '{}' not found", function.name),
                    String::new(),
                )
            }
        }
    }
}
