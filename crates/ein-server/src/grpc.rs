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

use ein_plugin::model_client::{Message, Role};
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
    AgentError, AgentEvent, SessionStarted, UserInput, agent_event::Event, agent_server::Agent,
    user_input,
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
    session_store: Arc<crate::persistence::SessionStore>,
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
        let session_store =
            Arc::new(crate::persistence::SessionStore::open(&config.db_path).await?);

        Ok(Self {
            config,
            engine,
            model_client_session_manager,
            tool_linker,
            session_store,
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
        let session_store = self.session_store.clone();

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

            // --- Session persistence: create or resume ---
            let (session_id, is_resumed) = {
                let raw_id = session_cfg.session_id.trim().to_string();
                if raw_id.is_empty() {
                    (uuid::Uuid::now_v7().to_string(), false)
                } else {
                    // Reject non-UUID session IDs to catch typos early and enforce the protocol
                    // contract stated in the proto comment.
                    if uuid::Uuid::parse_str(&raw_id).is_err() {
                        let _ = tx
                            .send(Err(Status::invalid_argument(format!(
                                "session_id must be a valid UUID, got: {raw_id}"
                            ))))
                            .await;
                        return;
                    }
                    let exists = session_store.session_exists(&raw_id).await.unwrap_or(false);
                    (raw_id, exists)
                }
            };

            if !is_resumed {
                let config_record = crate::persistence::SessionConfigRecord::from(&session_cfg);
                let config_json = serde_json::to_string(&config_record).unwrap_or_default();
                if let Err(e) = session_store.create_session(&session_id, &config_json).await {
                    eprintln!("[session] failed to persist new session {session_id}: {e}");
                    let _ = tx
                        .send(Err(Status::internal(format!("Failed to create session: {e}"))))
                        .await;
                    return;
                }
            }

            println!(
                "[session] {} session {session_id}",
                if is_resumed { "resumed" } else { "created" }
            );

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
            // Restore history if resuming; otherwise start fresh with an optional system message.
            let mut messages: Vec<Message> = if is_resumed {
                match session_store.load_messages(&session_id).await {
                    Ok(Some(msgs)) => msgs,
                    Ok(None) => vec![], // session exists but has no messages yet
                    Err(e) => {
                        eprintln!("[session] failed to load messages for {session_id}: {e}");
                        let _ = tx
                            .send(Err(Status::internal(format!(
                                "Failed to load session history: {e}"
                            ))))
                            .await;
                        return;
                    }
                }
            } else {
                let mut msgs = vec![];
                // Prepend a system message so the model knows which filesystem
                // paths the file tools (Read, Write, Edit) are allowed to access.
                if !session_cfg.allowed_paths.is_empty() {
                    let paths_list = session_cfg
                        .allowed_paths
                        .iter()
                        .map(|p| format!("- {p}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    msgs.push(Message {
                        role: Role::System,
                        content: Some(format!(
                            "The following filesystem paths are accessible to file tools (Read, Write, Edit):\n{paths_list}"
                        )),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                msgs
            };

            // Notify the client of the assigned session ID before any agent events.
            let _ = tx
                .send(Ok(AgentEvent {
                    event: Some(Event::SessionStarted(SessionStarted {
                        session_id: session_id.clone(),
                        resumed: is_resumed,
                    })),
                }))
                .await;

            while let Ok(Some(msg)) = inbound.message().await {
                match msg.input {
                    Some(user_input::Input::Prompt(prompt)) => {
                        println!("[session] prompt received ({} chars)", prompt.len());
                        messages.push(Message {
                            role: Role::User,
                            content: Some(prompt),
                            tool_calls: None,
                            tool_call_id: None,
                        });

                        if let Err(e) =
                            run_agent(&mut messages, &mut registry, &mut model_session, &tx).await
                        {
                            eprintln!("[session] agent error: {e}");
                            let _ = tx.send(Err(Status::internal(e.to_string()))).await;
                            break;
                        }

                        // Persist updated history after every agent turn.
                        if let Err(e) = session_store.save_messages(&session_id, &messages).await {
                            eprintln!("[session] failed to save messages for {session_id}: {e}");
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
