// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

//! Ein server binary.
//!
//! Starts a gRPC server that exposes the `Agent` service defined in
//! `ein-proto`. Clients (e.g. `ein-tui`) open a bidirectional streaming
//! session, stream user prompts in, and receive a sequence of `AgentEvent`
//! messages back as the agent thinks, invokes tools, and produces output.
//!
//! # Configuration
//!
//! | Variable | Description                         | Default |
//! |----------|-------------------------------------|---------|
//! | `--port` | TCP port the gRPC server listens on | `50051` |
//!
//! API credentials (`api_key`, `base_url`) are supplied by the client via
//! `SessionConfig` at connection time, read from `~/.ein/config.json`.
//!
//! # Plugin loading
//!
//! In debug builds, WASM plugins are loaded from `./target/wasm32-wasip2/debug/`.
//! In release builds tool plugins are loaded from `~/.ein/plugins/tools/` and
//! model client plugins from `~/.ein/plugins/model_clients/`.
//! Run `./scripts/build_install_plugins.sh` to compile and install them.

mod agent;
mod bindings;
mod grpc;
mod model_client;
mod model_client_bindings;
mod persistence;
mod syscalls;
mod tools;

use clap::Parser;
use ein_proto::ein::{AgentEvent, agent_server::AgentServer as AgentServiceServer};
use grpc::AgentServer;
use std::collections::HashSet;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tonic::{Status, transport::Server};
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};
use wasmtime_wasi_http::{
    HttpResult, WasiHttpCtx,
    bindings::http::types::ErrorCode,
    body::HyperOutgoingBody,
    types::{HostFutureIncomingResponse, OutgoingRequestConfig, default_send_request},
};

/// Shared state threaded through each Wasmtime `Store` for tool plugins.
///
/// Every WASM plugin instance gets its own `Store<HarnessState>`, giving it
/// an isolated WASI context and resource table.
pub struct HarnessState {
    pub resource_table: ResourceTable,
    pub wasi_ctx: WasiCtx,
    /// Set by the agent loop before each Bash tool call so the `spawn` syscall
    /// can stream stdout lines upstream as `ToolOutputChunk` events.
    pub chunk_tx: Option<mpsc::Sender<Result<AgentEvent, Status>>>,
    /// The tool call ID associated with the current `spawn` invocation.
    pub tool_call_id: String,
}

impl WasiView for HarnessState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.resource_table,
        }
    }
}

/// Shared state threaded through each Wasmtime `Store` for model client plugins.
///
/// Simpler than `HarnessState` — no chunk streaming. Includes `WasiHttpCtx`
/// so that the plugin's `wasi:http/outgoing-handler` import (used by `ein_http`
/// via `wstd`) is satisfied by the host linker.
///
/// `allowed_hosts` is a set of hostnames the plugin is permitted to connect to.
/// Requests to any other host are rejected with `ErrorCode::HttpRequestDenied`.
pub struct ModelClientHarnessState {
    pub resource_table: ResourceTable,
    pub wasi_ctx: WasiCtx,
    pub http_ctx: WasiHttpCtx,
    pub allowed_hosts: HashSet<String>,
}

impl WasiView for ModelClientHarnessState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.resource_table,
        }
    }
}

impl wasmtime_wasi_http::WasiHttpView for ModelClientHarnessState {
    fn ctx(&mut self) -> &mut WasiHttpCtx {
        &mut self.http_ctx
    }

    fn table(&mut self) -> &mut wasmtime::component::ResourceTable {
        &mut self.resource_table
    }

    fn send_request(
        &mut self,
        request: hyper::Request<HyperOutgoingBody>,
        config: OutgoingRequestConfig,
    ) -> HttpResult<HostFutureIncomingResponse> {
        // The WASI HTTP request model stores authority separately from the
        // path, so wasmtime_wasi_http may not embed the host in the hyper
        // URI. Fall back to the Host header if the URI has no host component.
        let host = request
            .uri()
            .host()
            .map(|h| h.to_string())
            .or_else(|| {
                request
                    .headers()
                    .get("host")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|h| h.split(':').next())
                    .map(|h| h.to_string())
            })
            .unwrap_or_default();
        let allowed = self.allowed_hosts.contains("*") || self.allowed_hosts.contains(&host);
        if !allowed {
            eprintln!(
                "[model client] blocked request to '{host}' — not in allowlist {:?}. \
                 Set 'base_url' in ~/.ein/config.json to allow this host.",
                self.allowed_hosts
            );
            return Err(ErrorCode::HttpRequestDenied.into());
        }
        Ok(default_send_request(request, config))
    }
}

/// Top-level runtime configuration for the Ein server.
#[derive(Debug, Clone)]
pub struct EinConfig {
    #[expect(unused)]
    ein_dir: PathBuf,
    /// Directory from which tool `.wasm` plugin files are loaded.
    pub plugin_dir: PathBuf,
    /// Directory from which model client `.wasm` plugin files are loaded.
    pub model_client_dir: PathBuf,
}

impl Default for EinConfig {
    fn default() -> Self {
        let ein_dir = dirs::home_dir()
            .expect("Failed to load EinConfig, Missing home directory")
            .join(".ein");

        // Use the local debug output directory during development so plugins
        // don't need to be installed after every rebuild.
        let (plugin_dir, model_client_dir) = if cfg!(debug_assertions) {
            let debug = PathBuf::from("./target/wasm32-wasip2/debug");
            (debug.clone(), debug)
        } else {
            (
                ein_dir.join("plugins").join("tools"),
                ein_dir.join("plugins").join("model_clients"),
            )
        };

        Self {
            ein_dir,
            plugin_dir,
            model_client_dir,
        }
    }
}

#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    /// TCP port for the gRPC server to listen on.
    #[arg(long, default_value = "50051")]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let addr = format!("0.0.0.0:{}", args.port).parse()?;

    let server = AgentServer::new().await?;

    println!("ein-server listening on {addr}");

    Server::builder()
        .add_service(AgentServiceServer::new(server))
        .serve(addr)
        .await?;

    Ok(())
}
