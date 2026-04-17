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
    }
}

#[macro_export]
macro_rules! export_tool {
    ($($t:tt)*) => {
        $crate::tool::__wit::__export_plugin_inner!($($t)*);
    };
}

pub use ein_core::types::{ToolDef, ToolResult};

pub trait ConstructableToolPlugin: ToolPlugin {
    fn new() -> Self;
}

// WASM plugin for Ein implementation detail
pub trait ToolPlugin: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> ToolDef;
    fn call(&self, id: &str, args: &str) -> anyhow::Result<ToolResult>;

    fn enable_chunk_sender(&self) -> bool {
        false
    }
}
