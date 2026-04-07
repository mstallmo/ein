// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use ein_plugin::model_client::{CompletionRequest, CompletionResponse, Message};
use serde_json::Value;
use tokio::sync::OnceCell;
use wasmtime::{Engine, Store, component::*};
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi_http::WasiHttpCtx;

use crate::ModelClientHarnessState;
use crate::agent::SessionParams;
use crate::model_client_bindings::{ModelClient, ModelClientPre};

#[derive(Clone)]
pub struct ModelClientSessionManager {
    engine: Engine,
    linker: Arc<Linker<ModelClientHarnessState>>,
    cache: ModelClientCache,
    fallback_name: Arc<str>,
}

impl ModelClientSessionManager {
    pub async fn new<P: AsRef<Path>>(model_client_dir: P, engine: Engine) -> anyhow::Result<Self> {
        let model_client_dir = model_client_dir.as_ref();
        let linker = Arc::new(build_model_client_linker(&engine)?);
        let cache = ModelClientCache::new(model_client_dir);
        let fallback_name = scan_model_client_name(model_client_dir)
            .await
            .map(Arc::from)
            .ok_or(anyhow!(
                "No model client found in {}",
                model_client_dir.display()
            ))?;

        Ok(Self {
            engine,
            linker,
            cache,
            fallback_name,
        })
    }

    pub async fn new_session(
        &self,
        session_cfg: &ein_proto::ein::SessionConfig,
    ) -> anyhow::Result<ModelClientSession> {
        let model_client_name = if session_cfg.model_client_name.is_empty() {
            self.fallback_name.deref()
        } else {
            &session_cfg.model_client_name
        };

        let (params_json, allowed_hosts, session_params) =
            extract_model_params(session_cfg, model_client_name);

        if allowed_hosts.is_empty() {
            return Err(anyhow!(
                "No valid host configured for the model client.\nUpdate ~/.ein/config.json and try again",
            ));
        }

        let instance_pre = self
            .cache
            .get_or_prepare(&self.engine, &self.linker, model_client_name)
            .await
            .map_err(|err| {
                anyhow!("Failed to load model client plugin '{model_client_name}': {err}")
            })?;

        let client =
            instantiate_model_client(&self.engine, &instance_pre, &params_json, &allowed_hosts)
                .await
                .map_err(|err| anyhow!("Failed to instantiate model client: {err}"))?;

        println!(
            "[session] params: model={}, max_tokens={}, allowed_paths={:?}, allowed_hosts={:?}",
            session_params.model,
            session_params.max_tokens,
            session_cfg.allowed_paths,
            session_cfg.allowed_hosts,
        );
        println!(
            "[session] model client: plugin={model_client_name}, allowed_hosts={:?}",
            allowed_hosts,
        );

        Ok(ModelClientSession {
            params: session_params,
            client,
        })
    }
}

