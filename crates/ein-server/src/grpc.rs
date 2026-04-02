// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

//! gRPC service implementation.
//!
//! [`AgentServer`] implements the `Agent` service from `ein.proto`. Each call
//! to [`AgentServer::agent_session`] spawns a dedicated Tokio task that owns
//! the WASM plugin registry and conversation history for that session, keeping
//! sessions fully isolated from one another.
//!
//! ## Session lifecycle
//!
//! 1. Client opens a bidirectional stream via `AgentSession`.
//! 2. The server loads WASM plugins from the configured plugin directory.
//! 3. For each `UserInput` message received from the client, the server
//!    appends it to the message history and calls [`run_agent`], which
//!    streams `AgentEvent`s back while driving the tool-call loop.
//! 4. When the client closes the inbound stream, the session task exits and
//!    plugins are unloaded.

use std::mem;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use wasmtime::Engine;
use wasmtime::component::Linker;

use crate::HarnessState;
use crate::agent::run_agent;
use crate::model_client::ModelClientSessionManager;
use crate::tools::{ToolRegistry, build_tool_linker};
use ein_proto::ein::{
    AgentError, AgentEvent, UserInput, agent_event::Event, agent_server::Agent, user_input,
};

/// gRPC service struct.
///
/// Holds shared resources behind `Arc`s so they can be cheaply cloned into
/// each session task. Model client plugins are compiled lazily on first use
/// and cached — only plugins that are actually requested ever consume memory.
pub struct AgentServer {
    config: Arc<crate::EinConfig>,
    engine: Engine,
    model_client_session_manager: ModelClientSessionManager,
    tool_linker: Arc<Linker<HarnessState>>,
}

impl AgentServer {
    /// Creates a new `AgentServer`.
    ///
    /// - Initialises the Wasmtime engine and pre-populates the component
    ///   linkers with WASI and the Ein plugin host functions.
    /// - Scans the model client directory to determine the fallback plugin
    ///   name; no WASM compilation happens at this point.
    pub async fn new() -> anyhow::Result<Self> {
        let engine = Engine::default();
        let config = Arc::new(crate::EinConfig::default());

        let model_client_session_manager =
            ModelClientSessionManager::new(&config.model_client_dir, engine.clone()).await?;
        let tool_linker = Arc::new(build_tool_linker(&engine)?);

        Ok(Self {
            config,
            engine,
            model_client_session_manager,
            tool_linker,
        })
    }
}

#[tonic::async_trait]
impl Agent for AgentServer {
    type AgentSessionStream = ReceiverStream<Result<AgentEvent, Status>>;

    /// Handles one client session.
    ///
    /// Spawns a background task that owns the session state. Events are sent
    /// through an mpsc channel whose receiver end is wrapped in a
    /// `ReceiverStream` and returned to tonic as the response stream.
    async fn agent_session(
        &self,
        request: Request<Streaming<UserInput>>,
    ) -> Result<Response<Self::AgentSessionStream>, Status> {
        let (tx, rx) = mpsc::channel(32);

        // Clone Arcs — cheap reference-count bumps, no data is copied.
        let config = self.config.clone();
        let engine = self.engine.clone();
        let model_client_session_manager = self.model_client_session_manager.clone();
        let tool_linker = self.tool_linker.clone();

        tokio::spawn(async move {
            println!("[session] new session started");
            let mut inbound = request.into_inner();

            // --- Phase 1: read and apply SessionConfig ---
            let session_cfg = match inbound.message().await {
                Ok(Some(msg)) => match msg.input {
                    Some(user_input::Input::Init(cfg)) => cfg,
                    _ => {
                        let _ = tx
                            .send(Err(Status::invalid_argument(
                                "First message must be SessionConfig (init variant)",
                            )))
                            .await;
                        return;
                    }
                },
                Ok(None) => return, // client disconnected immediately
                Err(e) => {
                    let _ = tx.send(Err(Status::internal(e.to_string()))).await;
                    return;
                }
            };

            // --- Phase 2: get (or prepare) model client, then instantiate ---

            let mut model_session =
                match model_client_session_manager.new_session(&session_cfg).await {
                    Ok(model_session) => model_session,
                    Err(err) => {
                        let _ = tx.send(Err(Status::internal(err.to_string()))).await;
                        return;
                    }
                };

            // --- Phase 3: load tool plugins with per-session constraints ---
            println!(
                "[session] loading plugins from {}",
                config.plugin_dir.display()
            );
            let mut registry = match ToolRegistry::load(
                &engine,
                &tool_linker,
                &config.plugin_dir,
                &session_cfg.allowed_paths,
                &session_cfg.allowed_hosts,
                &session_cfg.plugin_configs,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx
                        .send(Err(Status::internal(format!(
                            "Failed to load plugins: {e}"
                        ))))
                        .await;
                    return;
                }
            };

            println!("[session] plugins loaded");

            // --- Phase 4: prompt loop ---
            // `messages` accumulates the full conversation history for this
            // session in OpenAI chat-completion format.
            let mut messages: Vec<Value> = vec![];

            // Prepend a system message so the model knows which filesystem
            // paths the file tools (Read, Write, Edit) are allowed to access.
            if !session_cfg.allowed_paths.is_empty() {
                let paths_list = session_cfg
                    .allowed_paths
                    .iter()
                    .map(|p| format!("- {p}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                messages.push(json!({
                    "role": "system",
                    "content": format!(
                        "The following filesystem paths are accessible to file tools (Read, Write, Edit):\n{paths_list}"
                    ),
                }));
            }

            while let Ok(Some(msg)) = inbound.message().await {
                match msg.input {
                    Some(user_input::Input::Prompt(prompt)) => {
                        println!("[session] prompt received ({} chars)", prompt.len());
                        messages.push(json!({ "role": "user", "content": prompt }));

                        if let Err(e) =
                            run_agent(&mut messages, &mut registry, &mut model_session, &tx).await
                        {
                            eprintln!("[session] agent error: {e}");
                            let _ = tx.send(Err(Status::internal(e.to_string()))).await;
                            break;
                        }
                    }
                    Some(user_input::Input::ConfigUpdate(cfg)) => {
                        println!("[session] config updated");

                        match model_client_session_manager.new_session(&cfg).await {
                            Ok(new_session) => {
                                let old_session = mem::replace(&mut model_session, new_session);
                                if let Err(err) = old_session.cleanup().await {
                                    eprintln!("[session] Failed to cleanup model client {err}");
                                }

                                println!("[session] model client updated from config change");
                            }
                            Err(err) => {
                                let _ = tx
                                    .send(Ok(AgentEvent {
                                        event: Some(Event::AgentError(AgentError {
                                            message: format!("Config update failed: {err}"),
                                        })),
                                    }))
                                    .await;
                                continue;
                            }
                        }
                    }
                    _ => {
                        let _ = tx
                            .send(Err(Status::invalid_argument(
                                "Expected prompt or config_update after init",
                            )))
                            .await;
                        break;
                    }
                }
            }

            println!("[session] session ended");
            if let Err(err) = model_session.cleanup().await {
                eprintln!("[session] Failed to cleanup model client {err}");
            }
            registry.unload().await;
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}
