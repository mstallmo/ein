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
            .map_err(|e| {
                let is_local = self.config.base_url.contains("localhost")
                    || self.config.base_url.contains("127.0.0.1");
                if is_local {
                    anyhow!(
                        "Could not connect to {}.\n\
                         Is the server running? (e.g. `ollama serve`)\n\
                         Details: {e}",
                        self.config.base_url
                    )
                } else {
                    anyhow!("Could not connect to {}: {e}", self.config.base_url)
                }
            })?;

        match resp.status {
            401 => {
                let msg = extract_api_error(&resp.body)
                    .unwrap_or_else(|| "Invalid or missing API key".to_owned());
                return Err(anyhow!(
                    "{msg}\n\n\
                     Set your api_key in ~/.ein/config.json under \
                     plugin_configs.ein_openrouter.config.api_key"
                ));
            }
            402 => {
                let msg = extract_api_error(&resp.body)
                    .unwrap_or_else(|| "Insufficient credits".to_owned());
                return Err(anyhow!(
                    "{msg}\n\nCheck your account balance at openrouter.ai."
                ));
            }
            404 => {
                let msg = extract_api_error(&resp.body)
                    .unwrap_or_else(|| "Resource not found".to_owned());
                let is_local = self.config.base_url.contains("localhost")
                    || self.config.base_url.contains("127.0.0.1");
                let hint = if is_local {
                    "\n\nThe model may not be downloaded yet. Try:\n  ollama pull <model-name>"
                        .to_owned()
                } else {
                    String::new()
                };
                return Err(anyhow!("{msg}{hint}"));
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

ein_plugin::export_model_client!(OpenRouterPlugin);
