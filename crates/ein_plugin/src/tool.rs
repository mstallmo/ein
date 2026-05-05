// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

pub mod syscalls {
    pub use crate::tool::__wit::ein::host::host::log;
    pub use crate::tool::__wit::ein::plugin::process::spawn;
}

#[doc(hidden)]
pub mod __wit {
    use super::ConstructableToolPlugin;
    use wit_bindgen::generate;

    generate!({
        world: "plugin",
        path: "../../wit/plugin",
        export_macro_name: "__export_plugin_inner",
        pub_export_macro: true,
        default_bindings_module: "ein_plugin::tool::__wit",
        with: { "ein:host/host": generate }
    });

    impl<T> exports::tool::Guest for T
    where
        T: exports::tool::GuestTool,
    {
        type Tool = Self;
    }

    impl<T> exports::tool::GuestTool for T
    where
        T: ConstructableToolPlugin + 'static,
    {
        fn new() -> Self {
            T::new()
        }

        fn name(&self) -> String {
            self.name().to_string()
        }

        fn schema(&self) -> String {
            match serde_json::to_string(&self.schema()) {
                Ok(val) => val,
                Err(err) => {
                    eprintln!("Failed to serialize schema: {err}");
                    "".to_string()
                }
            }
        }

        fn enable_chunk_sender(&self) -> bool {
            self.enable_chunk_sender()
        }

        fn call(&self, id: String, args: String) -> Result<String, String> {
            let res = self.call(&id, &args).map_err(|err| err.to_string())?;
            serde_json::to_string(&res).map_err(|err| err.to_string())
        }

        fn primary_arg(&self) -> Option<String> {
            self.primary_arg().map(str::to_owned)
        }
    }
}

#[macro_export]
macro_rules! export_tool {
    ($($t:tt)*) => {
        $crate::tool::__wit::__export_plugin_inner!($($t)*);
    };
}

pub use ein_core::types::{ToolDef, ToolResult};

/// Extension of [`ToolPlugin`] that can be zero-argument constructed.
///
/// The WIT glue calls `new` when a session loads the plugin. Tool plugins do
/// not receive config JSON; per-session configuration flows through the WASI
/// context (preopened paths, allowed hosts) rather than constructor arguments.
pub trait ConstructableToolPlugin: ToolPlugin {
    fn new() -> Self;
}

/// Core trait implemented by every tool WASM plugin.
///
/// The server calls [`name`](Self::name) and [`schema`](Self::schema) once
/// after loading to register the tool with the LLM. For each model tool call,
/// it invokes [`call`](Self::call) with the call ID and JSON-encoded arguments.
pub trait ToolPlugin: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> ToolDef;
    fn call(&self, id: &str, args: &str) -> anyhow::Result<ToolResult>;

    /// Whether this plugin wants a chunk sender for streaming output.
    ///
    /// When `true`, the server wires up a `ToolOutputChunk` event channel
    /// before calling [`call`](Self::call), allowing the plugin to stream
    /// incremental output (e.g. Bash stdout lines) back to the client in real
    /// time rather than waiting for the call to complete.
    fn enable_chunk_sender(&self) -> bool {
        false
    }

    /// The name of the parameter to extract and display next to the tool name
    /// in client UIs. Return `None` (the default) to show only the tool name.
    fn primary_arg(&self) -> Option<&str> {
        None
    }
}
