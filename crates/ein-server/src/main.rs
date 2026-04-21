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

mod grpc;
mod model_client;
mod persistence;
mod tools;

use clap::Parser;
use ein_proto::ein::agent_server::AgentServer as AgentServiceServer;
use grpc::AgentServer;
use std::path::PathBuf;
use tonic::transport::Server;

/// Top-level runtime configuration for the Ein server.
#[derive(Debug, Clone)]
pub struct EinConfig {
    /// Directory from which tool `.wasm` plugin files are loaded.
    pub plugin_dir: PathBuf,
    /// Directory from which model client `.wasm` plugin files are loaded.
    pub model_client_dir: PathBuf,
    /// Path to the SQLite session database.
    pub db_path: PathBuf,
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
            plugin_dir,
            model_client_dir,
            db_path: ein_dir.join("sessions.db"),
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
