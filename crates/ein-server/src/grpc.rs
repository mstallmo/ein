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
use crate::agent::run_agent;
use crate::bindings::Plugin;
use crate::tools::ToolRegistry;
use ein_proto::ein::{AgentEvent, UserInput, agent_server::Agent};

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
            // Load plugins fresh for each session so sessions are isolated.
            let registry = ToolRegistry::load(&engine, &linker, &config.plugin_dir).await;
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

            // `messages` accumulates the full conversation history for this
            // session in OpenAI chat-completion format.
            let mut messages: Vec<Value> = vec![];
            let mut inbound = request.into_inner();

            while let Ok(Some(user_input)) = inbound.message().await {
                messages.push(json!({ "role": "user", "content": user_input.prompt }));
                if let Err(e) = run_agent(&mut messages, &mut registry, &client, &tx).await {
                    let _ = tx.send(Err(Status::internal(e.to_string()))).await;
                    break;
                }
            }

            registry.unload().await;
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}
