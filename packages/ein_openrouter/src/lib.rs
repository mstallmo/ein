// NOTE: This plugin routes HTTP through the `http_request` host syscall rather
// than using `reqwest` or `async-openai` directly. This is a current
// limitation of the `wasm32-wasip2` target: `reqwest`'s WASM backend uses
// `target_arch = "wasm32"` to enable the browser (`js-sys`/`web-sys`) backend,
// which matches `wasm32-wasip2` even though there is no JavaScript runtime
// inside Wasmtime, causing a panic at runtime.
//
// Once `reqwest` ships stable WASM component / WASIP2 support (tracked at
// https://github.com/seanmonstar/reqwest/issues/1766), this plugin can be
// rewritten to use `async-openai` directly for typed request building,
// streaming SSE responses, and automatic retries — without any changes to the
// plugin interface or the host.

use anyhow::anyhow;
use ein_model_client::{
    CompletionRequest, CompletionResponse, ConstructableModelClientPlugin, HttpRequest,
    ModelClientPlugin,
};
use serde::Deserialize;

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
            .send()?;

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

ein_model_client::export!(OpenRouterPlugin);
