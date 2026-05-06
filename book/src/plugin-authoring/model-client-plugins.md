# Writing a Model Client Plugin

A model client plugin adapts Ein's internal request format to a specific LLM API. It implements two traits from `ein_plugin::model_client`:

- **`ConstructableModelClientPlugin`** — provides `new(config_json)` called once per session with the plugin's config
- **`ModelClientPlugin`** — the core interface: a single `complete` method

## The traits

```rust
pub trait ConstructableModelClientPlugin: ModelClientPlugin {
    /// Called once per session with the plugin's `params` from config.json,
    /// serialized as a JSON string.
    fn new(config_json: &str) -> Self;
}

pub trait ModelClientPlugin: Send + Sync {
    /// Receives a serialized CompletionRequest, returns a serialized
    /// CompletionResponse (or an error string).
    fn complete(&self, request_json: &str) -> anyhow::Result<String>;
}
```

Unlike tool plugins, model client plugins **do** receive config at construction time. The `config_json` string is the `params` object from the plugin's entry in `~/.ein/config.json`, serialized to a JSON string.

## Key types

These are re-exported from `ein_plugin::model_client`:

```rust
use ein_plugin::model_client::{
    CompletionRequest,   // deserialized from request_json
    CompletionResponse,  // serialized and returned from complete()
    HttpRequest,         // WASM HTTP client
};
```

### `CompletionRequest`

```rust
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
    pub max_tokens: i32,
}
```

### `CompletionResponse`

```rust
pub struct CompletionResponse {
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
    pub error: Option<serde_json::Value>,
}
```

If the upstream API returns an error, set `error` rather than returning `Err`. If the whole request fails (network error, auth failure), return `Err` — the server emits an `AgentError` event and preserves the session for retry.

### `HttpRequest`

WASM plugins cannot use `reqwest` or `tokio` directly. Use `HttpRequest`, which is backed by `wasi:http/outgoing-handler`:

```rust
let resp = HttpRequest::post("https://api.example.com/v1/chat/completions")
    .bearer_auth(&self.config.api_key)
    .json(&body)?
    .send()?;

if resp.is_success() {
    // resp.body is a String
    let data: MyResponseType = serde_json::from_str(&resp.body)?;
}
```

Available methods: `HttpRequest::get`, `post`, `put`, `patch`, `delete`.

Builder API:

| Method | Description |
|--------|-------------|
| `.header(key, value)` | Add an arbitrary HTTP header |
| `.bearer_auth(token)` | Set `Authorization: Bearer <token>` |
| `.content_type_json()` | Set `Content-Type: application/json` |
| `.json(&value)` | Serialize value and set as body |
| `.body(string)` | Set raw string body |
| `.send()` | Dispatch the request |

## Network access

The hostname derived from the plugin's `base_url` is automatically allowlisted by the server — you don't need to add it to `allowed_hosts`. If your plugin needs to contact additional hosts (e.g., a token refresh endpoint), add them to `plugin_configs.<name>.allowed_hosts` in `~/.ein/config.json`.

## Exporting the plugin

```rust
#[cfg(target_arch = "wasm32")]
ein_plugin::export_model_client!(MyPlugin);
```

---

## Worked example: a minimal OpenAI-compatible adapter

This example connects to any OpenAI-compatible API. Many open-source model servers (Ollama, vLLM, LM Studio, llama.cpp server) expose an OpenAI-compatible chat completions endpoint, so this pattern covers a lot of ground.

```rust
use anyhow::anyhow;
use ein_plugin::model_client::{
    CompletionRequest, CompletionResponse, ConstructableModelClientPlugin,
    HttpRequest, ModelClientPlugin,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct MyConfig {
    api_key: String,
    #[serde(default = "default_base_url")]
    base_url: String,
    #[serde(default)]
    model: String,
}

fn default_base_url() -> String {
    "http://localhost:8080/v1".to_string()
}

pub struct MyModelClient {
    config: MyConfig,
}

impl ConstructableModelClientPlugin for MyModelClient {
    fn new(config_json: &str) -> Self {
        let config: MyConfig = serde_json::from_str(config_json)
            .expect("invalid plugin config JSON");
        Self { config }
    }
}

impl ModelClientPlugin for MyModelClient {
    fn complete(&self, request_json: &str) -> anyhow::Result<String> {
        let mut req: CompletionRequest = serde_json::from_str(request_json)?;

        // Override the model if the config specifies one.
        if !self.config.model.is_empty() {
            req.model = self.config.model.clone();
        }

        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );

        let resp = HttpRequest::post(url)
            .bearer_auth(&self.config.api_key)
            .json(&req)?
            .send()
            .map_err(|e| anyhow!("request failed: {e}"))?;

        if !resp.is_success() {
            return Err(anyhow!(
                "API error (HTTP {}): {}",
                resp.status,
                resp.body
            ));
        }

        // Validate the response parses before returning.
        let _: CompletionResponse = serde_json::from_str(&resp.body)
            .map_err(|e| anyhow!("unexpected response: {e}\nbody: {}", resp.body))?;

        Ok(resp.body)
    }
}

#[cfg(target_arch = "wasm32")]
ein_plugin::export_model_client!(MyModelClient);
```

### Corresponding config

```json
{
  "model_client_name": "my_model_client",
  "plugin_configs": {
    "my_model_client": {
      "params": {
        "api_key": "your-key-or-dummy",
        "base_url": "http://localhost:8080/v1",
        "model": "my-custom-model"
      }
    }
  }
}
```

The filename stem of your `.wasm` file must match the key in `plugin_configs`. If you name the file `my_model_client.wasm`, use `"my_model_client"` as the key.

## Adapting to non-OpenAI APIs

If the upstream API uses a different request/response format, deserialize `CompletionRequest` and translate it into your API's format before sending, then translate the response back into `CompletionResponse` before returning.

`CompletionRequest.messages` uses `Role::User`, `Role::Assistant`, `Role::System`, and `Role::Tool` (for tool results). `CompletionRequest.tools` carries `ToolDef` entries that you translate into your API's tool/function spec.

The Anthropic and Ollama plugins in the Ein source tree are good references for non-OpenAI translation — see `plugins/ein_anthropic/` and `plugins/ein_ollama/`.
