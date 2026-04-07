// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

// NOTE: This plugin translates between the OpenAI chat completions format used
// internally by Ein and the Anthropic Messages API format.
//
// Key translation points:
//   Request:  system messages → top-level "system" field
//             assistant tool_calls → content array with "tool_use" blocks
//             role:"tool" messages → batched into user messages with "tool_result" blocks
//             tool definitions → "input_schema" instead of "parameters"
//   Response: content array → text joined into content + tool_use → tool_calls
//             stop_reason "end_turn"/"tool_use" → FinishReason::Stop/ToolCalls
//             input_tokens/output_tokens → prompt_tokens/completion_tokens

use anyhow::anyhow;
use ein_plugin::model_client::{
    Choice, CompletionRequest, CompletionResponse, ConstructableModelClientPlugin, FinishReason,
    FunctionCall, HttpRequest, Message, ModelClientPlugin, Role, ToolCall, Usage,
};
use serde::Deserialize;
use serde_json::{Value, json};

fn extract_api_error(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("error")?.get("message")?.as_str().map(str::to_owned))
}

#[derive(Deserialize)]
struct AnthropicConfig {
    api_key: String,
    #[serde(default = "default_api_version")]
    api_version: String,
    #[serde(default = "default_base_url")]
    base_url: String,
}

fn default_api_version() -> String {
    "2023-06-01".to_string()
}

fn default_base_url() -> String {
    "https://api.anthropic.com".to_string()
}

pub struct AnthropicPlugin {
    config: AnthropicConfig,
}

impl ConstructableModelClientPlugin for AnthropicPlugin {
    fn new(config_json: &str) -> Self {
        let config: AnthropicConfig =
            serde_json::from_str(config_json).expect("invalid Anthropic config JSON");
        Self { config }
    }
}

impl ModelClientPlugin for AnthropicPlugin {
    fn complete(&self, request_json: &str) -> anyhow::Result<String> {
        let req: CompletionRequest = serde_json::from_str(request_json)?;

        let url = format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'));

        let body = translate_request(&req)?;

        let resp = HttpRequest::post(url)
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", &self.config.api_version)
            .json(&body)?
            .send()
            .map_err(|e| anyhow!("Could not connect to Anthropic API: {e}"))?;

        match resp.status {
            401 => {
                let msg = extract_api_error(&resp.body)
                    .unwrap_or_else(|| "Invalid or missing API key".to_owned());
                return Err(anyhow!(
                    "{msg}\n\n\
                     Set your api_key in ~/.ein/config.json under \
                     plugin_configs.ein_anthropic.params.api_key"
                ));
            }
            429 => {
                let msg = extract_api_error(&resp.body)
                    .unwrap_or_else(|| "Rate limit exceeded".to_owned());
                return Err(anyhow!("{msg}\n\nPlease wait before retrying."));
            }
            529 => {
                return Err(anyhow!(
                    "Anthropic API is temporarily overloaded. Please retry shortly."
                ));
            }
            s if !(200..300).contains(&s) => {
                let msg = extract_api_error(&resp.body).unwrap_or_else(|| format!("HTTP {s}"));
                return Err(anyhow!("Anthropic API error: {msg}"));
            }
            _ => {}
        }

        let completion = translate_response(&resp.body)?;
        Ok(serde_json::to_string(&completion)?)
    }
}

/// Convert an OpenAI-format `CompletionRequest` into an Anthropic Messages API request body.
fn translate_request(req: &CompletionRequest) -> anyhow::Result<Value> {
    // Extract system messages and join them into the top-level "system" field.
    let system_text: String = req
        .messages
        .iter()
        .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
        .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
        .collect::<Vec<_>>()
        .join("\n\n");

    let non_system: Vec<&Value> = req
        .messages
        .iter()
        .filter(|m| m.get("role").and_then(|r| r.as_str()) != Some("system"))
        .collect();

    let anthropic_messages = translate_messages(&non_system);
    let anthropic_tools = translate_tools(&req.tools);

    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "messages": anthropic_messages,
    });

    if !system_text.is_empty() {
        body["system"] = Value::String(system_text);
    }

    if !anthropic_tools.is_empty() {
        body["tools"] = Value::Array(anthropic_tools);
    }

    Ok(body)
}

/// Convert OpenAI-format tool definitions to Anthropic format.
///
/// OpenAI: `{"type":"function","function":{"name","description","parameters":{...}}}`
/// Anthropic: `{"name","description","input_schema":{...}}`
fn translate_tools(oai_tools: &[Value]) -> Vec<Value> {
    oai_tools
        .iter()
        .filter_map(|t| {
            let func = t.get("function")?;
            let name = func.get("name")?.clone();
            let description = func.get("description")?.clone();
            let input_schema = func.get("parameters")?.clone();
            Some(json!({
                "name": name,
                "description": description,
                "input_schema": input_schema,
            }))
        })
        .collect()
}

