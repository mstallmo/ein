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

use std::sync::Arc;

use ein_agent::{Agent, AgentEvent, tools::ToolSet};
use ein_plugin::model_client::{Message, Role};
use ein_proto::ein::{
    AgentFinished, ContentDelta, TokenUsage, ToolCallEnd, ToolCallStart, ToolOutputChunk,
    agent_event,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use wasmtime::Engine;

use crate::model_client::ModelClientSessionManager;
use crate::tools::ToolSetManager;
use ein_proto::ein::{
    AgentError, AgentEvent as AgentEventProto, DeleteSessionRequest, DeleteSessionResponse,
    HistoryMessage, HistoryToolCall, ListSessionsRequest, ListSessionsResponse, SessionStarted,
    SessionSummary, UserInput, agent_event::Event, agent_server::Agent as AgentService, user_input,
};

/// gRPC service struct.
///
/// Holds shared resources behind `Arc`s so they can be cheaply cloned into
/// each session task. Model client plugins are compiled lazily on first use
/// and cached — only plugins that are actually requested ever consume memory.
pub struct AgentServer {
    config: Arc<crate::EinConfig>,
    model_client_session_manager: ModelClientSessionManager,
    tool_set_manager: ToolSetManager,
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
        let config = Arc::new(crate::EinConfig::default());
        let engine = Engine::default();

        let model_client_session_manager =
            ModelClientSessionManager::new(&config.model_client_dir, engine.clone()).await?;
        let tool_set_manager = ToolSetManager::new(&config.plugin_dir, engine).await?;
        let session_store =
            Arc::new(crate::persistence::SessionStore::open(&config.db_path).await?);

        Ok(Self {
            config,
            model_client_session_manager,
            tool_set_manager,
            session_store,
        })
    }
}

#[tonic::async_trait]
impl AgentService for AgentServer {
    type AgentSessionStream = ReceiverStream<Result<AgentEventProto, Status>>;

    async fn list_sessions(
        &self,
        _request: Request<ListSessionsRequest>,
    ) -> Result<Response<ListSessionsResponse>, Status> {
        let summaries = self
            .session_store
            .list_sessions()
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        Ok(Response::new(ListSessionsResponse {
            sessions: summaries
                .into_iter()
                .map(|s| SessionSummary {
                    id: s.id,
                    created_at: s.created_at,
                    preview: s.preview,
                    session_config_json: s.session_config_json,
                })
                .collect(),
        }))
    }

    async fn delete_session(
        &self,
        request: Request<DeleteSessionRequest>,
    ) -> Result<Response<DeleteSessionResponse>, Status> {
        let id = request.into_inner().session_id;
        self.session_store
            .delete_session(&id)
            .await
            .map_err(|e| Status::internal(e.to_string()))?;

        println!("[session] deleted session {id}");
        Ok(Response::new(DeleteSessionResponse {}))
    }

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
        let channel_sender = AgentEventSender::new(tx);

        // Clone Arcs — cheap reference-count bumps, no data is copied.
        let config = self.config.clone();
        let model_client_session_manager = self.model_client_session_manager.clone();
        let tool_set_manager = self.tool_set_manager.clone();
        let session_store = self.session_store.clone();

