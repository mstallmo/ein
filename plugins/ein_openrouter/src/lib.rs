// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

// NOTE: This plugin uses `ein_model_client::HttpRequest` (backed by `wstd` via
// `ein_http`) rather than `reqwest` or `async-openai` directly.
//
// `reqwest` cannot be used from `wasm32-wasip2`: its `target_arch = "wasm32"`
// cfg unconditionally enables the browser (`js-sys`/`web-sys`) backend, which
// panics inside Wasmtime. `ein_http` wraps `wstd::http` instead, routing
// outgoing requests through `wasi:http/outgoing-handler`.
//
// `async-openai` could be adopted here once it supports a wasi:http / wstd
// backend, providing typed request building, streaming SSE, and automatic
// retries — without any changes to the plugin interface or the host.

use anyhow::anyhow;
use ein_plugin::model_client::{
    Choice, CompletionRequest, CompletionResponse, ConstructableModelClientPlugin, FinishReason,
    FunctionCall, HttpRequest, Message, ModelClientPlugin, Role, ToolCall, Usage, syscalls,
};
use serde::Deserialize;

fn extract_api_error(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error")?.get("message")?.as_str().map(str::to_owned))
}

fn map_http_error(status: u16, body: &str) -> Option<anyhow::Error> {
    match status {
        401 => {
            let msg =
                extract_api_error(body).unwrap_or_else(|| "Invalid or missing API key".to_owned());
            Some(anyhow!(
                "{msg}\n\n\
                 Set your api_key in ~/.ein/config.json under \
                 plugin_configs.ein_openrouter.params.api_key"
            ))
        }
        402 => {
            let msg = extract_api_error(body).unwrap_or_else(|| "Insufficient credits".to_owned());
            Some(anyhow!(
                "{msg}\n\nCheck your account balance at openrouter.ai."
            ))
        }
        404 => {
            let msg = extract_api_error(body).unwrap_or_else(|| "Resource not found".to_owned());
            Some(anyhow!("{msg}"))
        }
        s if !(200..300).contains(&s) => {
            let msg = extract_api_error(body).unwrap_or_default();
            Some(anyhow!("Status HTTP {s}. API error: {msg}"))
        }
        _ => None,
    }
}

#[derive(Deserialize)]
struct OpenRouterConfig {
    api_key: String,
    #[serde(default = "default_base_url")]
    base_url: String,
}

fn default_base_url() -> String {
    "https://openrouter.ai/api/v1".to_string()
}

/// WASM model client plugin for the OpenRouter API.
///
/// Forwards requests verbatim in OpenAI chat-completions format (no
/// translation required) and validates that the response parses as a
/// [`CompletionResponse`] before returning it.
pub struct OpenRouterPlugin {
    config: OpenRouterConfig,
}

impl ConstructableModelClientPlugin for OpenRouterPlugin {
    fn new(config_json: &str) -> Self {
        let config: OpenRouterConfig =
            serde_json::from_str(config_json).expect("invalid OpenRouter config JSON");
        Self { config }
    }
}

impl ModelClientPlugin for OpenRouterPlugin {
    fn complete(&self, request_json: &str) -> anyhow::Result<String> {
        let req: CompletionRequest = serde_json::from_str(request_json)?;

        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );

        // CompletionRequest field names already match the OpenAI wire format;
        // add the streaming switches. `include_usage` makes the provider send a
        // final chunk carrying token counts.
        let mut body = serde_json::to_value(&req)?;
        body["stream"] = serde_json::Value::Bool(true);
        body["stream_options"] = serde_json::json!({ "include_usage": true });

        // Reassemble the streamed SSE into a single CompletionResponse, emitting
        // each text chunk to the host as it arrives (`syscalls::on_content_delta`).
        let mut acc = StreamAccumulator::default();
        let mut raw = Vec::new(); // full raw body, kept for error reporting
        let mut pending = Vec::new(); // bytes not yet split into a complete line

