// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

pub mod syscalls {
    pub use crate::model_client::__wit::ein::host::host::log;
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

pub trait ConstructableModelClientPlugin: ModelClientPlugin {
    fn new(config_json: &str) -> Self;
}

pub trait ModelClientPlugin: Send + Sync {
    fn complete(&self, request_json: &str) -> anyhow::Result<String>;
}
