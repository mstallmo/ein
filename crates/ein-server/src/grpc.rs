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

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use wasmtime::Engine;
use wasmtime::component::Linker;

use crate::HarnessState;
use crate::ModelClientHarnessState;
use crate::agent::{SessionParams, run_agent};
use crate::model_client::{
    build_model_client_linker, compile_model_client_component, instantiate_model_client,
    scan_model_client_name,
};
use crate::tools::{ToolRegistry, build_tool_linker, merge_dedup};
use ein_proto::ein::{
    AgentError, AgentEvent, UserInput, agent_event::Event, agent_server::Agent, user_input,
};
use wasmtime::component::Component;

/// gRPC service struct.
///
/// Holds shared resources behind `Arc`s so they can be cheaply cloned into
/// each session task. Model client plugins are compiled lazily on first use
/// and cached — only plugins that are actually requested ever consume memory.
pub struct AgentServer {
    config: Arc<crate::EinConfig>,
    engine: Arc<Engine>,
    model_client_linker: Arc<Linker<ModelClientHarnessState>>,
    /// Compiled model client components, keyed by plugin name. Populated
    /// on demand the first time a session requests a given plugin.
    model_client_cache: Arc<Mutex<HashMap<String, Arc<Component>>>>,
    /// Fallback plugin name when the client does not specify one — derived
    /// by scanning the plugin directory at startup (no compilation).
    fallback_model_client_name: Option<Arc<str>>,
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

/// Extracts model client parameters from a `SessionConfig`. Uses
/// `session_cfg.model_client_name` to identify the plugin, falling back to
/// `fallback_name` when the client sends an empty string.
///
/// Returns the JSON config string, resolved allowed-hosts list, session
/// parameters, and the resolved plugin name.
fn extract_model_params(
    session_cfg: &ein_proto::ein::SessionConfig,
    fallback_name: Option<&str>,
) -> (String, Vec<String>, SessionParams, String) {
    let model_client_name = if session_cfg.model_client_name.is_empty() {
        fallback_name.unwrap_or("ein_openrouter").to_string()
    } else {
        session_cfg.model_client_name.clone()
    };

    let pc = session_cfg.plugin_configs.get(&model_client_name);
    let api_key = pc
        .and_then(|p| p.config.get("api_key"))
        .cloned()
        .unwrap_or_default();
    let base_url = pc
        .and_then(|p| p.config.get("base_url"))
        .cloned()
        .unwrap_or_default();
    let model = pc
        .and_then(|p| p.config.get("model"))
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "anthropic/claude-haiku-4.5".to_string());
    let max_tokens = pc
        .and_then(|p| p.config.get("max_tokens"))
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(2500);

    let plugin_extra_hosts = pc.map(|p| p.allowed_hosts.as_slice()).unwrap_or(&[]);
    let (model_config_json, derived_hosts) = build_model_config(&api_key, &base_url);
    let allowed_hosts = merge_dedup(&derived_hosts, plugin_extra_hosts);

    let params = SessionParams { model, max_tokens };
    (model_config_json, allowed_hosts, params, model_client_name)
}

/// Returns the compiled [`Component`] for `name`, compiling it from disk on
/// first use and caching it for subsequent sessions.
async fn get_or_compile_model_client(
    engine: &Arc<Engine>,
    model_client_dir: &std::path::Path,
    cache: &Mutex<HashMap<String, Arc<Component>>>,
    name: &str,
) -> anyhow::Result<Arc<Component>> {
    // Fast path: already compiled.
    {
        let lock = cache.lock().await;
        if let Some(component) = lock.get(name) {
            return Ok(component.clone());
        }
    }

    // Slow path: compile from disk (CPU-bound, run in blocking thread pool).
    let component = compile_model_client_component(engine, model_client_dir, name).await?;
    let component = Arc::new(component);

    // Insert into cache; if another session raced us, keep the first winner.
    let mut lock = cache.lock().await;
    Ok(lock.entry(name.to_string()).or_insert(component).clone())
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

        let model_client_linker = Arc::new(build_model_client_linker(&engine)?);
        let fallback_model_client_name = scan_model_client_name(&config.model_client_dir)
            .await
            .map(Arc::from);

        if let Some(ref name) = fallback_model_client_name {
            println!("[model client] fallback plugin: {name}");
        } else {
            println!(
                "[model client] no plugins found in {} — session init will fail unless \
                 a plugin name is provided",
                config.model_client_dir.display()
            );
        }

        let tool_linker = Arc::new(build_tool_linker(&engine)?);

        Ok(Self {
            config,
            engine: Arc::new(engine),
            model_client_linker,
            model_client_cache: Arc::new(Mutex::new(HashMap::new())),
            fallback_model_client_name,
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
        let model_client_cache = self.model_client_cache.clone();
        let fallback_model_client_name = self.fallback_model_client_name.clone();
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

            let (model_config_json, model_allowed_hosts, mut session_params, model_client_name) =
                extract_model_params(&session_cfg, fallback_model_client_name.as_deref());

            println!(
                "[session] config: model={}, max_tokens={}, allowed_paths={:?}, allowed_hosts={:?}",
                session_params.model,
                session_params.max_tokens,
                session_cfg.allowed_paths,
                session_cfg.allowed_hosts,
            );
            println!(
                "[session] model client: plugin={model_client_name}, allowed_hosts={:?}",
                model_allowed_hosts,
            );

            // --- Phase 2: get (or compile) model client, then instantiate ---

            let model_client_component = match get_or_compile_model_client(
                &engine,
                &config.model_client_dir,
                &model_client_cache,
                &model_client_name,
            )
            .await
            {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx
                        .send(Err(Status::internal(format!(
                            "Failed to load model client plugin '{model_client_name}': {e}"
                        ))))
                        .await;
                    return;
                }
            };

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
                &session_cfg.plugin_configs,
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
                        let (new_config_json, new_allowed_hosts, new_params, new_plugin_name) =
                            extract_model_params(&cfg, fallback_model_client_name.as_deref());
                        session_params = new_params;
                        println!("[session] config updated: plugin={new_plugin_name}");

                        let new_component = match get_or_compile_model_client(
                            &engine,
                            &config.model_client_dir,
                            &model_client_cache,
                            &new_plugin_name,
                        )
                        .await
                        {
                            Ok(c) => c,
                            Err(e) => {
                                let _ = tx
                                    .send(Ok(AgentEvent {
                                        event: Some(Event::AgentError(AgentError {
                                            message: format!("Config update failed: {e}"),
                                        })),
                                    }))
                                    .await;
                                continue;
                            }
                        };

                        match instantiate_model_client(
                            &engine,
                            &model_client_linker,
                            &new_component,
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
