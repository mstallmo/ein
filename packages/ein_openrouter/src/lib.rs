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
    CompletionRequest, CompletionResponse, ConstructableModelClientPlugin, HttpRequest,
    ModelClientPlugin,
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
            let msg =
                extract_api_error(body).unwrap_or_else(|| "Insufficient credits".to_owned());
            Some(anyhow!(
                "{msg}\n\nCheck your account balance at openrouter.ai."
            ))
        }
        404 => {
            let msg =
                extract_api_error(body).unwrap_or_else(|| "Resource not found".to_owned());
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

        // CompletionRequest field names already match the OpenAI wire format.
        let resp = HttpRequest::post(url)
            .bearer_auth(&self.config.api_key)
            .json(&req)?
            .send()
            .map_err(|e| anyhow!("Could not connect to {}: {e}", self.config.base_url))?;

        if let Some(e) = map_http_error(resp.status, &resp.body) {
            return Err(e);
        }

        // Validate the body parses as CompletionResponse before returning.
        let _: CompletionResponse = serde_json::from_str(&resp.body).map_err(|e| {
            anyhow!(
                "unexpected response (HTTP {}): {e}\nbody: {}",
                resp.status,
                resp.body
            )
        })?;

        Ok(resp.body)
    }
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
        assert!(msg.contains("openrouter.ai"), "expected openrouter.ai link in: {msg}");
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
}
