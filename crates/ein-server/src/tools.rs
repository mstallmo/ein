use crate::{HarnessState, bindings::Plugin};
use ein_proto::ein::AgentEvent;
use ein_tool::{ToolDef, ToolResult};
use serde_json::Value;
use std::{collections, net::IpAddr, path::Path, sync::Arc};
use tokio::{fs, sync::mpsc};
use tonic::Status;
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
        allowed_paths: &[String],
        allowed_hosts: &[String],
    ) -> anyhow::Result<Self> {
        let mut builder = WasiCtx::builder();
        let mut wasi_builder = builder.inherit_stdio().inherit_args();

        for host_path in allowed_paths {
            wasi_builder = wasi_builder
                .preopened_dir(host_path, host_path, DirPerms::all(), FilePerms::all())
                .expect("failed to preopen dir");
        }

        if allowed_hosts.iter().any(|h| h == "*") {
            // Wildcard: allow all network connections.
            wasi_builder.inherit_network();
        } else if allowed_hosts.is_empty() {
            // Default: deny all network connections.
            wasi_builder.socket_addr_check(|_, _| Box::pin(async move { false }));
        } else {
            // Resolve hostnames to IPs upfront so the check closure is cheap.
            let mut allowed_ips = collections::HashSet::<IpAddr>::new();
            for host in allowed_hosts {
                // lookup_host requires a host:port pair; use port 0 as placeholder.
                if let Ok(addrs) = tokio::net::lookup_host(format!("{host}:0")).await {
                    for addr in addrs {
                        allowed_ips.insert(addr.ip());
                    }
                }
            }
            let allowed_ips = Arc::new(allowed_ips);
            wasi_builder.socket_addr_check(move |addr, _use| {
                let allowed = allowed_ips.clone();
                Box::pin(async move { allowed.contains(&addr.ip()) })
            });
        }

        let wasi = wasi_builder.build();

        let mut store = Store::new(
            &engine,
            crate::HarnessState {
                wasi_ctx: wasi,
                resource_table: ResourceTable::new(),
                chunk_tx: None,
                tool_call_id: String::new(),
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

    pub async fn cleanup(mut self) -> anyhow::Result<()> {
        self.handle.resource_drop_async(&mut self.store).await?;

        Ok(())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn schema(&self) -> &ToolDef {
        &self.schema
    }

    pub async fn enable_chunk_sender(&mut self) -> anyhow::Result<bool> {
        let res = self
            .bindings
            .tool()
            .tool()
            .call_enable_chunk_sender(&mut self.store, self.handle)
            .await?;

        Ok(res)
    }

    /// Injects the gRPC event sender and tool call ID into the store so the
    /// `spawn` host syscall can stream stdout lines as `ToolOutputChunk` events.
    pub fn set_chunk_sender(
        &mut self,
        tx: mpsc::Sender<Result<AgentEvent, Status>>,
        tool_call_id: String,
    ) {
        let state = self.store.data_mut();
        state.chunk_tx = Some(tx);
        state.tool_call_id = tool_call_id;
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
        allowed_paths: &[String],
        allowed_hosts: &[String],
    ) -> anyhow::Result<Self> {
        let mut registry = Self::new();

        let mut entries = fs::read_dir(plugin_dir.as_ref()).await?;

        loop {
            match entries.next_entry().await {
                Ok(Some(entry)) => {
                    if entry.path().extension().and_then(|e| e.to_str()) == Some("wasm") {
                        let tool = WasmTool::load(
                            engine,
                            linker,
                            entry.path(),
                            allowed_paths,
                            allowed_hosts,
                        )
                        .await?;
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
        for (name, tool) in self.0.drain() {
            if let Err(err) = tool.cleanup().await {
                eprintln!("Failed to cleanup tool {name}: {err}");
            }
        }
    }
}

/// Builds the Wasmtime linker for tool plugins — called once at server startup.
///
/// Registers WASI p2 interface.
pub fn build_tool_linker(engine: &Engine) -> anyhow::Result<Linker<HarnessState>> {
    let mut linker: Linker<HarnessState> = Linker::new(&engine);
    // Register standard WASI p2 host functions.
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    // Register Ein-specific host functions (syscalls exposed to plugins).
    Plugin::add_to_linker::<HarnessState, HasSelf<HarnessState>>(&mut linker, |state| state)?;

    Ok(linker)
}
