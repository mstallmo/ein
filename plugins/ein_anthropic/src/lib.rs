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

mod anthropic_api;

use anthropic_api::{AnthropicRequest, translate_response};
use anyhow::anyhow;
use ein_plugin::model_client::{
    CompletionRequest, ConstructableModelClientPlugin, HttpRequest, ModelClientPlugin,
};
use serde::Deserialize;
use serde_json::Value;

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

        let body = AnthropicRequest::from(req);

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

#[cfg(target_arch = "wasm32")]
ein_plugin::export_model_client!(AnthropicPlugin);
