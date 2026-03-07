mod bash;

pub use bash::BashTool;

use async_trait::async_trait;
use ein_plugin::{ToolDef, ToolResult};
use std::path::Path;
use wasmtime::{Engine, Store, component::*};
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtx};

// plugin.wit
bindgen!({
    world: "plugin",
    path: "wit/plugin",
    imports: { default: async },
    exports: { default: async }
});

#[async_trait]
pub(crate) trait Tool {
    fn name(&self) -> &str;
    fn schema(&self) -> &ToolDef;
    async fn call(&mut self, id: &str, args: &str) -> anyhow::Result<ToolResult>;
}

pub struct WasmTool {
    // Static values that don't change during tool execution
    name: String,
    schema: ToolDef, // Would be better if this was strongly typed
    // Mutable state for `call`
    store: Store<super::MyState>,
    bindings: Plugin,
    handle: ResourceAny,
}

impl WasmTool {
    pub async fn load<P: AsRef<Path>>(
        engine: &Engine,
        linker: &Linker<super::MyState>,
        path: P,
    ) -> anyhow::Result<Self> {
        let wasi = WasiCtx::builder()
            .inherit_stdio()
            .inherit_args()
            .preopened_dir(".", ".", DirPerms::all(), FilePerms::all())
            .expect("failed to preopen dir")
            .build();

        let mut store = Store::new(
            &engine,
            super::MyState {
                wasi_ctx: wasi,
                resource_table: ResourceTable::new(),
            },
        );

        let component = Component::from_file(engine, path)?;
        let bindings = Plugin::instantiate_async(&mut store, &component, linker).await?;

        let accessor = bindings.tool().tool();
        let handle = accessor.call_constructor(&mut store).await?;
        let name = accessor.call_name(&mut store, handle).await?;
        let schema = accessor.call_schema(&mut store, handle).await?;

        Ok(Self {
            name,
            schema: serde_json::from_str(&schema)?,
            store,
            bindings,
            handle,
        })
    }

    pub async fn cleanup(&mut self) -> anyhow::Result<()> {
        self.handle.resource_drop_async(&mut self.store).await?;

        Ok(())
    }
}

#[async_trait]
impl Tool for WasmTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> &ToolDef {
        &self.schema
    }

    async fn call(&mut self, id: &str, args: &str) -> anyhow::Result<ToolResult> {
        let res = self
            .bindings
            .tool()
            .tool()
            .call_call(&mut self.store, self.handle, id, args)
            .await?
            .map_err(|err| anyhow::anyhow!(err))?;

        Ok(serde_json::from_str(&res)?)
    }
}