/// Extracts model client parameters from a `SessionConfig`.
///
/// Parses `config_json` from the plugin's `PluginConfig` entry, extracts the fields
/// needed for session setup, and returns the raw `config_json` to pass directly to
/// the WASM plugin constructor.
fn extract_model_params(
    session_cfg: &ein_proto::ein::SessionConfig,
    model_client_name: &str,
) -> (String, Vec<String>, SessionParams) {
    let pc = session_cfg.plugin_configs.get(model_client_name);

    let config: serde_json::Value = pc
        .map(|p| serde_json::from_str(&p.params_json).unwrap_or_default())
        .unwrap_or_default();

    let base_url = config["base_url"].as_str().unwrap_or_default();
    let model = config["model"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("anthropic/claude-haiku-4.5")
        .to_string();
    let max_tokens = config["max_tokens"]
        .as_i64()
        .map(|n| n as i32)
        .unwrap_or(2500);

    if pc.map(|p| !p.allowed_hosts.is_empty()).unwrap_or(false) {
        eprintln!(
            "[model_client] The `allowed_hosts` config option for model clients is ignored. \
             Only the `base_url` is used to derive the allowed host."
        );
    }

    let allowed_hosts = derive_allowed_hosts(base_url);
    let params_json = pc
        .map(|p| p.params_json.clone())
        .unwrap_or_else(|| "{}".to_string());

    (
        params_json,
        allowed_hosts,
        SessionParams { model, max_tokens },
    )
}

/// Derives the outbound host allowlist from a `base_url`.
///
/// - Empty → deny all (`[]`).
/// - `"*"` → allow all.
/// - Any real URL → extract and allowlist only the hostname.
fn derive_allowed_hosts(base_url: &str) -> Vec<String> {
    if base_url.is_empty() {
        vec![]
    } else if base_url == "*" {
        vec!["*".to_string()]
    } else {
        base_url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .and_then(|authority| authority.split(':').next())
            .map(|host| vec![host.to_string()])
            .unwrap_or_default()
    }
}

pub struct ModelClientSession {
    params: SessionParams,
    client: WasmModelClient,
}

impl ModelClientSession {
    pub fn params(&self) -> &SessionParams {
        &self.params
    }

    pub async fn complete(
        &mut self,
        messages: &[Message],
        tools: &[Value],
    ) -> anyhow::Result<CompletionResponse> {
        let req = CompletionRequest {
            model: self.params.model.clone(),
            messages: messages.to_vec(),
            tools: tools.to_vec(),
            max_tokens: self.params.max_tokens,
        };

        self.client.complete(&req).await
    }

    pub async fn cleanup(self) -> anyhow::Result<()> {
        self.client.cleanup().await
    }
}

pub struct WasmModelClient {
    store: Store<ModelClientHarnessState>,
    bindings: ModelClient,
    handle: ResourceAny,
}

impl WasmModelClient {
    async fn load(
        engine: &Engine,
        instance_pre: &ModelClientPre<ModelClientHarnessState>,
        params_json: &str,
        allowed_hosts: HashSet<String>,
    ) -> anyhow::Result<Self> {
        let wasi = WasiCtxBuilder::new().inherit_stdio().build();

        let mut store = Store::new(
            engine,
            ModelClientHarnessState {
                resource_table: ResourceTable::new(),
                wasi_ctx: wasi,
                http_ctx: WasiHttpCtx::new(),
                allowed_hosts,
            },
        );

        let bindings = instance_pre.instantiate_async(&mut store).await?;

        let accessor = bindings.model_client().model_client();
        let handle = accessor.call_constructor(&mut store, params_json).await?;

        Ok(Self {
            store,
            bindings,
            handle,
        })
    }

    pub async fn cleanup(mut self) -> anyhow::Result<()> {
        self.handle.resource_drop_async(&mut self.store).await?;

        Ok(())
    }

    pub async fn complete(
        &mut self,
        req: &CompletionRequest,
    ) -> anyhow::Result<CompletionResponse> {
        let request_json = serde_json::to_string(req)?;

        let result = self
            .bindings
            .model_client()
            .model_client()
            .call_complete(&mut self.store, self.handle, &request_json)
            .await?
            .map_err(|e| anyhow::anyhow!(e))?;

        eprintln!("[model_client] raw response: {result}");

        Ok(serde_json::from_str(&result)?)
    }
}

/// Builds the Wasmtime linker for model client plugins — called once at server startup.
///
/// Registers WASI p2, WASI HTTP, and the `ein:model-client/host` interface.
fn build_model_client_linker(engine: &Engine) -> anyhow::Result<Linker<ModelClientHarnessState>> {
    let mut linker: Linker<ModelClientHarnessState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::add_only_http_to_linker_async(&mut linker)?;
    ModelClient::add_to_linker::<ModelClientHarnessState, HasSelf<ModelClientHarnessState>>(
        &mut linker,
        |state| state,
    )?;

    Ok(linker)
}

/// Scans `model_client_dir` for the first `.wasm` file and returns its filename
/// stem (e.g. `ein_openrouter.wasm` → `"ein_openrouter"`). Does not compile.
/// Used at startup to determine the fallback plugin name.
async fn scan_model_client_name(model_client_dir: &Path) -> Option<String> {
    let mut entries = tokio::fs::read_dir(model_client_dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("wasm") {
            if let Some(file_name) = path.file_name() {
                println!(
                    "[model_client] Selected {} as fallback model client",
                    file_name.display()
                );
            }

            return path.file_stem().and_then(|s| s.to_str()).map(str::to_owned);
        }
    }
    None
}

/// Compiles a single model client plugin by name from `model_client_dir`.
/// Returns the compiled [`Component`].
async fn compile_model_client_component(
    engine: &Engine,
    model_client_dir: &Path,
    name: &str,
) -> anyhow::Result<Component> {
    let path = model_client_dir.join(format!("{name}.wasm"));
    if !path.exists() {
        anyhow::bail!(
            "Model client plugin '{name}' not found.\n\n\
             In debug builds, run `cargo build --target wasm32-wasip2 -p {name}` first.\n\
             In release builds, run `./scripts/build_install_plugins.sh`.",
        );
    }
    println!("[model client] compiling plugin '{name}'");
    let engine = engine.clone();

    // Component compilation is CPU-bound, run in blocking thread pool
    Ok(tokio::task::spawn_blocking(move || Component::from_file(&engine, &path)).await??)
}

/// Instantiates a model client for a single session — called per session.
///
/// `allowed_hosts` lists the hostnames the plugin may connect to via
/// `wasi:http/outgoing-handler`. Pass `["*"]` to allow all hosts (used when
/// `base_url` is absent and the plugin chooses its own endpoint).
async fn instantiate_model_client(
    engine: &Engine,
    instance_pre: &ModelClientPre<ModelClientHarnessState>,
    params_json: &str,
    allowed_hosts: &[String],
) -> anyhow::Result<WasmModelClient> {
    let allowed_hosts: HashSet<String> = if allowed_hosts.iter().any(|h| h == "*") {
        std::iter::once("*".to_string()).collect()
    } else {
        allowed_hosts.iter().cloned().collect()
    };

    WasmModelClient::load(engine, instance_pre, params_json, allowed_hosts).await
}

#[derive(Clone)]
struct ModelClientCache(Arc<ModelClientCacheInner>);

impl ModelClientCache {
    pub fn new<P: AsRef<Path>>(model_client_dir: P) -> Self {
        let inner = ModelClientCacheInner {
            model_client_dir: model_client_dir.as_ref().to_owned(),
            cache: Mutex::new(HashMap::new()),
        };

        Self(Arc::new(inner))
    }

    /// Returns a pre-instantiated [`ModelClientPre`] for `name`, compiling and
    /// linking it from disk on first use and caching it for subsequent sessions.
    pub async fn get_or_prepare(
        &self,
        engine: &Engine,
        linker: &Linker<ModelClientHarnessState>,
        client_name: &str,
    ) -> anyhow::Result<ModelClientPre<ModelClientHarnessState>> {
        // Get entry for the client name, inserting an empty OnceCell if no entry exits yet.
        let cell = {
            let mut lock = self.0.cache.lock().expect("model cache lock poisoned");
            lock.entry(client_name.to_string()).or_default().clone()
        };

        // Get the `ModelClientPre` from the OnceCell or initialize
        cell.get_or_try_init(|| async {
            let component =
                compile_model_client_component(engine, &self.0.model_client_dir, client_name)
                    .await?;
            ModelClientPre::new(linker.instantiate_pre(&component)?).map_err(anyhow::Error::from)
        })
        .await
        .cloned()
    }
}

struct ModelClientCacheInner {
    model_client_dir: PathBuf,
    cache: Mutex<HashMap<String, OnceCell<ModelClientPre<ModelClientHarnessState>>>>,
}
