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

// Session-storage abstraction. Embedders (e.g. Edward) implement
// [`SessionStore`] against their own database and inject it via
// [`run_with_store`] or [`AgentServer::with_session_store`].
pub use grpc::AgentServer;
pub use persistence::{SessionStore, SessionSummaryData, SqliteSessionStore};

// Re-exported so downstream `SessionStore` implementers can name the message
// types in the trait signatures without depending on `ein_plugin` directly.
pub use ein_plugin::model_client::{Message, Role};

use ein_proto::ein::agent_server::AgentServer as AgentServiceServer;
use std::path::PathBuf;
use std::sync::Arc;
use tonic::transport::Server;

/// Start the Ein gRPC server with the default SQLite session store and block
/// until it exits.
///
/// A thin wrapper over [`run_with_store`]: it opens the bundled SQLite store at
/// `~/.ein/sessions.db` and hands it to the same serving path every embedder
/// goes through.
pub async fn run(port: u16) -> anyhow::Result<()> {
    let store = open_default_session_store().await?;
    run_with_store(port, store).await
}

/// Start the Ein gRPC server with a caller-supplied [`SessionStore`] and block
/// until it exits.
///
/// This is the single serving path — [`run`] delegates here with the default
/// store. It is also the entry point for systems built on top of `eind` that
/// persist sessions in their own database. Plugin and data directories still
/// come from [`EinConfig::default`].
pub async fn run_with_store(port: u16, store: Arc<dyn SessionStore>) -> anyhow::Result<()> {
    ensure_plugins_installed().await?;
    let server = AgentServer::with_session_store(EinConfig::default(), store).await?;
    serve(port, server).await
}

/// Open the bundled SQLite-backed [`SessionStore`] at `EinConfig::default`'s
/// `db_path`, creating the data directory first.
///
/// On a fresh install nothing else has created `~/.ein` yet (plugin install is
/// the only other thing that would), so the directory is ensured here before
/// SQLite tries to create the database file inside it.
pub(crate) async fn open_default_session_store() -> anyhow::Result<Arc<dyn SessionStore>> {
    let config = EinConfig::default();
    tokio::fs::create_dir_all(&config.data_dir).await?;
    Ok(Arc::new(SqliteSessionStore::open(&config.db_path).await?))
}

/// In release builds, download and install plugins if none are present.
async fn ensure_plugins_installed() -> anyhow::Result<()> {
    if !cfg!(debug_assertions) {
        let config = EinConfig::default();

        if plugins::plugins_missing(&config.plugin_dir).await {
            println!("No plugins found, downloading from GitHub release...");
            plugins::install_plugins(None).await?;
        }
    }

    Ok(())
}

/// Bind the gRPC service to `0.0.0.0:port` and serve until the process exits.
async fn serve(port: u16, server: AgentServer) -> anyhow::Result<()> {
    let addr = format!("0.0.0.0:{port}").parse()?;

    println!("eind listening on {addr}");

    Server::builder()
        .add_service(AgentServiceServer::new(server))
        .serve(addr)
        .await?;

    Ok(())
}

/// Top-level runtime configuration for the Ein server.
#[derive(Debug, Clone)]
pub struct EinConfig {
    /// Directory from which tool `.wasm` plugin files are loaded.
    pub plugin_dir: PathBuf,
    /// Directory from which model client `.wasm` plugin files are loaded.
    pub model_client_dir: PathBuf,
    /// Path to the SQLite session database.
    pub db_path: PathBuf,
    /// Base data directory (`~/.ein/`).
    pub data_dir: PathBuf,
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
            data_dir: ein_dir,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ein_config_default_data_dir_is_under_home() {
        let cfg = EinConfig::default();
        let home = dirs::home_dir().unwrap();
        assert!(
            cfg.data_dir.starts_with(&home),
            "data_dir {:?} should be under home {:?}",
            cfg.data_dir,
            home
        );
        assert!(cfg.data_dir.ends_with(".ein"));
    }

    #[test]
    fn ein_config_default_db_path_is_inside_data_dir() {
        let cfg = EinConfig::default();
        assert!(cfg.db_path.starts_with(&cfg.data_dir));
    }
}
