// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

//! Host-side WASM execution engine for Ein tool and model-client plugins.
//!
//! This crate owns the Wasmtime machinery that loads, links, and runs the
//! `wasm32-wasip2` component plugins. It exposes two per-session factories —
//! [`ToolSetManager`] (tool plugins) and [`ModelClientSessionManager`] (model
//! client plugins) — bundled behind a single [`PluginRuntime`] that shares one
//! [`wasmtime::Engine`] between them.
//!
//! The engine is deliberately decoupled from the gRPC wire format: callers
//! describe a session with the plain [`ToolSessionSpec`] / [`ModelClientSpec`]
//! types rather than a protobuf message. The instantiated plugins implement the
//! [`ein_agent`] `ToolSet` / `ModelClient` traits so they drop straight into the
//! agent loop.

mod model_client;
mod tools;

use std::collections::HashMap;
use std::path::Path;

use wasmtime::Engine;

pub use model_client::{ModelClientSession, ModelClientSessionManager};
pub use tools::{ToolSetManager, WasmToolSet};

/// Filesystem and network access granted to a plugin instance.
///
/// The engine merges the session-global constraints with any per-plugin
/// overrides before building each plugin's WASI context.
#[derive(Debug, Clone, Default)]
pub struct PluginConstraints {
    /// Host filesystem paths preopened for the plugin.
    pub allowed_paths: Vec<String>,
    /// Hostnames the plugin may connect to (`"*"` = allow all; empty = deny all).
    pub allowed_hosts: Vec<String>,
}

/// Everything the [`ToolSetManager`] needs to build a session's tool set.
#[derive(Debug, Clone, Default)]
pub struct ToolSessionSpec {
    /// Constraints applied to every tool plugin.
    pub global: PluginConstraints,
    /// Per-plugin constraint overrides, keyed by plugin filename stem
    /// (e.g. `"ein_bash"`). Merged with [`global`](Self::global).
    pub overrides: HashMap<String, PluginConstraints>,
}

/// Everything the [`ModelClientSessionManager`] needs to instantiate a session's
/// model client.
#[derive(Debug, Clone, Default)]
pub struct ModelClientSpec {
    /// Plugin filename stem to use (e.g. `"ein_openrouter"`). `None` (or empty)
    /// selects the manager's scanned fallback plugin.
    pub client_name: Option<String>,
    /// Per-plugin config JSON blobs, keyed by plugin filename stem. The selected
    /// client's entry is passed to the plugin constructor; a missing entry is
    /// treated as `"{}"`.
    pub plugin_params: HashMap<String, String>,
}

/// The host-side WASM runtime shared across all sessions.
///
/// Owns a single [`Engine`] and the two per-session plugin factories. Cheap to
/// clone — the underlying engine, linkers, and compile caches are all
/// reference-counted.
#[derive(Clone)]
pub struct PluginRuntime {
    tools: ToolSetManager,
    model_clients: ModelClientSessionManager,
}

impl PluginRuntime {
    /// Builds the runtime, initialising the Wasmtime engine and both plugin
    /// linkers, and scanning `model_client_dir` for the fallback model client.
    ///
    /// No plugin is compiled here: tool plugins are loaded per session and model
    /// client plugins are compiled lazily on first use.
    pub async fn new<P: AsRef<Path>, Q: AsRef<Path>>(
        tool_dir: P,
        model_client_dir: Q,
    ) -> anyhow::Result<Self> {
        let engine = Engine::default();
        let model_clients =
            ModelClientSessionManager::new(model_client_dir, engine.clone()).await?;
        let tools = ToolSetManager::new(tool_dir, engine).await?;

        Ok(Self {
            tools,
            model_clients,
        })
    }

    /// The tool-plugin factory.
    pub fn tools(&self) -> &ToolSetManager {
        &self.tools
    }

    /// The model-client-plugin factory.
    pub fn model_clients(&self) -> &ModelClientSessionManager {
        &self.model_clients
    }
}
