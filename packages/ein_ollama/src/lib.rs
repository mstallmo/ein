// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

// NOTE: This plugin uses `ein_model_client::HttpRequest` (backed by `wstd` via
// `ein_http`) rather than `reqwest` or `async-openai` directly.
//
// `reqwest` cannot be used from `wasm32-wasip2`: its `target_arch = "wasm32"`
// cfg unconditionally enables the browser (`js-sys`/`web-sys`) backend, which
// panics inside Wasmtime. `ein_http` wraps `wstd::http` instead, routing
// outgoing requests through `wasi:http/outgoing-handler`.

use anyhow::anyhow;
use ein_plugin::model_client::{
    CompletionRequest, CompletionResponse, ConstructableModelClientPlugin, HttpRequest,
    ModelClientPlugin, RequestDeniedError,
};
use serde::Deserialize;

fn extract_api_error(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error")?.get("message")?.as_str().map(str::to_owned))
}

/// Treat an absent or empty `api_key` field identically — Ollama does not
/// require authentication for local instances.
fn empty_string_as_none<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = Option::<String>::deserialize(d)?;
    Ok(s.filter(|v| !v.is_empty()))
}

#[derive(Deserialize)]
struct OllamaConfig {
    /// Bearer token for Ollama deployments that require authentication.
    /// Most local instances do not set this.
    #[serde(default, deserialize_with = "empty_string_as_none")]
    api_key: Option<String>,
    #[serde(default = "default_base_url")]
    base_url: String,
    /// Context window size passed to Ollama as `options.num_ctx`.
    /// Ollama's default is 2048 tokens, which is too small for multi-step
    /// agent sessions. Set this to 16384 or higher for code review workloads.
    #[serde(default)]
    num_ctx: Option<u32>,
}

fn default_base_url() -> String {
    "http://localhost:11434/v1".to_string()
}

fn inject_num_ctx(body: &mut serde_json::Value, num_ctx: Option<u32>) {
    if let Some(n) = num_ctx {
        body["options"] = serde_json::json!({ "num_ctx": n });
    }
}

fn map_http_error(status: u16, body: &str, model: &str) -> Option<anyhow::Error> {
    match status {
        401 => {
            let msg = extract_api_error(body).unwrap_or_else(|| "Unauthorized".to_owned());
            Some(anyhow!(
                "{msg}\n\n\
                 Most local Ollama instances do not require authentication.\n\
                 If your deployment uses a bearer token, set it in \
                 ~/.ein/config.json under \
                 plugin_configs.ein_ollama.params.api_key"
            ))
        }
        402 => {
            let msg =
                extract_api_error(body).unwrap_or_else(|| "Payment required".to_owned());
            Some(anyhow!("{msg}"))
        }
        404 => {
            let msg =
                extract_api_error(body).unwrap_or_else(|| "Model not found".to_owned());
            Some(anyhow!(
                "{msg}\n\n\
                 The model may not be downloaded yet. Run:\n\
                   ollama pull {model}\n\
                 To list available models: ollama list"
            ))
        }
        s if !(200..300).contains(&s) => {
            let msg = extract_api_error(body).unwrap_or_else(|| format!("HTTP {s}"));
            Some(anyhow!("API error: {msg}"))
        }
        _ => None,
    }
}

pub struct OllamaPlugin {
    config: OllamaConfig,
}

impl ConstructableModelClientPlugin for OllamaPlugin {
    fn new(config_json: &str) -> Self {
        let config: OllamaConfig =
            serde_json::from_str(config_json).expect("invalid Ollama config JSON");
        Self { config }
    }
}

impl ModelClientPlugin for OllamaPlugin {
    fn complete(&self, request_json: &str) -> anyhow::Result<String> {
        let req: CompletionRequest = serde_json::from_str(request_json)?;

        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );

        // CompletionRequest field names already match the OpenAI wire format,
        // which Ollama's /v1/chat/completions endpoint also accepts.
        // Inject Ollama-specific options (e.g. num_ctx) alongside the standard
        // fields if configured.
        let mut body = serde_json::to_value(&req)?;
        if self.config.num_ctx.is_some() {
            eprintln!("[ollama] setting num_ctx={}", self.config.num_ctx.unwrap());
        }
        inject_num_ctx(&mut body, self.config.num_ctx);

        let mut req_builder = HttpRequest::post(url);
        if let Some(key) = &self.config.api_key {
            req_builder = req_builder.bearer_auth(key);
        }
        let resp = req_builder.json(&body)?.send().map_err(|e| {
            if e.is::<RequestDeniedError>() {
                anyhow!(
                    "Request to {} was blocked by the host allowlist.\n\
                         Add the Ollama host to ~/.ein/config.json:\n\
                         \n\
                         \"plugin_configs\": {{\n\
                         \"ein_ollama\": {{\n\
                             \"params\": {{\n\
                             \"base_url\": \"{}\"\n\
                             }}\n\
                         }}\n\
                         }}",
                    self.config.base_url,
                    self.config.base_url,
                )
            } else {
                anyhow!(
                    "Could not connect to Ollama at {}.\n\
                         Is Ollama running? Start it with: ollama serve\n\
                         Details: {e}",
                    self.config.base_url
                )
            }
        })?;

