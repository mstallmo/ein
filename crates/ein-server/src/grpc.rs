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

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use wasmtime::Engine;
use wasmtime::component::Linker;

use crate::HarnessState;
use crate::ModelClientHarnessState;
use crate::agent::{SessionParams, run_agent};
use crate::model_client::{
    build_model_client_linker, instantiate_model_client, load_model_client_component,
};
use crate::tools::{ToolRegistry, build_tool_linker};
use ein_proto::ein::{
    AgentError, AgentEvent, UserInput, agent_event::Event, agent_server::Agent, user_input,
};
use wasmtime::component::Component;

/// gRPC service struct.
///
/// Holds shared, read-only resources (Wasmtime engine, linkers, compiled
/// model client component) behind `Arc`s so they can be cheaply cloned into
/// each session task. The model client is instantiated per session with the
/// session's credentials.
pub struct AgentServer {
    config: Arc<crate::EinConfig>,
    engine: Arc<Engine>,
    model_client_linker: Arc<Linker<ModelClientHarnessState>>,
    model_client_component: Arc<Component>,
    tool_linker: Arc<Linker<HarnessState>>,
}

/// Builds the JSON config and allowed-hosts list for a model client instantiation.
///
/// - Empty `base_url` → deny all outbound hosts (`[]`).
/// - `base_url == "*"` → allow all hosts; `"*"` is NOT forwarded to the plugin as a URL.
/// - Any real URL → extract the hostname and allowlist only that host.
fn build_model_config(api_key: &str, base_url: &str) -> (String, Vec<String>) {
    let mut config = json!({ "api_key": api_key });

    let allowed_hosts = if base_url.is_empty() {
        vec![]
    } else if base_url == "*" {
        vec!["*".to_string()]
    } else {
        config["base_url"] = base_url.into();
        base_url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .and_then(|authority| authority.split(':').next())
            .map(|host| vec![host.to_string()])
            .unwrap_or_default()
    };

    (config.to_string(), allowed_hosts)
}

impl AgentServer {
    /// Creates a new `AgentServer`.
    ///
    /// - Initialises the Wasmtime engine and pre-populates the component
    ///   linker with WASI and the Ein plugin host functions.
    /// - Compiles the model client WASM component once; credentials are
    ///   supplied per-session via `SessionConfig`.
    pub async fn new() -> anyhow::Result<Self> {
        let engine = Engine::default();
        let config = Arc::new(crate::EinConfig::default());

        let model_client_linker = Arc::new(build_model_client_linker(&engine)?);
        let model_client_component =
            Arc::new(load_model_client_component(&engine, &config.model_client_dir).await?);

        let tool_linker = Arc::new(build_tool_linker(&engine)?);

        Ok(Self {
            config,
            engine: Arc::new(engine),
            model_client_linker,
            model_client_component,
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
        let model_client_linker = self.model_client_linker.clone();
        let model_client_component = self.model_client_component.clone();
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

            let mut session_params = SessionParams {
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

            // --- Phase 2: instantiate model client with session credentials ---
            let (model_config_json, model_allowed_hosts) =
                build_model_config(&session_cfg.api_key, &session_cfg.base_url);
            println!(
                "[session] model client: base_url={:?}, allowed_hosts={:?}",
                if session_cfg.base_url.is_empty() {
                    "<plugin default>"
                } else {
                    &session_cfg.base_url
                },
                model_allowed_hosts,
            );

            let mut model = match instantiate_model_client(
                &engine,
                &model_client_linker,
                &model_client_component,
                &model_config_json,
                &model_allowed_hosts,
            )
            .await
            {
                Ok(m) => m,
                Err(e) => {
                    let _ = tx
                        .send(Err(Status::internal(format!(
                            "Failed to instantiate model client: {e}"
                        ))))
                        .await;
                    return;
                }
            };

            // --- Phase 3: load tool plugins with per-session constraints ---
            println!(
                "[session] loading plugins from {}",
                config.plugin_dir.display()
            );
            let registry = ToolRegistry::load(
                &engine,
                &tool_linker,
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
                match msg.input {
                    Some(user_input::Input::Prompt(prompt)) => {
                        println!("[session] prompt received ({} chars)", prompt.len());
                        messages.push(json!({ "role": "user", "content": prompt }));

                        if let Err(e) = run_agent(
                            &mut messages,
                            &mut registry,
                            &mut model,
                            &session_params,
                            &tx,
                        )
                        .await
                        {
                            eprintln!("[session] agent error: {e}");
                            let _ = tx.send(Err(Status::internal(e.to_string()))).await;
                            break;
                        }
                    }
                    Some(user_input::Input::ConfigUpdate(cfg)) => {
                        if !cfg.model.is_empty() {
                            session_params.model = cfg.model.clone();
                        }
                        if cfg.max_tokens != 0 {
                            session_params.max_tokens = cfg.max_tokens;
                        }
                        let (new_config_json, new_allowed_hosts) =
                            build_model_config(&cfg.api_key, &cfg.base_url);
                        println!("[session] config updated: {new_config_json}");

                        match instantiate_model_client(
                            &engine,
                            &model_client_linker,
                            &model_client_component,
                            &new_config_json,
                            &new_allowed_hosts,
                        )
                        .await
                        {
                            Ok(m) => {
                                model = m;
                                println!("[session] model client updated from config change");
                            }
                            Err(e) => {
                                let _ = tx
                                    .send(Ok(AgentEvent {
                                        event: Some(Event::AgentError(AgentError {
                                            message: format!("Config update failed: {e}"),
                                        })),
                                    }))
                                    .await;
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
            if let Err(err) = model.cleanup().await {
                eprintln!("[session] Failed to cleanup model client {err}");
            }
            registry.unload().await;
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}
