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
use std::{env, process};

use async_openai::{Client, config::OpenAIConfig};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use wasmtime::Engine;
use wasmtime::component::{HasSelf, Linker};

use crate::HarnessState;
use crate::agent::{SessionParams, run_agent};
use crate::bindings::Plugin;
use crate::tools::ToolRegistry;
use ein_proto::ein::{AgentEvent, UserInput, agent_server::Agent, user_input};

/// gRPC service struct.
///
/// Holds shared, read-only resources (Wasmtime engine, linker, OpenRouter
/// client) behind `Arc`s so they can be cheaply cloned into each session task.
pub struct AgentServer {
    engine: Arc<Engine>,
    linker: Arc<Linker<HarnessState>>,
    config: Arc<crate::EinConfig>,
    client: Arc<Client<OpenAIConfig>>,
}

impl AgentServer {
    /// Creates a new `AgentServer`.
    ///
    /// - Initialises the Wasmtime engine and pre-populates the component
    ///   linker with WASI and the Ein plugin host functions.
    /// - Reads `OPENROUTER_API_KEY` (required) and `OPENROUTER_BASE_URL`
    ///   (optional) from the environment.
    pub fn new() -> anyhow::Result<Self> {
        let engine = Engine::default();
        let mut linker: Linker<HarnessState> = Linker::new(&engine);
        // Register standard WASI p2 host functions.
        wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
        // Register Ein-specific host functions (syscalls exposed to plugins).
        Plugin::add_to_linker::<HarnessState, HasSelf<HarnessState>>(&mut linker, |state| state)?;

        let base_url = env::var("OPENROUTER_BASE_URL")
            .unwrap_or_else(|_| "https://openrouter.ai/api/v1".to_string());

        let api_key = env::var("OPENROUTER_API_KEY").unwrap_or_else(|_| {
            eprintln!("OPENROUTER_API_KEY is not set");
            process::exit(1);
        });

        let config = OpenAIConfig::new()
            .with_api_base(base_url)
            .with_api_key(api_key);

        let client = Client::with_config(config);

        Ok(Self {
            engine: Arc::new(engine),
            linker: Arc::new(linker),
            config: Arc::new(crate::EinConfig::default()),
            client: Arc::new(client),
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
        let engine = self.engine.clone();
        let linker = self.linker.clone();
        let config = self.config.clone();
        let client = self.client.clone();

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

            let session_params = SessionParams {
                model: if session_cfg.model.is_empty() {
                    "anthropic/claude-haiku-4.5".to_string()
                } else {
                    session_cfg.model.clone()
                },
                max_tokens: if session_cfg.max_tokens == 0 {
                    2500
                } else {
                    session_cfg.max_tokens
                },
            };

            println!(
                "[session] config: model={}, max_tokens={}, allowed_paths={:?}, allowed_hosts={:?}",
                session_params.model,
                session_params.max_tokens,
                session_cfg.allowed_paths,
                session_cfg.allowed_hosts,
            );

            // --- Phase 2: load plugins with per-session constraints ---
            println!("[session] loading plugins from {}", config.plugin_dir.display());
            let registry = ToolRegistry::load(
                &engine,
                &linker,
                &config.plugin_dir,
                &session_cfg.allowed_paths,
                &session_cfg.allowed_hosts,
            )
            .await;
            let mut registry = match registry {
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

            // --- Phase 3: prompt loop ---
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
                let prompt = match msg.input {
                    Some(user_input::Input::Prompt(p)) => p,
                    _ => {
                        let _ = tx
                            .send(Err(Status::invalid_argument("Expected prompt after init")))
                            .await;
                        break;
                    }
                };
                println!("[session] prompt received ({} chars)", prompt.len());
                messages.push(json!({ "role": "user", "content": prompt }));
                if let Err(e) =
                    run_agent(&mut messages, &mut registry, &client, &session_params, &tx).await
                {
                    eprintln!("[session] agent error: {e}");
                    let _ = tx.send(Err(Status::internal(e.to_string()))).await;
                    break;
                }
            }

            println!("[session] session ended");
            registry.unload().await;
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}
