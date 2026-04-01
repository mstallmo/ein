// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

pub mod syscalls {
    pub use crate::model_client::__wit::ein::host::host::log;
}

pub use ein_http::{HttpRequest, HttpResponse};

#[doc(hidden)]
pub mod __wit {
    use super::ConstructableModelClientPlugin;
    use wit_bindgen::generate;

    generate!({
        world: "model-client",
        path: "../../wit/model_client",
        export_macro_name: "__export_model_client_inner",
        pub_export_macro: true,
        default_bindings_module: "ein_plugin::model_client::__wit",
        with: { "ein:host/host": generate }
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
macro_rules! export_model_client {
    ($($t:tt)*) => {
        $crate::model_client::__wit::__export_model_client_inner!($($t)*);
    };
}

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
    #[serde(other)]
    Unsupported,
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