        let status = HttpRequest::post(url)
            .bearer_auth(&self.config.api_key)
            .json(&body)?
            .send_streaming(|chunk| {
                raw.extend_from_slice(chunk);
                pending.extend_from_slice(chunk);
                // Split on '\n' (safe at the byte level — never inside a UTF-8
                // multibyte sequence) and feed each complete line to the parser,
                // forwarding text chunks to the host as they arrive.
                while let Some(nl) = pending.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=nl).collect();
                    let line = String::from_utf8_lossy(&line[..line.len() - 1]);
                    acc.handle_sse_line(line.trim(), &mut |delta| {
                        syscalls::on_content_delta(delta)
                    });
                }
            })
            .map_err(|e| anyhow!("Could not connect to {}: {e}", self.config.base_url))?;

        // A non-2xx streaming response is a normal JSON error body, not SSE.
        if !(200..300).contains(&status) {
            let body = String::from_utf8_lossy(&raw);
            if let Some(e) = map_http_error(status, &body) {
                return Err(e);
            }
        }

        Ok(serde_json::to_string(&acc.into_response())?)
    }
}

/// Accumulates the fields of a streamed chat completion across SSE chunks into a
/// single [`CompletionResponse`], mirroring how the non-streaming endpoint would
/// have returned it in one shot.
#[derive(Default)]
struct StreamAccumulator {
    content: String,
    /// Tool calls indexed by their streaming `index`; fragments arrive across
    /// chunks (id/name once, `arguments` in pieces) and are joined here.
    tool_calls: Vec<ToolCallAcc>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
    /// An `error` object seen mid-stream (some providers stream errors as SSE).
    error: Option<serde_json::Value>,
}

#[derive(Default)]
struct ToolCallAcc {
    id: String,
    name: String,
    arguments: String,
}

