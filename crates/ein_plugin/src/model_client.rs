// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

pub mod syscalls {
    pub use crate::model_client::__wit::ein::host::host::log;
    /// Emit a chunk of streamed assistant text to the host mid-`complete` (see
    /// the `streaming` interface in `wit/model_client`). A plugin that streams
    /// calls this per token/chunk; one that doesn't never calls it.
    pub use crate::model_client::__wit::ein::model_client::streaming::on_content_delta;
}

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

pub use ein_core::types::{
    Choice, CompletionRequest, CompletionResponse, FinishReason, FunctionCall, Message, Role,
    ToolCall, ToolDef, ToolFunctionParams, Usage,
};
pub use ein_http::{HttpRequest, HttpResponse, RequestDeniedError};

/// Extension of [`ModelClientPlugin`] that can be constructed from config JSON.
///
/// The WIT glue calls `new` with the plugin's `config_json` string (the
/// `params_json` field from `SessionConfig.plugin_configs`) when a new session
/// starts. Implement this trait alongside [`ModelClientPlugin`] for every model
/// client plugin.
pub trait ConstructableModelClientPlugin: ModelClientPlugin {
    fn new(config_json: &str) -> Self;
}

/// Core trait implemented by every model client WASM plugin.
///
/// The server calls `complete` with a serialised [`CompletionRequest`] and
/// expects a serialised [`CompletionResponse`] in return. The plugin is
/// responsible for translating between Ein's internal format and whatever
/// wire protocol the upstream API uses (e.g. Anthropic Messages, OpenAI).
pub trait ModelClientPlugin: Send + Sync {
    fn complete(&self, request_json: &str) -> anyhow::Result<String>;
}
