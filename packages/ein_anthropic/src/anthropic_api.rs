use anyhow::anyhow;
use ein_plugin::model_client::{
    Choice, CompletionRequest, CompletionResponse, FinishReason, FunctionCall, Message, Role, Tool,
    ToolCall, ToolFunctionParams, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicRequest {
    model: String,
    max_tokens: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    system: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,
    messages: Vec<AnthropicMessage>,
}

impl From<CompletionRequest> for AnthropicRequest {
    fn from(req: CompletionRequest) -> Self {
        // Extract system messages and join them into the top-level "system" field.
        let system_text: String = req
            .messages
            .iter()
            .filter(|m| matches!(m.role, Role::System))
            .filter_map(|m| m.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n\n");

        let non_system: Vec<&Message> = req
            .messages
            .iter()
            .filter(|m| !matches!(m.role, Role::System))
            .collect();

        Self {
            model: req.model,
            max_tokens: req.max_tokens,
            system: system_text,
            tools: req.tools.into_iter().map(Into::into).collect(),
            messages: non_system.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: ToolFunctionParams,
}

impl From<Tool> for AnthropicTool {
    fn from(tool: Tool) -> Self {
        match tool {
            Tool::Function { function } => Self {
                name: function.name,
                description: function.description,
                input_schema: function.parameters,
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AnthropicMessage {
    role: AnthropicRole,
    content: AnthropicMessageContent,
}

impl From<&Message> for AnthropicMessage {
    fn from(msg: &Message) -> Self {
        match msg.role {
            Role::User => Self {
                role: AnthropicRole::User,
                content: AnthropicMessageContent::Text(msg.content.clone().unwrap_or_default()),
            },
            Role::Assistant => {
                let mut blocks: Vec<AnthropicContentBlock> = Vec::new();

                if let Some(text) = &msg.content {
                    if !text.is_empty() {
                        blocks.push(AnthropicContentBlock::Text { text: text.clone() });
                    }
                }

                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        match tc {
                            ToolCall::Function { id, function, .. } => {
                                let input = serde_json::from_str(&function.arguments)
                                    .unwrap_or(Value::Object(Default::default()));
                                blocks.push(AnthropicContentBlock::ToolUse {
                                    id: id.clone(),
                                    name: function.name.clone(),
                                    input,
                                });
                            }
                        }
                    }
                }

                // Anthropic requires a non-empty content array for assistant turns.
                if blocks.is_empty() {
                    blocks.push(AnthropicContentBlock::Text {
                        text: String::new(),
                    });
                }

                Self {
                    role: AnthropicRole::Assistant,
                    content: AnthropicMessageContent::Blocks(blocks),
                }
            }
            Role::Tool => Self {
                role: AnthropicRole::User,
                content: AnthropicMessageContent::Blocks(vec![AnthropicContentBlock::ToolResult {
                    tool_use_id: msg.tool_call_id.clone().unwrap_or_default(),
                    content: msg.content.clone().unwrap_or_default(),
                }]),
            },
            Role::System => {
                // System messages should be extracted into the top-level "system" field
                // before conversion. Callers are expected to filter them out; this arm
                // is a safe fallback.
                Self {
                    role: AnthropicRole::User,
                    content: AnthropicMessageContent::Text(msg.content.clone().unwrap_or_default()),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AnthropicRole {
    Assistant,
    User,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum AnthropicMessageContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicApiResponse {
    Message(AnthropicSuccessResponse),
    Error { error: serde_json::Value },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AnthropicStopReason {
    EndTurn,
    ToolUse,
    #[serde(other)]
    Other,
}

impl From<AnthropicStopReason> for FinishReason {
    fn from(r: AnthropicStopReason) -> Self {
        match r {
            AnthropicStopReason::ToolUse => FinishReason::ToolCalls,
            _ => FinishReason::Stop,
        }
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicResponseUsage {
    input_tokens: i32,
    output_tokens: i32,
}

impl From<AnthropicResponseUsage> for Usage {
    fn from(u: AnthropicResponseUsage) -> Self {
        Usage {
            prompt_tokens: u.input_tokens,
            completion_tokens: u.output_tokens,
            total_tokens: u.input_tokens + u.output_tokens,
        }
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicSuccessResponse {
    content: Vec<AnthropicContentBlock>,
    stop_reason: AnthropicStopReason,
    usage: Option<AnthropicResponseUsage>,
}

impl From<AnthropicSuccessResponse> for CompletionResponse {
    fn from(resp: AnthropicSuccessResponse) -> Self {
        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        for (idx, block) in resp.content.into_iter().enumerate() {
            match block {
                AnthropicContentBlock::Text { text } => {
                    text_parts.push(text);
                }
                AnthropicContentBlock::ToolUse { id, name, input } => {
                    let arguments =
                        serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                    tool_calls.push(ToolCall::Function {
                        id,
                        index: idx,
                        function: FunctionCall { name, arguments },
                    });
                }
                AnthropicContentBlock::ToolResult { .. } => {}
            }
        }

        let content = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join(""))
        };

        let tool_calls_opt = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        };

        CompletionResponse {
            choices: vec![Choice {
                index: Some(0),
                finish_reason: resp.stop_reason.into(),
                message: Message {
                    role: Role::Assistant,
                    content,
                    tool_calls: tool_calls_opt,
                    tool_call_id: None,
                },
            }],
            usage: resp.usage.map(Into::into),
            error: None,
        }
    }
}

/// Convert an Anthropic Messages API response body into a `CompletionResponse`.
pub fn translate_response(body: &str) -> anyhow::Result<CompletionResponse> {
    let resp: AnthropicApiResponse = serde_json::from_str(body)
        .map_err(|e| anyhow!("failed to parse Anthropic response: {e}\nbody: {body}"))?;

    match resp {
        AnthropicApiResponse::Error { error } => Ok(CompletionResponse {
            choices: vec![],
            usage: None,
            error: Some(error),
        }),
        AnthropicApiResponse::Message(success) => Ok(success.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ein_plugin::model_client::{FunctionCall, ToolCall};
    use serde_json::json;

    // ---------------------------------------------------------------------------
    // From<Message> for AnthropicMessage
    // ---------------------------------------------------------------------------

    #[test]
    fn test_from_user_message() {
        let msg = Message {
            role: Role::User,
            content: Some("hello".to_owned()),
            tool_calls: None,
            tool_call_id: None,
        };
        let out = serde_json::to_value(AnthropicMessage::from(&msg)).unwrap();
        assert_eq!(out["role"], "user");
        assert_eq!(out["content"], "hello");
    }

    #[test]
    fn test_from_assistant_text_only() {
        let msg = Message {
            role: Role::Assistant,
            content: Some("thinking".to_owned()),
            tool_calls: None,
            tool_call_id: None,
        };
        let out = serde_json::to_value(AnthropicMessage::from(&msg)).unwrap();
        assert_eq!(out["role"], "assistant");
        assert_eq!(out["content"][0]["type"], "text");
        assert_eq!(out["content"][0]["text"], "thinking");
    }

    #[test]
    fn test_from_assistant_empty_content_gets_empty_text_block() {
        let msg = Message {
            role: Role::Assistant,
            content: None,
            tool_calls: None,
            tool_call_id: None,
        };
        let out = serde_json::to_value(AnthropicMessage::from(&msg)).unwrap();
        assert_eq!(out["role"], "assistant");
        assert_eq!(out["content"][0]["type"], "text");
        assert_eq!(out["content"][0]["text"], "");
    }

    #[test]
    fn test_from_assistant_tool_calls() {
        let msg = Message {
            role: Role::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall::Function {
                id: "call_1".to_owned(),
                index: 0,
                function: FunctionCall {
                    name: "Bash".to_owned(),
                    arguments: r#"{"command":"ls"}"#.to_owned(),
                },
            }]),
            tool_call_id: None,
        };
        let out = serde_json::to_value(AnthropicMessage::from(&msg)).unwrap();
        assert_eq!(out["role"], "assistant");
        assert_eq!(out["content"][0]["type"], "tool_use");
        assert_eq!(out["content"][0]["id"], "call_1");
        assert_eq!(out["content"][0]["name"], "Bash");
        assert_eq!(out["content"][0]["input"], json!({"command": "ls"}));
    }

    #[test]
    fn test_from_assistant_text_and_tool_calls() {
        let msg = Message {
            role: Role::Assistant,
            content: Some("running it".to_owned()),
            tool_calls: Some(vec![ToolCall::Function {
                id: "call_2".to_owned(),
                index: 0,
                function: FunctionCall {
                    name: "Read".to_owned(),
                    arguments: r#"{"path":"/tmp/foo"}"#.to_owned(),
                },
            }]),
            tool_call_id: None,
        };
        let out = serde_json::to_value(AnthropicMessage::from(&msg)).unwrap();
        assert_eq!(out["role"], "assistant");
        assert_eq!(out["content"][0]["type"], "text");
        assert_eq!(out["content"][0]["text"], "running it");
        assert_eq!(out["content"][1]["type"], "tool_use");
        assert_eq!(out["content"][1]["name"], "Read");
    }

    #[test]
    fn test_from_tool_result_message() {
        let msg = Message {
            role: Role::Tool,
            content: Some("file contents".to_owned()),
            tool_calls: None,
            tool_call_id: Some("call_1".to_owned()),
        };
        let out = serde_json::to_value(AnthropicMessage::from(&msg)).unwrap();
        assert_eq!(out["role"], "user");
        assert_eq!(out["content"][0]["type"], "tool_result");
        assert_eq!(out["content"][0]["tool_use_id"], "call_1");
        assert_eq!(out["content"][0]["content"], "file contents");
    }

    // ---------------------------------------------------------------------------
    // translate_response
    // ---------------------------------------------------------------------------

    #[test]
    fn test_translate_response_text_only() {
        let body = r#"{
            "type": "message",
            "content": [{"type": "text", "text": "Hello!"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        }"#;
        let resp = translate_response(body).unwrap();
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].message.content.as_deref(), Some("Hello!"));
        assert!(resp.choices[0].message.tool_calls.is_none());
        assert!(matches!(resp.choices[0].finish_reason, FinishReason::Stop));
        let usage = resp.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_translate_response_tool_use() {
        let body = r#"{
            "type": "message",
            "content": [
                {"type": "tool_use", "id": "call_1", "name": "Bash", "input": {"command": "ls"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 20, "output_tokens": 8}
        }"#;
        let resp = translate_response(body).unwrap();
        assert!(resp.choices[0].message.content.is_none());
        let tool_calls = resp.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        match &tool_calls[0] {
            ToolCall::Function { id, function, .. } => {
                assert_eq!(id, "call_1");
                assert_eq!(function.name, "Bash");
                assert_eq!(function.arguments, r#"{"command":"ls"}"#);
            }
        }
        assert!(matches!(
            resp.choices[0].finish_reason,
            FinishReason::ToolCalls
        ));
    }

    #[test]
    fn test_translate_response_text_and_tool_use() {
        let body = r#"{
            "type": "message",
            "content": [
                {"type": "text", "text": "Running it"},
                {"type": "tool_use", "id": "call_2", "name": "Read", "input": {"path": "/tmp/foo"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 5, "output_tokens": 3}
        }"#;
        let resp = translate_response(body).unwrap();
        assert_eq!(
            resp.choices[0].message.content.as_deref(),
            Some("Running it")
        );
        let tool_calls = resp.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
    }

    #[test]
    fn test_translate_response_multiple_text_blocks_joined() {
        let body = r#"{
            "type": "message",
            "content": [
                {"type": "text", "text": "Part one"},
                {"type": "text", "text": " part two"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }"#;
        let resp = translate_response(body).unwrap();
        assert_eq!(
            resp.choices[0].message.content.as_deref(),
            Some("Part one part two")
        );
    }

    #[test]
    fn test_translate_response_error_type() {
        let body =
            r#"{"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}"#;
        let resp = translate_response(body).unwrap();
        assert!(resp.choices.is_empty());
        assert!(resp.usage.is_none());
        assert!(resp.error.is_some());
    }

    #[test]
    fn test_translate_response_no_usage() {
        let body = r#"{
            "type": "message",
            "content": [{"type": "text", "text": "hi"}],
            "stop_reason": "end_turn"
        }"#;
        let resp = translate_response(body).unwrap();
        assert!(resp.usage.is_none());
    }

    #[test]
    fn test_translate_tools() {
        let tool: Tool = serde_json::from_value(json!({
            "type": "function",
            "function": {
                "name": "Read",
                "description": "Read a file at the specified path",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Path to the file to read"
                        }
                    },
                    "required": ["path"]
                }
            }
        }))
        .expect("Failed to parse JSON value");

        let translated_tool = AnthropicTool::from(tool.clone());
        assert_eq!(translated_tool.name, "Read".to_string());
        assert_eq!(
            translated_tool.description,
            "Read a file at the specified path".to_string()
        );

        match tool {
            Tool::Function { function } => {
                assert_eq!(translated_tool.input_schema, function.parameters);
            }
        }
    }
}
