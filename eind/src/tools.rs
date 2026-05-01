// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

mod bindings;
mod syscalls;

use ein_agent::{
    AgentEventHandler, async_trait,
    tools::{ToolDef, ToolResult, ToolSet},
};
use tokio::fs;
use wasmtime::{Engine, Store, component::*};
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtx, WasiCtxView, WasiView};

use std::{
    collections,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use bindings::Plugin;

#[derive(Clone)]
pub struct ToolSetManager {
    engine: Engine,
    linker: Arc<Linker<ToolState>>,
    tool_dir: PathBuf,
}

impl ToolSetManager {
    pub async fn new<P: AsRef<Path>>(tool_dir: P, engine: Engine) -> anyhow::Result<Self> {
        let linker = Arc::new(build_tool_linker(&engine)?);

        Ok(Self {
            engine,
            linker,
            tool_dir: tool_dir.as_ref().to_owned(),
        })
    }

    pub async fn new_tool_set(
        &self,
        session_cfg: &ein_proto::ein::SessionConfig,
    ) -> anyhow::Result<WasmToolSet> {
        WasmToolSet::load(&self.engine, &self.linker, &self.tool_dir, session_cfg).await
    }
}

/// Builds the Wasmtime linker for tool plugins — called once at server startup.
///
/// Registers WASI p2 interface.
fn build_tool_linker(engine: &Engine) -> anyhow::Result<Linker<ToolState>> {
    let mut linker: Linker<ToolState> = Linker::new(engine);
    // Register standard WASI p2 host functions.
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    // Register Ein-specific host functions (syscalls exposed to plugins).
    Plugin::add_to_linker::<ToolState, HasSelf<ToolState>>(&mut linker, |state| state)?;

    Ok(linker)
}

/// Shared state threaded through each Wasmtime `Store` for tool plugins.
///
/// Every WASM plugin instance gets its own `Store<HarnessState>`, giving it
/// an isolated WASI context and resource table.
struct ToolState {
    pub resource_table: ResourceTable,
    pub wasi_ctx: WasiCtx,
    pub current_call_id: Option<String>,
    /// Set by the agent loop before each Bash tool call so the `spawn` syscall
    /// can stream stdout lines upstream as `ToolOutputChunk` events.
    pub event_handler: Option<AgentEventHandler>,
}

impl WasiView for ToolState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.resource_table,
        }
    }
}

pub struct WasmToolSet(collections::HashMap<String, WasmTool>);

impl WasmToolSet {
    fn new() -> Self {
        Self(collections::HashMap::new())
    }

