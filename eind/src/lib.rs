// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

//! Ein server library.
//!
//! Exposes [`run`] so both the standalone `eind` binary and the `ein`
//! meta-package binary can share the same entry-point without duplicating code.

mod grpc;
mod model_client;
mod persistence;
mod plugins;
mod tools;

pub use plugins::install_plugins;

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

/// Start the Ein gRPC server and block until it exits.
pub async fn run(port: u16) -> anyhow::Result<()> {
    // In release builds, auto-install plugins if none are present.
    if !cfg!(debug_assertions) {
        let config = EinConfig::default();

        if plugins::plugins_missing(&config.plugin_dir).await {
            println!("No plugins found, downloading from GitHub release...");
            plugins::install_plugins(None).await?;
        }
    }

    let addr = format!("0.0.0.0:{port}").parse()?;

    let server = AgentServer::new().await?;

    println!("eind listening on {addr}");

    Server::builder()
        .add_service(AgentServiceServer::new(server))
        .serve(addr)
        .await?;

    Ok(())
}
