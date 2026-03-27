pub mod syscalls {
    pub use crate::model_client::ein::model_client::host::{http_request, log};
}

#[doc(hidden)]
pub mod model_client {
    use super::ConstructableModelClientPlugin;
    use wit_bindgen::generate;

    generate!({
        world: "model-client",
        path: "../../wit/model_client",
        export_macro_name: "__export_model_client_inner",
        pub_export_macro: true,
        default_bindings_module: "ein_model_client::model_client"
    });

    impl<T> exports::model_client::Guest for T
    where
        T: exports::model_client::GuestModelClient,
    {
        type ModelClient = Self;
    }

    impl<T> exports::model_client::GuestModelClient for T
    where
        T: ConstructableModelClientPlugin + 'static,
    {
        fn new(config_json: String) -> Self {
            T::new(&config_json)
        }

        fn complete(&self, request_json: String) -> Result<String, String> {
            self.complete(&request_json).map_err(|e| e.to_string())
        }
    }
}

#[macro_export]
macro_rules! __export_model_client {
    ($($t:tt)*) => {
        $crate::model_client::__export_model_client_inner!($($t)*);
    };
}

pub use __export_model_client as export;

pub trait ConstructableModelClientPlugin: ModelClientPlugin {
    fn new(config_json: &str) -> Self;
}

pub trait ModelClientPlugin: Send + Sync {
    fn complete(&self, request_json: &str) -> anyhow::Result<String>;
}

// ---------------------------------------------------------------------------
// Shared request / response types
//
// These mirror the OpenAI chat completion wire format used by OpenRouter.
// Serde attributes must preserve the exact field names and shapes the API
// expects so that serialised messages can be sent back to the LLM unchanged.
// ---------------------------------------------------------------------------

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    /// Full conversation history in OpenAI message format. Kept as raw
    /// `Value`s so the server's `Vec<Value>` history can be passed through
    /// without an extra conversion layer.
    pub messages: Vec<serde_json::Value>,
    pub tools: Vec<serde_json::Value>,
    pub max_tokens: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    #[serde(default)]
    pub choices: Vec<Choice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Present when the upstream API returns an error object (e.g. 402
    /// insufficient credits). The server emits an `AgentError` event rather
    /// than crashing when this field is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: usize,
    pub finish_reason: FinishReason,
    pub message: Message,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "lowercase")]
pub enum ToolCall {
    Function {
        id: String,
        index: usize,
        function: FunctionCall,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// Raw JSON string containing the arguments chosen by the model.
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    #[allow(dead_code)]
    pub total_tokens: i32,
}

// ---------------------------------------------------------------------------
// HTTP types and helpers for the http_request syscall
// ---------------------------------------------------------------------------

/// A pending HTTP request. Build one with [`HttpRequest::post`] (or the other
/// method constructors) and send it with [`HttpRequest::send`].
///
/// # Example
/// ```rust,ignore
/// let resp = HttpRequest::post("https://api.example.com/v1/chat/completions")
///     .bearer_auth(&api_key)
///     .json(&body)
///     .send()?;
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    pub headers: std::collections::HashMap<String, String>,
    pub body: String,
}

impl HttpRequest {
    fn new(method: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers: std::collections::HashMap::new(),
            body: String::new(),
        }
    }

    pub fn get(url: impl Into<String>) -> Self {
        Self::new("GET", url)
    }

    pub fn post(url: impl Into<String>) -> Self {
        Self::new("POST", url)
    }

    pub fn put(url: impl Into<String>) -> Self {
        Self::new("PUT", url)
    }

    pub fn delete(url: impl Into<String>) -> Self {
        Self::new("DELETE", url)
    }

    /// Add an arbitrary header.
    pub fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    /// Add a `Content-Type: application/json` header.
    pub fn content_type_json(self) -> Self {
        self.header("Content-Type", "application/json")
    }

    /// Add an `Authorization: Bearer <token>` header.
    pub fn bearer_auth(self, token: impl Into<String>) -> Self {
        self.header("Authorization", format!("Bearer {}", token.into()))
    }

    /// Serialize `value` as JSON, set `Content-Type: application/json`, and
    /// use the result as the request body.
    pub fn json<T: Serialize>(mut self, value: &T) -> anyhow::Result<Self> {
        self.body = serde_json::to_string(value)?;
        Ok(self.content_type_json())
    }

    /// Set a raw string body without changing headers.
    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = body.into();
        self
    }

    /// Dispatch the request via the host `http_request` syscall and return the
    /// parsed [`HttpResponse`].
    ///
    /// # Errors
    /// Returns an error if the syscall fails or if the response cannot be
    /// deserialised. Does **not** treat non-2xx status codes as errors — check
    /// [`HttpResponse::status`] yourself.
    pub fn send(self) -> anyhow::Result<HttpResponse> {
        let req_json = serde_json::to_string(&self)?;
        let resp_json = syscalls::http_request(&req_json).map_err(|e| anyhow::anyhow!(e))?;
        Ok(serde_json::from_str(&resp_json)?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    /// Returns `true` for 2xx status codes.
    pub fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    /// Deserialise the response body as JSON.
    pub fn json<T: for<'de> Deserialize<'de>>(&self) -> anyhow::Result<T> {
        Ok(serde_json::from_str(&self.body)?)
    }
}
