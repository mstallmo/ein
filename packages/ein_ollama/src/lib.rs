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
}

fn default_base_url() -> String {
    "http://localhost:11434/v1".to_string()
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
        let mut req_builder = HttpRequest::post(url);
        if let Some(key) = &self.config.api_key {
            req_builder = req_builder.bearer_auth(key);
        }
        let resp = req_builder
            .json(&req)?
            .send()
            .map_err(|e| {
                if e.is::<RequestDeniedError>() {
                    anyhow!(
                        "Request to {} was blocked by the host allowlist.\n\
                         Add the Ollama host to ~/.ein/config.json:\n\
                         \n\
                         \"plugin_configs\": {{\n\
                         \"ein_ollama\": {{\n\
                             \"config\": {{\n\
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

        match resp.status {
            401 => {
                let msg = extract_api_error(&resp.body)
                    .unwrap_or_else(|| "Unauthorized".to_owned());
                return Err(anyhow!(
                    "{msg}\n\n\
                     Most local Ollama instances do not require authentication.\n\
                     If your deployment uses a bearer token, set it in \
                     ~/.ein/config.json under \
                     plugin_configs.ein_ollama.config.api_key"
                ));
            }
            402 => {
                let msg = extract_api_error(&resp.body)
                    .unwrap_or_else(|| "Payment required".to_owned());
                return Err(anyhow!("{msg}"));
            }
            404 => {
                let msg = extract_api_error(&resp.body)
                    .unwrap_or_else(|| "Model not found".to_owned());
                return Err(anyhow!(
                    "{msg}\n\n\
                     The model may not be downloaded yet. Run:\n\
                       ollama pull {}\n\
                     To list available models: ollama list",
                    req.model
                ));
            }
            s if !(200..300).contains(&s) => {
                let msg = extract_api_error(&resp.body).unwrap_or_else(|| format!("HTTP {s}"));
                return Err(anyhow!("API error: {msg}"));
            }
            _ => {}
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

ein_plugin::export_model_client!(OllamaPlugin);
