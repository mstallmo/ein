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
use ein_plugin::model_client::{CompletionRequest, FinishReason, FunctionCall, ToolCall};
use ein_proto::ein::{
    AgentError, AgentEvent, AgentFinished, ContentDelta, TokenUsage, ToolCallEnd, ToolCallStart,
    agent_event::Event,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tonic::Status;

use crate::model_client::WasmModelClient;
use crate::tools::ToolRegistry;

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

/// Runs the agent loop for one user turn.
///
/// Sends `messages` to the LLM via the model client plugin, streams events
/// back through `tx`, executes any requested tools, and loops until the model
/// stops. The updated message history (including assistant turns and tool
/// results) is written back into `messages` in place so the caller's
/// conversation state stays current.
pub async fn run_agent(
    messages: &mut Vec<Value>,
    tool_registry: &mut ToolRegistry,
    model: &mut WasmModelClient,
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

        let resp = match model
            .complete(&CompletionRequest {
                model: session.model.clone(),
                messages: messages.clone(),
                tools: tool_registry.schemas()?,
                max_tokens: session.max_tokens,
            })
            .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[agent] model client error: {e}");

                let _ = tx
                    .send(Ok(AgentEvent {
                        event: Some(Event::AgentError(AgentError {
                            message: e.to_string(),
                        })),
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
        if let Some(usage) = &resp.usage {
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

        let choice = resp
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Response contained no choices"))?;

        // Append the assistant's reply to the running history immediately so
        // tool results added in the same iteration are correctly sequenced.
        messages.push(serde_json::to_value(&choice.message)?);

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

                                let (result_str, metadata) =
                                    handle_tool_call(tx, tool_registry, id, function).await;

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

async fn handle_tool_call(
    tx: &mpsc::Sender<Result<AgentEvent, Status>>,
    tool_registry: &mut ToolRegistry,
    id: &String,
    function: &FunctionCall,
) -> (String, String) {
    match tool_registry.get(function.name.as_str()) {
        Some(tool) => {
            match tool.enable_chunk_sender().await {
                Ok(should_enable_chunk_sender) => {
                    if should_enable_chunk_sender {
                        tool.set_chunk_sender(tx.clone(), id.clone())
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