        tokio::spawn(async move {
            println!("[session] new session started");
            let mut inbound = request.into_inner();

            // --- Phase 1: read and apply SessionConfig ---
            let session_cfg = match inbound.message().await {
                Ok(Some(msg)) => match msg.input {
                    Some(user_input::Input::Init(cfg)) => cfg,
                    _ => {
                        channel_sender
                            .send_error(Status::invalid_argument(
                                "First message must be SessionConfig (init variant)",
                            ))
                            .await;
                        return;
                    }
                },
                Ok(None) => return, // client disconnected immediately
                Err(e) => {
                    channel_sender
                        .send_error(Status::internal(e.to_string()))
                        .await;
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
                        channel_sender
                            .send_error(Status::invalid_argument(format!(
                                "session_id must be a valid UUID, got: {raw_id}"
                            )))
                            .await;
                        return;
                    }

                    let exists = match session_store.session_exists(&raw_id).await {
                        Ok(exists) => exists,
                        Err(e) => {
                            eprintln!(
                                "[session] failed to check session existence for {raw_id}: {e}"
                            );

                            channel_sender
                                .send_error(Status::internal(format!(
                                    "Failed to check session: {e}"
                                )))
                                .await;

                            return;
                        }
                    };

                    (raw_id.to_string(), exists)
                }
            };

            if !is_resumed {
                let config_record = crate::persistence::SessionConfigRecord::from(&session_cfg);
                let config_json = serde_json::to_string(&config_record)
                    .expect("SessionConfigRecord contains only serialisable primitive types");

                if let Err(e) = session_store
                    .create_session(&session_id, &config_json)
                    .await
                {
                    eprintln!("[session] failed to persist new session {session_id}: {e}");

                    channel_sender
                        .send_error(Status::internal(format!("Failed to create session: {e}")))
                        .await;

                    return;
                }
            }

            println!(
                "[session] {} session {session_id}",
                if is_resumed { "resumed" } else { "created" }
            );

            // --- Phase 2: get (or prepare) model client, then instantiate ---

            let model_session = match model_client_session_manager.new_session(&session_cfg).await {
                Ok(model_session) => model_session,
                Err(err) => {
                    channel_sender
                        .send_error(Status::internal(err.to_string()))
                        .await;

                    return;
                }
            };

            // --- Phase 3: load tool plugins with per-session constraints ---
            println!(
                "[session] loading plugins from {}",
                config.plugin_dir.display()
            );
            let tool_set = match tool_set_manager.new_tool_set(&session_cfg).await {
                Ok(r) => r,
                Err(e) => {
                    channel_sender
                        .send_error(Status::internal(format!("Failed to load plugins: {e}")))
                        .await;

                    return;
                }
            };

            println!("[session] plugins loaded");

            // --- Phase 4: prompt loop ---
            // Restore history if resuming; otherwise start fresh with an optional system message.
            let messages: Vec<Message> = if is_resumed {
                match session_store.load_messages(&session_id).await {
                    Ok(Some(msgs)) => msgs,
                    Ok(None) => vec![], // session exists but has no messages yet
                    Err(e) => {
                        eprintln!("[session] failed to load messages for {session_id}: {e}");

                        channel_sender
                            .send_error(Status::internal(format!(
                                "Failed to load session history: {e}"
                            )))
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

            // Build history for the client when resuming an existing session.
            // Must happen before tool_set is moved into the agent.
            let history: Vec<HistoryMessage> = if is_resumed {
                messages
                    .iter()
                    .filter_map(|m| match m.role {
                        Role::User => Some(HistoryMessage {
                            role: "user".to_string(),
                            content: m.content.clone().unwrap_or_default(),
                            tool_calls: vec![],
                        }),
                        Role::Assistant => {
                            let tool_calls = m
                                .tool_calls
                                .as_deref()
                                .unwrap_or(&[])
                                .iter()
                                .map(|tc| match tc {
                                    ein_plugin::model_client::ToolCall::Function {
                                        function,
                                        ..
                                    } => HistoryToolCall {
                                        tool_name: function.name.clone(),
                                        arguments: function.arguments.clone(),
                                        display_arg: tool_set
                                            .display_arg_for(&function.name, &function.arguments)
                                            .unwrap_or_default(),
                                    },
                                })
                                .collect();
                            Some(HistoryMessage {
                                role: "assistant".to_string(),
                                content: m.content.clone().unwrap_or_default(),
                                tool_calls,
                            })
                        }
                        _ => None,
                    })
                    .collect()
            } else {
                vec![]
            };

            // Notify the client of the assigned session ID before any agent events.
            channel_sender
                .send_event(Event::SessionStarted(SessionStarted {
                    session_id: session_id.clone(),
                    resumed: is_resumed,
                    history,
                }))
                .await;

            let channel_sender_clone = channel_sender.clone();
            let mut agent = Agent::builder_with_tool_set(model_session, tool_set)
                .with_message_history(messages.clone())
                .with_event_handler(move |event| {
                    let channel_sender = channel_sender_clone.clone();

                    async move {
                        let proto_event = match event {
                            AgentEvent::ContentDelta(content) => {
                                Event::ContentDelta(ContentDelta { text: content })
                            }
                            AgentEvent::ToolCallStart {
                                tool_call_id,
                                tool_name,
                                arguments,
                                display_arg,
                            } => Event::ToolCallStart(ToolCallStart {
                                tool_call_id,
                                tool_name,
                                arguments,
                                display_arg: display_arg.unwrap_or_default(),
                            }),
                            AgentEvent::ToolOutputChunk {
                                tool_call_id,
                                output,
                            } => Event::ToolOutputChunk(ToolOutputChunk {
                                tool_call_id,
                                output,
                            }),
                            AgentEvent::ToolCallEnd {
                                tool_call_id,
                                tool_name,
                                result,
                                metadata,
                            } => Event::ToolCallEnd(ToolCallEnd {
                                tool_call_id,
                                tool_name,
                                result,
                                metadata,
                            }),
                            AgentEvent::TokenUsage {
                                prompt_tokens,
                                completion_tokens,
                                total_tokens,
                            } => Event::TokenUsage(TokenUsage {
                                prompt_tokens: prompt_tokens as i32,
                                completion_tokens: completion_tokens as i32,
                                total_tokens: total_tokens as i32,
                            }),
                        };

                        channel_sender.send_event(proto_event).await;
                    }
                })
                .build();

            while let Ok(Some(msg)) = inbound.message().await {
                match msg.input {
                    Some(user_input::Input::Prompt(prompt)) => {
                        println!("[session] prompt received ({} chars)", prompt.len());

                        let res = match agent.chat(prompt).await {
                            Ok(content) => content.content.unwrap_or_default(),
                            Err(err) => {
                                eprintln!("[session] agent error: {err}");
                                channel_sender
                                    .send_error(Status::internal(err.to_string()))
                                    .await;
                                // Deliberate: we do not call save_messages here because this hard-error
                                // path is only reached by catastrophic transport failures. Soft errors
                                // (API errors, HTTP failures) are returned as Ok(()) by run_agent and
                                // reach the save_messages call below.
                                break;
                            }
                        };

                        channel_sender
                            .send_event(Event::AgentFinished(AgentFinished { final_content: res }))
                            .await;

                        // Persist updated history after every agent turn.
                        if let Err(err) = session_store
                            .save_messages(&session_id, agent.messages())
                            .await
                        {
                            eprintln!("[session] failed to save messages for {session_id}: {err}");
                        }
                    }
                    Some(user_input::Input::ConfigUpdate(cfg)) => {
                        println!("[session] config updated");

                        match model_client_session_manager.new_session(&cfg).await {
                            Ok(new_session) => {
                                agent.replace_model_client(new_session).await;

                                println!("[session] model client updated from config change");
                            }
                            Err(err) => {
                                channel_sender
                                    .send_event(Event::AgentError(AgentError {
                                        message: format!("Config update failed: {err}"),
                                    }))
                                    .await;
                                continue;
                            }
                        }
                    }
                    Some(user_input::Input::ClearContext(should_clear)) => {
                        if should_clear {
                            // Intentionally skip save_messages — the SQLite history
                            // is preserved; only the in-memory LLM context is wiped.
                            println!("[session] context cleared");
                            agent.clear_messages();
                        }
                    }
                    Some(user_input::Input::CompactContext(should_compact)) => {
                        if should_compact {
                            println!("[session] compacting context");

                            match agent.compact_history().await {
                                Ok(_) => {
                                    // compact_history already broadcast ContentDelta events.
                                    channel_sender
                                        .send_event(Event::AgentFinished(AgentFinished {
                                            final_content: String::new(),
                                        }))
                                        .await;

                                    // Persist the compacted history so resumed sessions stay compact.
                                    if let Err(err) = session_store
                                        .save_messages(&session_id, agent.messages())
                                        .await
                                    {
                                        eprintln!(
                                            "[session] failed to save compacted messages for {session_id}: {err}"
                                        );
                                    }
                                }
                                Err(err) => {
                                    eprintln!("[session] compact error: {err}");
                                    channel_sender
                                        .send_event(Event::AgentError(AgentError {
                                            message: format!("Compact failed: {err}"),
                                        }))
                                        .await;
                                }
                            }
                        }
                    }
                    _ => {
                        channel_sender
                            .send_error(Status::invalid_argument(
                                "Expected prompt or config_update after init",
                            ))
                            .await;
                        break;
                    }
                }
            }

            println!("[session] session ended");
            agent.cleanup().await;
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

#[derive(Clone)]
struct AgentEventSender(mpsc::Sender<Result<AgentEventProto, Status>>);

impl AgentEventSender {
    pub fn new(tx: mpsc::Sender<Result<AgentEventProto, Status>>) -> Self {
        Self(tx)
    }

    pub async fn send_event(&self, event: agent_event::Event) {
        let _ = self
            .0
            .send(Ok(AgentEventProto { event: Some(event) }))
            .await;
    }

    pub async fn send_error(&self, status: Status) {
        let _ = self.0.send(Err(status)).await;
    }
}