    async fn load<P: AsRef<Path>>(
        engine: &Engine,
        linker: &Linker<ToolState>,
        plugin_dir: P,
        session_cfg: &ein_proto::ein::SessionConfig,
    ) -> anyhow::Result<Self> {
        let mut registry = Self::new();

        let mut entries = fs::read_dir(plugin_dir.as_ref()).await.map_err(|e| {
            let dir = plugin_dir.as_ref().display();
            anyhow::anyhow!(
                "Plugin directory not found: {dir}\n\n\
                 In debug builds, run `cargo build --target wasm32-wasip2` first.\n\
                 In release builds, run `./scripts/build_install_plugins.sh`.\n\
                 Details: {e}"
            )
        })?;

        loop {
            match entries.next_entry().await {
                Ok(Some(entry)) => {
                    if entry.path().extension().and_then(|e| e.to_str()) == Some("wasm") {
                        let stem = entry
                            .path()
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        let pc = session_cfg.plugin_configs.get(&stem);
                        let merged_paths = merge_dedup(
                            &session_cfg.allowed_paths,
                            pc.map(|p| p.allowed_paths.as_slice()).unwrap_or(&[]),
                        );
                        let merged_hosts = merge_dedup(
                            &session_cfg.allowed_hosts,
                            pc.map(|p| p.allowed_hosts.as_slice()).unwrap_or(&[]),
                        );

                        let tool = WasmTool::load(
                            engine,
                            linker,
                            entry.path(),
                            &merged_paths,
                            &merged_hosts,
                        )
                        .await
                        .map_err(|e| {
                            anyhow::anyhow!(
                                "Failed to load plugin '{}': {e}\n\n\
                                 In debug builds try rebuilding with 'cargo build -p {} --target wasm32-wasip2'
                                 In release build try rebuilding with `./scripts/build_install_plugins.sh`.",
                                entry.path().display(),
                                stem
                            )
                        })?;
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

    pub fn schemas(&self) -> Vec<ToolDef> {
        self.0
            .values()
            .map(|tool| tool.schema().to_owned())
            .collect()
    }
}

#[async_trait]
impl ToolSet for WasmToolSet {
    fn schemas(&self) -> Vec<ToolDef> {
        self.schemas()
    }

    async fn call_tool(&mut self, name: &str, id: &str, args: &str) -> anyhow::Result<ToolResult> {
        match self.0.get_mut(name) {
            Some(tool) => tool.call(id, args).await,
            None => Err(anyhow::anyhow!("tool not found: {name}")),
        }
    }

    fn display_arg_for(&self, tool_name: &str, args: &str) -> Option<String> {
        let primary_param = self.0.get(tool_name)?.primary_arg.as_deref()?;
        let val: serde_json::Value = serde_json::from_str(args).ok()?;
        val.get(primary_param)?.as_str().map(String::from)
    }

    fn set_event_handler(&mut self, handler: AgentEventHandler) {
        for (_name, tool) in self.0.iter_mut() {
            tool.set_event_handler(handler.clone());
        }
    }

    async fn cleanup(mut self) {
        for (name, tool) in self.0.drain() {
            if let Err(err) = tool.cleanup().await {
                eprintln!("Failed to cleanup tool {name}: {err}");
            }
        }
    }
}

/// Merges two slices into a deduplicated Vec, preserving insertion order with
/// `base` entries first. Used to union global and per-plugin allowed lists.
pub fn merge_dedup(base: &[String], extra: &[String]) -> Vec<String> {
    let mut seen = collections::HashSet::new();
    let mut result = Vec::with_capacity(base.len() + extra.len());
    for s in base.iter().chain(extra.iter()) {
        if seen.insert(s.clone()) {
            result.push(s.clone());
        }
    }
    result
}

pub struct WasmTool {
    name: String,
    schema: ToolDef,
    pub primary_arg: Option<String>,
    store: Store<ToolState>,
    bindings: Plugin,
    handle: ResourceAny,
}

impl WasmTool {
    async fn load<P: AsRef<Path>>(
        engine: &Engine,
        linker: &Linker<ToolState>,
        path: P,
        allowed_paths: &[String],
        allowed_hosts: &[String],
    ) -> anyhow::Result<Self> {
        let mut builder = WasiCtx::builder();
        let mut wasi_builder = builder.inherit_stdio().inherit_args();

        // Mount each allowed path at its absolute guest path so plugins can
        // open files by absolute path.  Additionally mount the first path as
        // "." so that relative paths (e.g. "crates/eind/Cargo.toml")
        // resolve correctly — WASI resolves relative paths against the guest
        // current directory, which must be explicitly preopened.
        let mut first = true;
        for host_path in allowed_paths {
            wasi_builder = wasi_builder
                .preopened_dir(host_path, host_path, DirPerms::all(), FilePerms::all())
                .expect("failed to preopen dir");
            if first {
                wasi_builder = wasi_builder
                    .preopened_dir(host_path, ".", DirPerms::all(), FilePerms::all())
                    .expect("failed to preopen dir as current directory");
                first = false;
            }
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
            engine,
            ToolState {
                wasi_ctx: wasi,
                resource_table: ResourceTable::new(),
                current_call_id: None,
                event_handler: None,
            },
        );

        let component = Component::from_file(engine, path)?;
        let bindings = Plugin::instantiate_async(&mut store, &component, linker).await?;

        let accessor = bindings.tool().tool();
        let handle = accessor.call_constructor(&mut store).await?;
        let name = accessor.call_name(&mut store, handle).await?;
        let schema = accessor.call_schema(&mut store, handle).await?;
        let primary_arg = accessor.call_primary_arg(&mut store, handle).await?;

        Ok(Self {
            name,
            schema: serde_json::from_str(&schema)?,
            primary_arg,
            store,
            bindings,
            handle,
        })
    }

    fn set_event_handler(&mut self, handler: AgentEventHandler) {
        self.store.data_mut().event_handler = Some(handler);
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn schema(&self) -> &ToolDef {
        &self.schema
    }

    pub async fn call(&mut self, id: &str, args: &str) -> anyhow::Result<ToolResult> {
        self.store.data_mut().current_call_id = Some(id.to_owned());

        let res = self
            .bindings
            .tool()
            .tool()
            .call_call(&mut self.store, self.handle, id, args)
            .await?
            .map_err(|err| anyhow::anyhow!(err))?;

        self.store.data_mut().current_call_id = None;

        Ok(serde_json::from_str(&res)?)
    }

    pub async fn cleanup(mut self) -> anyhow::Result<()> {
        self.handle.resource_drop_async(&mut self.store).await?;
        Ok(())
    }
}