        if let Some(e) = map_http_error(resp.status, &resp.body, &req.model) {
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
ein_plugin::export_model_client!(OllamaPlugin);

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
        let body = r#"{"error": {"message": "model not loaded", "type": "not_found"}}"#;
        assert_eq!(
            extract_api_error(body).as_deref(),
            Some("model not loaded")
        );
    }

    #[test]
    fn extract_api_error_missing_error_key() {
        assert!(extract_api_error(r#"{"choices": []}"#).is_none());
    }

    #[test]
    fn extract_api_error_malformed_json() {
        assert!(extract_api_error("not json").is_none());
    }

    // ---------------------------------------------------------------------------
    // OllamaConfig deserialization
    // ---------------------------------------------------------------------------

    #[test]
    fn config_default_base_url() {
        let cfg: OllamaConfig = serde_json::from_value(json!({})).unwrap();
        assert_eq!(cfg.base_url, "http://localhost:11434/v1");
    }

    #[test]
    fn config_absent_api_key_is_none() {
        let cfg: OllamaConfig = serde_json::from_value(json!({})).unwrap();
        assert!(cfg.api_key.is_none());
    }

    #[test]
    fn config_empty_api_key_treated_as_none() {
        let cfg: OllamaConfig = serde_json::from_value(json!({"api_key": ""})).unwrap();
        assert!(cfg.api_key.is_none());
    }

    #[test]
    fn config_valid_api_key() {
        let cfg: OllamaConfig = serde_json::from_value(json!({"api_key": "tok"})).unwrap();
        assert_eq!(cfg.api_key.as_deref(), Some("tok"));
    }

    #[test]
    fn config_num_ctx_absent_is_none() {
        let cfg: OllamaConfig = serde_json::from_value(json!({})).unwrap();
        assert!(cfg.num_ctx.is_none());
    }

    #[test]
    fn config_num_ctx_set() {
        let cfg: OllamaConfig = serde_json::from_value(json!({"num_ctx": 16384})).unwrap();
        assert_eq!(cfg.num_ctx, Some(16384));
    }

    // ---------------------------------------------------------------------------
    // inject_num_ctx
    // ---------------------------------------------------------------------------

    #[test]
    fn num_ctx_injected_into_body() {
        let mut body = json!({"model": "llama3", "messages": []});
        inject_num_ctx(&mut body, Some(8192));
        assert_eq!(body["options"]["num_ctx"], 8192);
    }

    #[test]
    fn num_ctx_not_injected_when_absent() {
        let mut body = json!({"model": "llama3", "messages": []});
        inject_num_ctx(&mut body, None);
        assert!(body.get("options").is_none());
    }

    // ---------------------------------------------------------------------------
    // map_http_error
    // ---------------------------------------------------------------------------

    #[test]
    fn map_http_error_401_contains_api_key_hint() {
        let err = map_http_error(401, "{}", "llama3").unwrap();
        let msg = err.to_string();
        assert!(msg.contains("api_key"), "expected api_key hint in: {msg}");
    }

    #[test]
    fn map_http_error_401_includes_api_message() {
        let body = r#"{"error": {"message": "Invalid token"}}"#;
        let err = map_http_error(401, body, "llama3").unwrap();
        assert!(err.to_string().contains("Invalid token"));
    }

    #[test]
    fn map_http_error_404_suggests_ollama_pull() {
        let err = map_http_error(404, "{}", "mistral").unwrap();
        let msg = err.to_string();
        assert!(msg.contains("ollama pull"), "expected 'ollama pull' in: {msg}");
        assert!(msg.contains("mistral"), "expected model name in: {msg}");
    }

    #[test]
    fn map_http_error_404_passes_through_api_message() {
        let body = r#"{"error": {"message": "model 'qwen' not found"}}"#;
        let err = map_http_error(404, body, "qwen").unwrap();
        assert!(err.to_string().contains("model 'qwen' not found"));
    }

    #[test]
    fn map_http_error_other_non_2xx() {
        let err = map_http_error(503, "{}", "llama3").unwrap();
        let msg = err.to_string();
        assert!(msg.contains("503"), "expected status code in: {msg}");
    }

    #[test]
    fn map_http_error_2xx_returns_none() {
        assert!(map_http_error(200, "{}", "llama3").is_none());
        assert!(map_http_error(201, "{}", "llama3").is_none());
    }

    // ---------------------------------------------------------------------------
    // Response body validation
    // ---------------------------------------------------------------------------

    #[test]
    fn valid_completion_response_parses() {
        let body = r#"{
            "id": "ollama-gen-1",
            "object": "chat.completion",
            "model": "llama3",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": "Hello!"}
            }],
            "usage": {"prompt_tokens": 8, "completion_tokens": 3, "total_tokens": 11}
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
