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
    ModelClientPlugin, RequestDeniedError,
};
use serde::Deserialize;

fn map_http_error(status: u16, body: &str) -> Option<anyhow::Error> {
    match status {
        401 => {
            let msg =
                extract_api_error(body).unwrap_or_else(|| "Invalid or missing API key".to_owned());
            Some(anyhow!(
                "{msg}\n\n\
                 Set your api_key in ~/.ein/config.json under \
                 plugin_configs.ein_openai.params.api_key"
            ))
        }
        429 => {
            let msg = extract_api_error(body).unwrap_or_else(|| "Rate limit exceeded".to_owned());
            Some(anyhow!(
                "{msg}\n\nCheck your usage and limits at platform.openai.com."
            ))
        }
        500 | 503 => {
            let msg = extract_api_error(body)
                .unwrap_or_else(|| format!("OpenAI service error (HTTP {status})"));
            Some(anyhow!("{msg}\n\nTry again in a moment."))
        }
        s if !(200..300).contains(&s) => {
            let msg = extract_api_error(body).unwrap_or_else(|| format!("HTTP {s}"));
            Some(anyhow!("API error: {msg}"))
        }
        _ => None,
    }
}

fn extract_api_error(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error")?.get("message")?.as_str().map(str::to_owned))
}

#[derive(Deserialize)]
struct OpenAIConfig {
    api_key: String,
    #[serde(default = "default_base_url")]
    base_url: String,
    /// OpenAI organization ID. When set, sent as the `OpenAI-Organization`
    /// header. Required if your API key belongs to multiple organizations.
    #[serde(default)]
    organization: Option<String>,
}

fn default_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}

pub struct OpenAIPlugin {
    config: OpenAIConfig,
}

impl ConstructableModelClientPlugin for OpenAIPlugin {
    fn new(config_json: &str) -> Self {
        let config: OpenAIConfig =
            serde_json::from_str(config_json).expect("invalid OpenAI config JSON");
        Self { config }
    }
}

impl ModelClientPlugin for OpenAIPlugin {
    fn complete(&self, request_json: &str) -> anyhow::Result<String> {
        let req: CompletionRequest = serde_json::from_str(request_json)?;

        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );

        // CompletionRequest field names already match the OpenAI wire format.
        let mut req_builder = HttpRequest::post(url).bearer_auth(&self.config.api_key);

        if let Some(org) = &self.config.organization {
            req_builder = req_builder.header("OpenAI-Organization", org);
        }

        let resp = req_builder.json(&req)?.send().map_err(|e| {
            if e.is::<RequestDeniedError>() {
                anyhow!(
                    "Request blocked by host allowlist.\n\
                         Add the OpenAI host to allowed_hosts in ~/.ein/config.json."
                )
            } else {
                anyhow!("Failed to connect to OpenAI API: {e}")
            }
        })?;

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
ein_plugin::export_model_client!(OpenAIPlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use ein_plugin::model_client::CompletionResponse;
    use serde_json::json;

    // ---------------------------------------------------------------------------
    // extract_api_error
    // ---------------------------------------------------------------------------

    #[test]
    fn test_extract_api_error_present() {
        let body = r#"{"error": {"message": "You exceeded your quota", "type": "quota_exceeded"}}"#;
        assert_eq!(
            extract_api_error(body).as_deref(),
            Some("You exceeded your quota")
        );
    }

    #[test]
    fn test_extract_api_error_missing_error_key() {
        let body = r#"{"choices": []}"#;
        assert!(extract_api_error(body).is_none());
    }

    #[test]
    fn test_extract_api_error_missing_message_key() {
        let body = r#"{"error": {"type": "server_error"}}"#;
        assert!(extract_api_error(body).is_none());
    }

    #[test]
    fn test_extract_api_error_malformed_json() {
        assert!(extract_api_error("not json at all").is_none());
    }

    // ---------------------------------------------------------------------------
    // OpenAIConfig deserialization
    // ---------------------------------------------------------------------------

    #[test]
    fn test_config_defaults() {
        let cfg: OpenAIConfig = serde_json::from_value(json!({"api_key": "sk-test"})).unwrap();
        assert_eq!(cfg.api_key, "sk-test");
        assert_eq!(cfg.base_url, "https://api.openai.com/v1");
        assert!(cfg.organization.is_none());
    }

    #[test]
    fn test_config_with_organization() {
        let cfg: OpenAIConfig = serde_json::from_value(json!({
            "api_key": "sk-test",
            "organization": "org-abc123"
        }))
        .unwrap();
        assert_eq!(cfg.organization.as_deref(), Some("org-abc123"));
    }

    #[test]
    fn test_config_custom_base_url() {
        let cfg: OpenAIConfig = serde_json::from_value(json!({
            "api_key": "sk-test",
            "base_url": "https://my-proxy.example.com/v1"
        }))
        .unwrap();
        assert_eq!(cfg.base_url, "https://my-proxy.example.com/v1");
    }

    // ---------------------------------------------------------------------------
    // map_http_error
    // ---------------------------------------------------------------------------

    #[test]
    fn test_map_http_error_401() {
        let err = map_http_error(401, "{}").unwrap();
        let msg = err.to_string();
        assert!(msg.contains("api_key"), "expected api_key hint in: {msg}");
    }

    #[test]
    fn test_map_http_error_401_with_api_message() {
        let body = r#"{"error": {"message": "Incorrect API key provided"}}"#;
        let err = map_http_error(401, body).unwrap();
        let msg = err.to_string();
        assert!(
            msg.contains("Incorrect API key provided"),
            "expected API message in: {msg}"
        );
    }

    #[test]
    fn test_map_http_error_429() {
        let err = map_http_error(429, "{}").unwrap();
        let msg = err.to_string();
        assert!(msg.contains("Rate limit"), "expected rate limit in: {msg}");
        assert!(
            msg.contains("platform.openai.com"),
            "expected platform link in: {msg}"
        );
    }

    #[test]
    fn test_map_http_error_500() {
        let err = map_http_error(500, "{}").unwrap();
        let msg = err.to_string();
        assert!(
            msg.contains("service error"),
            "expected service error in: {msg}"
        );
    }

    #[test]
    fn test_map_http_error_503() {
        let err = map_http_error(503, "{}").unwrap();
        let msg = err.to_string();
        assert!(
            msg.contains("service error"),
            "expected service error in: {msg}"
        );
    }

    #[test]
    fn test_map_http_error_other_non_2xx() {
        let err = map_http_error(422, "{}").unwrap();
        let msg = err.to_string();
        assert!(msg.contains("API error"), "expected 'API error' in: {msg}");
    }

    #[test]
    fn test_map_http_error_success() {
        assert!(map_http_error(200, "{}").is_none());
    }

    // ---------------------------------------------------------------------------
    // Response body validation
    // ---------------------------------------------------------------------------

    #[test]
    fn test_valid_completion_response_parses() {
        let body = r#"{
            "id": "chatcmpl-abc",
            "object": "chat.completion",
            "model": "gpt-4o",
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
    fn test_invalid_response_body_returns_error() {
        let resp: Result<CompletionResponse, _> = serde_json::from_str("not valid json");
        assert!(resp.is_err());
    }
}