impl StreamAccumulator {
    /// Process one SSE line (`data: {…}`, `data: [DONE]`, comments, or blanks).
    /// Each text delta is passed to `on_delta` immediately (the caller forwards
    /// it to the host); everything else is accumulated for the final response.
    /// `on_delta` is injected so the reassembly logic is testable off-wasm.
    fn handle_sse_line(&mut self, line: &str, on_delta: &mut impl FnMut(&str)) {
        let Some(data) = line.strip_prefix("data:") else {
            return; // comments, event:/id: lines, and blank separators
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        let Ok(chunk) = serde_json::from_str::<StreamChunk>(data) else {
            return; // ignore keep-alives / unparseable chunks
        };

        if chunk.error.is_some() {
            self.error = chunk.error;
            return;
        }
        if chunk.usage.is_some() {
            self.usage = chunk.usage;
        }
        let Some(choice) = chunk.choices.into_iter().next() else {
            return;
        };
        if let Some(reason) = choice.finish_reason {
            self.finish_reason = Some(reason);
        }
        if let Some(content) = choice.delta.content
            && !content.is_empty()
        {
            // Forward the chunk to the host, then accumulate for history.
            on_delta(&content);
            self.content.push_str(&content);
        }
        for tool_call in choice.delta.tool_calls.unwrap_or_default() {
            if self.tool_calls.len() <= tool_call.index {
                self.tool_calls
                    .resize_with(tool_call.index + 1, Default::default);
            }
            let acc = &mut self.tool_calls[tool_call.index];
            if let Some(id) = tool_call.id.filter(|id| !id.is_empty()) {
                acc.id = id;
            }
            if let Some(function) = tool_call.function {
                if let Some(name) = function.name.filter(|name| !name.is_empty()) {
                    acc.name = name;
                }
                if let Some(arguments) = function.arguments {
                    acc.arguments.push_str(&arguments);
                }
            }
        }
    }

    /// Build the final [`CompletionResponse`] from the accumulated fields.
    fn into_response(self) -> CompletionResponse {
        let tool_calls: Vec<ToolCall> = self
            .tool_calls
            .into_iter()
            .enumerate()
            .filter(|(_, tc)| !tc.name.is_empty())
            .map(|(index, tc)| ToolCall::Function {
                id: tc.id,
                index,
                function: FunctionCall {
                    name: tc.name,
                    arguments: tc.arguments,
                },
            })
            .collect();

        let finish_reason = match self.finish_reason.as_deref() {
            Some("tool_calls") => FinishReason::ToolCalls,
            Some("stop") => FinishReason::Stop,
            Some(_) => FinishReason::Unsupported,
            // A stream that ended without a reason but produced tool calls is a
            // tool-call turn; otherwise treat it as a normal stop.
            None if !tool_calls.is_empty() => FinishReason::ToolCalls,
            None => FinishReason::Stop,
        };

        let message = Message {
            role: Role::Assistant,
            content: Some(self.content).filter(|c| !c.is_empty()),
            tool_calls: Some(tool_calls).filter(|tcs| !tcs.is_empty()),
            tool_call_id: None,
        };

        CompletionResponse {
            choices: vec![Choice {
                index: Some(0),
                finish_reason,
                message,
            }],
            usage: self.usage,
            error: self.error,
        }
    }
}

/// A single SSE chunk from the OpenAI-compatible streaming endpoint.
#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<StreamChoice>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Deserialize)]
struct StreamToolCall {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<StreamFunction>,
}

#[derive(Deserialize)]
struct StreamFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[cfg(target_arch = "wasm32")]
ein_plugin::export_model_client!(OpenRouterPlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use ein_plugin::model_client::CompletionResponse;
    use serde_json::json;

    // ---------------------------------------------------------------------------
    // extract_api_error
    // ---------------------------------------------------------------------------

    #[test]
    fn extract_api_error_present() {
        let body = r#"{"error": {"message": "You exceeded your quota", "type": "quota_exceeded"}}"#;
        assert_eq!(
            extract_api_error(body).as_deref(),
            Some("You exceeded your quota")
        );
    }

    #[test]
    fn extract_api_error_missing_error_key() {
        assert!(extract_api_error(r#"{"choices": []}"#).is_none());
    }

    #[test]
    fn extract_api_error_missing_message_key() {
        assert!(extract_api_error(r#"{"error": {"type": "server_error"}}"#).is_none());
    }

    #[test]
    fn extract_api_error_malformed_json() {
        assert!(extract_api_error("not json at all").is_none());
    }

    // ---------------------------------------------------------------------------
    // OpenRouterConfig deserialization
    // ---------------------------------------------------------------------------

    #[test]
    fn config_default_base_url() {
        let cfg: OpenRouterConfig =
            serde_json::from_value(json!({"api_key": "sk-or-test"})).unwrap();
        assert_eq!(cfg.api_key, "sk-or-test");
        assert_eq!(cfg.base_url, "https://openrouter.ai/api/v1");
    }

    #[test]
    fn config_custom_base_url() {
        let cfg: OpenRouterConfig = serde_json::from_value(json!({
            "api_key": "sk-or-test",
            "base_url": "https://my-proxy.example.com/v1"
        }))
        .unwrap();
        assert_eq!(cfg.base_url, "https://my-proxy.example.com/v1");
    }

    // ---------------------------------------------------------------------------
    // map_http_error
    // ---------------------------------------------------------------------------

    #[test]
    fn map_http_error_401_contains_api_key_hint() {
        let err = map_http_error(401, "{}").unwrap();
        let msg = err.to_string();
        assert!(msg.contains("api_key"), "expected api_key hint in: {msg}");
    }

    #[test]
    fn map_http_error_401_includes_api_message() {
        let body = r#"{"error": {"message": "Incorrect API key provided"}}"#;
        let err = map_http_error(401, body).unwrap();
        assert!(err.to_string().contains("Incorrect API key provided"));
    }

    #[test]
    fn map_http_error_402_mentions_credits_and_balance() {
        let err = map_http_error(402, "{}").unwrap();
        let msg = err.to_string();
        assert!(
            msg.contains("openrouter.ai"),
            "expected openrouter.ai link in: {msg}"
        );
    }

    #[test]
    fn map_http_error_404_passes_through_api_message() {
        let body = r#"{"error": {"message": "No endpoints found for model"}}"#;
        let err = map_http_error(404, body).unwrap();
        assert!(err.to_string().contains("No endpoints found for model"));
    }

    #[test]
    fn map_http_error_404_fallback_when_no_api_message() {
        let err = map_http_error(404, "{}").unwrap();
        assert!(err.to_string().contains("Resource not found"));
    }

    #[test]
    fn map_http_error_other_non_2xx() {
        let err = map_http_error(503, "{}").unwrap();
        let msg = err.to_string();
        assert!(msg.contains("503"), "expected status code in: {msg}");
    }

    #[test]
    fn map_http_error_2xx_returns_none() {
        assert!(map_http_error(200, "{}").is_none());
        assert!(map_http_error(201, "{}").is_none());
    }

    // ---------------------------------------------------------------------------
    // Response body validation
    // ---------------------------------------------------------------------------

    #[test]
    fn valid_completion_response_parses() {
        let body = r#"{
            "id": "gen-abc",
            "object": "chat.completion",
            "model": "anthropic/claude-haiku-4-5",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": "Hello!"}
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        }"#;
        let resp: Result<CompletionResponse, _> = serde_json::from_str(body);
        assert!(resp.is_ok(), "expected valid response to parse: {:?}", resp);
    }

    #[test]
    fn invalid_response_body_returns_error() {
        let resp: Result<CompletionResponse, _> = serde_json::from_str("not valid json");
        assert!(resp.is_err());
    }

    // ---------------------------------------------------------------------------
    // SSE stream reassembly
    // ---------------------------------------------------------------------------

    /// Feed synthetic SSE lines through the accumulator, returning the final
    /// reassembled response and the deltas emitted in order.
    fn feed(lines: &[&str]) -> (CompletionResponse, Vec<String>) {
        let mut acc = StreamAccumulator::default();
        let mut deltas = Vec::new();
        for line in lines {
            acc.handle_sse_line(line, &mut |d| deltas.push(d.to_string()));
        }
        (acc.into_response(), deltas)
    }

    #[test]
    fn reassembles_streamed_text_and_emits_each_chunk() {
        let (resp, deltas) = feed(&[
            r#"data: {"choices":[{"delta":{"role":"assistant","content":"Hel"}}]}"#,
            r#"data: {"choices":[{"delta":{"content":"lo"}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            "data: [DONE]",
        ]);
        // Each chunk is emitted live, then the full text is assembled for history.
        assert_eq!(deltas, vec!["Hel".to_string(), "lo".to_string()]);
        let choice = &resp.choices[0];
        assert_eq!(choice.message.content.as_deref(), Some("Hello"));
        assert!(matches!(choice.finish_reason, FinishReason::Stop));
        assert!(choice.message.tool_calls.is_none());
    }

    #[test]
    fn reassembles_tool_calls_fragmented_across_chunks() {
        let (resp, deltas) = feed(&[
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather","arguments":"{\"ci"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"ty\":\"SF\"}"}}]}}]}"#,
            r#"data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "data: [DONE]",
        ]);
        assert!(deltas.is_empty(), "a pure tool-call turn streams no text");
        let choice = &resp.choices[0];
        assert!(matches!(choice.finish_reason, FinishReason::ToolCalls));
        let tool_calls = choice.message.tool_calls.as_ref().expect("tool calls");
        assert_eq!(tool_calls.len(), 1);
        let ToolCall::Function { id, function, .. } = &tool_calls[0];
        assert_eq!(id, "call_1");
        assert_eq!(function.name, "get_weather");
        // The `arguments` fragments are joined into one valid JSON string.
        assert_eq!(function.arguments, r#"{"city":"SF"}"#);
    }

    #[test]
    fn captures_usage_and_ignores_comments_and_keepalives() {
        let (resp, deltas) = feed(&[
            ": OPENROUTER PROCESSING", // SSE comment — no `data:` prefix
            r#"data: {"choices":[{"delta":{"content":"hi"}}]}"#,
            r#"data: {"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}"#,
            "data: [DONE]",
        ]);
        assert_eq!(deltas, vec!["hi".to_string()]);
        let usage = resp.usage.expect("usage captured from the final chunk");
        assert_eq!(usage.total_tokens, 12);
    }

    #[test]
    fn surfaces_a_mid_stream_error() {
        let (resp, deltas) = feed(&[r#"data: {"error":{"message":"rate limited"}}"#]);
        assert!(deltas.is_empty());
        assert!(
            resp.error.is_some(),
            "a streamed error is surfaced on the response so the loop can report it"
        );
    }
}
