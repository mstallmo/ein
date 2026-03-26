use crate::bindings::Plugin;
use ein_tool::{ToolDef, ToolResult};
use serde_json::Value;
use std::{collections, path::Path};
use tokio::fs;
use wasmtime::{Engine, Store, component::*};
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtx};

pub struct WasmTool {
    // Static values that don't change during tool execution
    name: String,
    schema: ToolDef, // Would be better if this was strongly typed
    // Mutable state for `call`
    store: Store<crate::HarnessState>,
    bindings: Plugin,
    handle: ResourceAny,
}

impl WasmTool {
    pub async fn load<P: AsRef<Path>>(
        engine: &Engine,
        linker: &Linker<crate::HarnessState>,
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
            crate::HarnessState {
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

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn schema(&self) -> &ToolDef {
        &self.schema
    }

    pub async fn call(&mut self, id: &str, args: &str) -> anyhow::Result<ToolResult> {
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

pub struct ToolRegistry(collections::HashMap<String, WasmTool>);

impl ToolRegistry {
    fn new() -> Self {
        Self(collections::HashMap::new())
    }

    pub async fn load<P: AsRef<Path>>(
        engine: &Engine,
        linker: &Linker<crate::HarnessState>,
        plugin_dir: P,
    ) -> anyhow::Result<Self> {
        let mut registry = Self::new();

        let mut entries = fs::read_dir(plugin_dir.as_ref()).await?;

        loop {
            match entries.next_entry().await {
                Ok(Some(entry)) => {
                    if entry.path().extension().and_then(|e| e.to_str()) == Some("wasm") {
                        let tool = WasmTool::load(engine, linker, entry.path()).await?;
                        registry.add_tool(tool);
                    }
                }
                Ok(None) => break,
                Err(err) => {
                    eprintln!(
                        "failed to get entry from directory {}: {err}",
                        plugin_dir.as_ref().display()
                    );
                }
            }
        }

        Ok(registry)
    }

    fn add_tool(&mut self, tool: WasmTool) {
        println!("Adding tool: {}", tool.name());
        self.0.insert(tool.name().to_string(), tool);
    }

    pub fn schemas(&self) -> Result<Vec<Value>, serde_json::Error> {
        self.0
            .values()
            .map(|tool| serde_json::to_value(tool.schema()))
            .collect::<Result<Vec<_>, serde_json::Error>>()
    }

    pub fn get(&mut self, name: &str) -> Option<&mut WasmTool> {
        self.0.get_mut(name)
    }

    pub async fn unload(mut self) {
        for (name, mut tool) in self.0.drain() {
            if let Err(err) = tool.cleanup().await {
                eprintln!("Failed to cleanup tool {name}: {err}");
            }
        }
    }
}
