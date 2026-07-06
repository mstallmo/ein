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
// [`SessionStore`] against their own database and inject it via [`run_with`] or
// [`AgentServer::with_session_store`]. Runtime configuration is supplied the
// same way, as an [`EinConfig`] the caller owns rather than a library default.
pub use grpc::AgentServer;
pub use persistence::{SessionStore, SessionSummaryData, SqliteSessionStore};

// Re-exported so downstream `SessionStore` implementers can name the message
// types in the trait signatures without depending on `ein_plugin` directly.
pub use ein_plugin::model_client::{Message, Role};

use ein_proto::ein::agent_server::AgentServer as AgentServiceServer;
use std::path::PathBuf;
use std::sync::Arc;
use tonic::transport::Server;

/// Start the Ein gRPC server with the default [`EinConfig`] and the bundled
/// SQLite session store, blocking until it exits.
///
/// The zero-configuration entry point used by the standalone `eind` binary.
/// Embedders that need to control the plugin/data directories or supply their
/// own session store should use [`run_with`], pairing a caller-built
/// [`EinConfig`] with a [`SessionStore`] (the bundled default is available via
/// [`open_default_session_store`]).
pub async fn run(port: u16) -> anyhow::Result<()> {
    let config = EinConfig::default();
    let store = open_default_session_store(&config).await?;
    run_with(port, config, store).await
}

/// Start the server with a caller-supplied [`EinConfig`] and [`SessionStore`],
/// blocking until it exits.
///
/// The fully explicit counterpart to [`run`]: the runtime configuration and
/// session storage are entirely the caller's responsibility — the library
/// injects no defaults of its own on this path.
pub async fn run_with(
    port: u16,
    config: EinConfig,
    store: Arc<dyn SessionStore>,
) -> anyhow::Result<()> {
    ensure_plugins_installed(&config).await?;
    let server = AgentServer::with_session_store(config, store).await?;
    serve(port, server).await
}

/// Open the bundled SQLite-backed [`SessionStore`] at `config.db_path`,
/// creating the data directory first.
///
/// Exposed so callers that want a custom [`EinConfig`] but the default storage
/// can build the store to hand to [`run_with`]. On a fresh install nothing else
/// has created the data directory yet (plugin install is the only other thing
/// that would), so it is ensured here before SQLite tries to create the
/// database file inside it.
pub async fn open_default_session_store(
    config: &EinConfig,
) -> anyhow::Result<Arc<dyn SessionStore>> {
    tokio::fs::create_dir_all(&config.data_dir).await?;
    Ok(Arc::new(SqliteSessionStore::open(&config.db_path).await?))
}

/// In release builds, download and install plugins if none are present in
/// `config.plugin_dir`.
async fn ensure_plugins_installed(config: &EinConfig) -> anyhow::Result<()> {
    if !cfg!(debug_assertions) && plugins::plugins_missing(&config.plugin_dir).await {
        println!("No plugins found, downloading from GitHub release...");
        plugins::install_plugins(None).await?;
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
