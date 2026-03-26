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
use async_openai::{Client, config::OpenAIConfig};
use ein_proto::ein::{
    AgentError, AgentEvent, AgentFinished, ContentDelta, TokenUsage, ToolCallEnd, ToolCallStart,
    agent_event::Event,
};
use ein_tool::Role;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tonic::Status;

use crate::tools::ToolRegistry;

// ---------------------------------------------------------------------------
// LLM response types
//
// These mirror the OpenAI chat completion response shape used by OpenRouter.
// We deserialise only the fields Ein actually needs.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Choice {
    index: usize,
    finish_reason: FinishReason,
    message: LlmMessage,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum FinishReason {
    /// The model finished naturally with no pending tool calls.
    Stop,
    /// The model wants to invoke one or more tools before continuing.
    ToolCalls,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LlmMessage {
    role: Role,
    content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
}

/// A tool call requested by the model. Only the `function` variant is used
/// by the OpenAI-compatible API Ein targets.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
#[serde(rename_all = "lowercase")]
enum ToolCall {
    Function {
        id: String,
        index: usize,
        function: FunctionCall,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct FunctionCall {
    name: String,
    /// Raw JSON string containing the arguments chosen by the model.
    arguments: String,
}

// ---------------------------------------------------------------------------
// Session configuration
// ---------------------------------------------------------------------------

/// Per-session LLM configuration derived from the client's `SessionConfig`.
pub struct SessionParams {
    pub model: String,
    pub max_tokens: i32,
}

/// Token counts returned by OpenRouter in each completion response.
#[derive(Debug, Clone, Deserialize)]
struct Usage {
    prompt_tokens: i32,
    completion_tokens: i32,
    #[allow(dead_code)]
    total_tokens: i32,
}

// ---------------------------------------------------------------------------
// Agent loop
// ---------------------------------------------------------------------------

/// Runs the agent loop for one user turn.
///
/// Sends `messages` to the LLM, streams events back through `tx`, executes
/// any requested tools, and loops until the model stops. The updated message
/// history (including assistant turns and tool results) is written back into
/// `messages` in place so the caller's conversation state stays current.
pub async fn run_agent(
    messages: &mut Vec<Value>,
    tool_registry: &mut ToolRegistry,
    client: &Client<OpenAIConfig>,
    session: &SessionParams,
    tx: &mpsc::Sender<Result<AgentEvent, Status>>,
) -> anyhow::Result<()> {
    let mut cumulative_prompt = 0i32;
    let mut cumulative_completion = 0i32;

    loop {
        println!(
            "[agent] sending {} messages to {} (max_tokens={})",
            messages.len(),
            session.model,
            session.max_tokens,
        );

        let response: Value = client
            .chat()
            .create_byot(json!({
                "messages": messages,
                "model": session.model,
                "tools": tool_registry.schemas()?,
                "max_tokens": session.max_tokens,
            }))
            .await?;

        // Check for API-level error (e.g. 402 insufficient credits).
        if let Some(error_obj) = response.get("error") {
            let msg = error_obj
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown API error");
            eprintln!("[agent] api error: {msg}");
            let _ = tx
                .send(Ok(AgentEvent {
                    event: Some(Event::AgentError(AgentError {
                        message: msg.to_string(),
                    })),
                }))
                .await;
            return Ok(());
        }

        // Extract and accumulate token usage from this response.
        if let Some(usage_val) = response.get("usage") {
            if let Ok(usage) = serde_json::from_value::<Usage>(usage_val.clone()) {
                cumulative_prompt += usage.prompt_tokens;
                cumulative_completion += usage.completion_tokens;
                let _ = tx
                    .send(Ok(AgentEvent {
                        event: Some(Event::TokenUsage(TokenUsage {
                            prompt_tokens: cumulative_prompt,
                            completion_tokens: cumulative_completion,
                            total_tokens: cumulative_prompt + cumulative_completion,
                        })),
                    }))
                    .await;
            }
        }

        let choices: Vec<Choice> = response
            .get("choices")
            .map(|v| serde_json::from_value(v.clone()))
            .ok_or_else(|| anyhow!("Response missing 'choices' field"))??;
        let choice = choices
            .first()
            .ok_or_else(|| anyhow!("Response contained no choices"))?;

        // Append the assistant's reply to the running history immediately so
        // tool results added in the same iteration are correctly sequenced.
        messages.push(serde_json::to_value(choice.message.clone())?);

        let content = choice
            .message
            .content
            .as_deref()
            .unwrap_or_default()
            .to_string();

        println!("[agent] finish_reason={:?}", choice.finish_reason);

        match choice.finish_reason {
            FinishReason::ToolCalls => {
                // Stream any accompanying text before executing tools.
                if !content.is_empty() {
                    let _ = tx
                        .send(Ok(AgentEvent {
                            event: Some(Event::ContentDelta(ContentDelta { text: content })),
                        }))
                        .await;
                }

                if let Some(tool_calls) = &choice.message.tool_calls {
                    for tool_call in tool_calls {
                        match tool_call {
                            ToolCall::Function { id, function, .. } => {
                                println!("[agent] tool call: {} (id={})", function.name, id);
                                // Notify the client that a tool is starting.
                                let _ = tx
                                    .send(Ok(AgentEvent {
                                        event: Some(Event::ToolCallStart(ToolCallStart {
                                            tool_call_id: id.clone(),
                                            tool_name: function.name.clone(),
                                            arguments: function.arguments.clone(),
                                        })),
                                    }))
                                    .await;

                                let (result_str, metadata) = match tool_registry
                                    .get(function.name.as_str())
                                {
                                    Some(tool) => match tool.call(id, &function.arguments).await {
                                        Ok(res) => {
                                            let meta = res
                                                .metadata
                                                .as_ref()
                                                .map(|v| v.to_string())
                                                .unwrap_or_default();
                                            (res.content, meta)
                                        }
                                        Err(e) => {
                                            eprintln!(
                                                "[agent] tool '{}' error: {e}",
                                                function.name
                                            );
                                            (format!("Error: {e}"), String::new())
                                        }
                                    },
                                    None => {
                                        eprintln!("[agent] unknown tool '{}'", function.name);
                                        (
                                            format!("Error: tool '{}' not found", function.name),
                                            String::new(),
                                        )
                                    }
                                };

                                // Notify the client that the tool finished.
                                let _ = tx
                                    .send(Ok(AgentEvent {
                                        event: Some(Event::ToolCallEnd(ToolCallEnd {
                                            tool_call_id: id.clone(),
                                            tool_name: function.name.clone(),
                                            result: result_str.clone(),
                                            metadata,
                                        })),
                                    }))
                                    .await;

                                // Append the tool result so the LLM sees it on
                                // the next iteration.
                                messages.push(json!({
                                    "role": "tool",
                                    "tool_call_id": id,
                                    "content": result_str,
                                }));
                            }
                        }
                    }
                }
                // Loop: send the updated history back to the LLM.
            }
            FinishReason::Stop => {
                let _ = tx
                    .send(Ok(AgentEvent {
                        event: Some(Event::AgentFinished(AgentFinished {
                            final_content: content,
                        })),
                    }))
                    .await;
                break;
            }
        }
    }

    Ok(())
}
