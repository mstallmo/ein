mod agent;
mod bindings;
mod grpc;
mod syscalls;
mod tools;

use clap::Parser;
use ein_proto::ein::agent_server::AgentServer as AgentServiceServer;
use grpc::AgentServer;
use std::path::PathBuf;
use tonic::transport::Server;
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

pub struct HarnessState {
    pub resource_table: ResourceTable,
    pub wasi_ctx: WasiCtx,
}

impl WasiView for HarnessState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.resource_table,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EinConfig {
    #[expect(unused)]
    ein_dir: PathBuf,
    pub plugin_dir: PathBuf,
}

impl Default for EinConfig {
    fn default() -> Self {
        let ein_dir = dirs::home_dir()
            .expect("Failed to load EinConfig, Missing home directory")
            .join(".ein");

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
    #[arg(long, default_value = "50051")]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let addr = format!("0.0.0.0:{}", args.port).parse()?;

    let server = AgentServer::new()?;

    Server::builder()
        .add_service(AgentServiceServer::new(server))
        .serve(addr)
        .await?;

    Ok(())
}