/// Convert a slice of non-system OpenAI messages to Anthropic messages.
///
/// The main challenges:
/// - `role:"tool"` messages (tool results) must be batched into a single
///   `role:"user"` message with an array of `tool_result` content blocks.
/// - `role:"assistant"` messages with `tool_calls` must emit `tool_use` content blocks.
/// - `arguments` (raw JSON string in OpenAI) becomes `input` (parsed object in Anthropic).
fn translate_messages(messages: &[&Value]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut pending_tool_results: Vec<Value> = Vec::new();

    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");

        if role == "tool" {
            let tool_use_id = msg
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content = msg
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            pending_tool_results.push(json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
            }));
        } else {
            // Flush any accumulated tool results before a non-tool message.
            if !pending_tool_results.is_empty() {
                out.push(json!({
                    "role": "user",
                    "content": pending_tool_results.drain(..).collect::<Vec<_>>(),
                }));
            }

            match role {
                "user" => {
                    out.push(json!({
                        "role": "user",
                        "content": msg.get("content").cloned().unwrap_or(Value::String(String::new())),
                    }));
                }
                "assistant" => {
                    let mut content_blocks: Vec<Value> = Vec::new();

                    // Optional text content.
                    if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
                        if !text.is_empty() {
                            content_blocks.push(json!({ "type": "text", "text": text }));
                        }
                    }

                    // Tool calls → tool_use blocks.
                    if let Some(tool_calls) = msg.get("tool_calls").and_then(|tc| tc.as_array()) {
                        for tc in tool_calls {
                            let id = tc
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let name = tc
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            // arguments is a raw JSON string; Anthropic wants a parsed object.
                            let input: Value = tc
                                .get("function")
                                .and_then(|f| f.get("arguments"))
                                .and_then(|v| v.as_str())
                                .and_then(|s| serde_json::from_str(s).ok())
                                .unwrap_or(Value::Object(Default::default()));
                            content_blocks.push(json!({
                                "type": "tool_use",
                                "id": id,
                                "name": name,
                                "input": input,
                            }));
                        }
                    }

                    // Anthropic requires a non-empty content array for assistant turns.
                    if content_blocks.is_empty() {
                        content_blocks.push(json!({ "type": "text", "text": "" }));
                    }

                    out.push(json!({
                        "role": "assistant",
                        "content": content_blocks,
                    }));
                }
                _ => {} // Unknown roles are skipped.
            }
        }
    }

    // Flush any trailing tool results at the end of the message list.
    if !pending_tool_results.is_empty() {
        out.push(json!({
            "role": "user",
            "content": pending_tool_results,
        }));
    }

    out
}

/// Convert an Anthropic Messages API response body into a `CompletionResponse`.
fn translate_response(body: &str) -> anyhow::Result<CompletionResponse> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| anyhow!("failed to parse Anthropic response: {e}\nbody: {body}"))?;

    // Anthropic wraps errors as {"type":"error","error":{...}}.
    if v.get("type").and_then(|t| t.as_str()) == Some("error") {
        return Ok(CompletionResponse {
            choices: vec![],
            usage: None,
            error: Some(v),
        });
    }

    let content_arr = v
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| anyhow!("missing content array in Anthropic response\nbody: {body}"))?;

    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();

    for (idx, block) in content_arr.iter().enumerate() {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                    text_parts.push(t.to_string());
                }
            }
            Some("tool_use") => {
                let id = block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // input is a parsed object; serialize back to a JSON string for FunctionCall.arguments.
                let arguments = block
                    .get("input")
                    .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string()))
                    .unwrap_or_else(|| "{}".to_string());
                tool_calls.push(ToolCall::Function {
                    id,
                    index: idx,
                    function: FunctionCall { name, arguments },
                });
            }
            _ => {}
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

    let finish_reason = match v.get("stop_reason").and_then(|r| r.as_str()) {
        Some("tool_use") => FinishReason::ToolCalls,
        _ => FinishReason::Stop,
    };

    let usage = v.get("usage").map(|u| Usage {
        prompt_tokens: u.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
        completion_tokens: u.get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
        total_tokens: (u.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0)
            + u.get("output_tokens").and_then(|v| v.as_i64()).unwrap_or(0))
            as i32,
    });

    Ok(CompletionResponse {
        choices: vec![Choice {
            index: 0,
            finish_reason,
            message: Message {
                role: Role::Assistant,
                content,
                tool_calls: tool_calls_opt,
                tool_call_id: None,
            },
        }],
        usage,
        error: None,
    })
}

ein_plugin::export_model_client!(AnthropicPlugin);
