//! Ein server binary.
//!
//! Starts a gRPC server that exposes the `Agent` service defined in
//! `ein-proto`. Clients (e.g. `ein-tui`) open a bidirectional streaming
//! session, stream user prompts in, and receive a sequence of `AgentEvent`
//! messages back as the agent thinks, invokes tools, and produces output.
//!
//! # Configuration
//!
//! | Variable              | Description                            | Default                        |
//! |-----------------------|----------------------------------------|--------------------------------|
//! | `OPENROUTER_API_KEY`  | API key for OpenRouter (required)      | —                              |
//! | `OPENROUTER_BASE_URL` | Override the OpenRouter endpoint       | `https://openrouter.ai/api/v1` |
//! | `--port`              | TCP port the gRPC server listens on    | `50051`                        |
//!
//! # Plugin loading
//!
//! In debug builds, WASM plugins are loaded from `./target/wasm32-wasip2/debug/`.
//! In release builds they are loaded from `~/.ein/plugins/`.
//! Run `./scripts/build_install_plugins.sh` to compile and install them.

mod agent;
mod bindings;
mod grpc;
mod syscalls;
mod tools;

use clap::Parser;
use ein_proto::ein::{AgentEvent, agent_server::AgentServer as AgentServiceServer};
use grpc::AgentServer;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tonic::{Status, transport::Server};
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

/// Shared state threaded through each Wasmtime `Store`.
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

/// Top-level runtime configuration for the Ein server.
#[derive(Debug, Clone)]
pub struct EinConfig {
    #[expect(unused)]
    ein_dir: PathBuf,
    /// Directory from which `.wasm` plugin files are loaded.
    pub plugin_dir: PathBuf,
}

impl Default for EinConfig {
    fn default() -> Self {
        let ein_dir = dirs::home_dir()
            .expect("Failed to load EinConfig, Missing home directory")
            .join(".ein");

        // Use the local debug output directory during development so plugins
        // don't need to be installed after every rebuild.
        let plugin_dir = if cfg!(debug_assertions) {
            PathBuf::from("./target/wasm32-wasip2/debug")
        } else {
            ein_dir.join("plugins")
        };

        Self {
            ein_dir,
            plugin_dir,
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

    let server = AgentServer::new()?;

    println!("ein-server listening on {addr}");

    Server::builder()
        .add_service(AgentServiceServer::new(server))
        .serve(addr)
        .await?;

    Ok(())
}
