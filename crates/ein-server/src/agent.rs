use anyhow::anyhow;
use async_openai::{Client, config::OpenAIConfig};
use ein_proto::ein::{
    AgentEvent, AgentFinished, ContentDelta, ToolCallEnd, ToolCallStart,
    agent_event::Event,
};
use ein_tool::Role;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tonic::Status;

use crate::tools::ToolRegistry;

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Choice {
    index: usize,
    finish_reason: FinishReason,
    message: LlmMessage,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum FinishReason {
    Stop,
    ToolCalls,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct LlmMessage {
    role: Role,
    content: Option<String>,
    tool_calls: Option<Vec<ToolCall>>,
}

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
    arguments: String,
}

pub async fn run_agent(
    prompt: String,
    tool_registry: &mut ToolRegistry,
    client: &Client<OpenAIConfig>,
    tx: mpsc::Sender<Result<AgentEvent, Status>>,
) -> anyhow::Result<()> {
    let mut messages = vec![json!({
        "role": "user",
        "content": prompt
    })];

    loop {
        let response: Value = client
            .chat()
            .create_byot(json!({
                "messages": messages,
                "model": "anthropic/claude-haiku-4.5",
                "tools": tool_registry.schemas()?,
                "max_tokens": 2500,
            }))
            .await?;

        let choices: Vec<Choice> = response
            .get("choices")
            .map(|v| serde_json::from_value(v.clone()))
            .ok_or_else(|| anyhow!("Response missing 'choices' field"))??;
        let choice = choices
            .first()
            .ok_or_else(|| anyhow!("Response contained no choices"))?;

        messages.push(serde_json::to_value(choice.message.clone())?);

        let content = choice.message.content.as_deref().unwrap_or_default().to_string();

        match choice.finish_reason {
            FinishReason::ToolCalls => {
                if !content.is_empty() {
                    let _ = tx
                        .send(Ok(AgentEvent {
                            event: Some(Event::ContentDelta(ContentDelta {
                                text: content,
                            })),
                        }))
                        .await;
                }

                if let Some(tool_calls) = &choice.message.tool_calls {
                    for tool_call in tool_calls {
                        match tool_call {
                            ToolCall::Function { id, function, .. } => {
                                let _ = tx
                                    .send(Ok(AgentEvent {
                                        event: Some(Event::ToolCallStart(ToolCallStart {
                                            tool_call_id: id.clone(),
                                            tool_name: function.name.clone(),
                                            arguments: function.arguments.clone(),
                                        })),
                                    }))
                                    .await;

                                let Some(tool) = tool_registry.get(function.name.as_str()) else {
                                    return Err(anyhow!("Missing tool {}", function.name));
                                };

                                let res = tool.call(id, &function.arguments).await?;
                                let res_value = serde_json::to_value(&res)?;
                                let result_str = res_value
                                    .get("content")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();

                                let _ = tx
                                    .send(Ok(AgentEvent {
                                        event: Some(Event::ToolCallEnd(ToolCallEnd {
                                            tool_call_id: id.clone(),
                                            tool_name: function.name.clone(),
                                            result: result_str,
                                        })),
                                    }))
                                    .await;

                                messages.push(res_value);
                            }
                        }
                    }
                }
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
