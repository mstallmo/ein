// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

mod bindings;
mod syscalls;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::anyhow;
use ein_agent::{
    SessionParams, async_trait,
    model_clients::{CompletionRequest, CompletionResponse, Message, ModelClient, ToolDef},
};
use tokio::sync::OnceCell;
use wasmtime::{Engine, Store, component::*};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::{
    HttpResult, WasiHttpCtx,
    bindings::http::types::ErrorCode,
    body::HyperOutgoingBody,
    types::{HostFutureIncomingResponse, OutgoingRequestConfig, default_send_request},
};

use crate::ModelClientSpec;
use bindings::{ModelClient as ModelClientPlugin, ModelClientPre};

/// Shared factory for creating per-session WASM model client instances.
///
/// Compiled plugins are cached by name in a [`ModelClientCache`] so that
/// repeated session creation for the same plugin incurs only the cheap
/// per-session instantiation cost, not a full WASM compilation.
#[derive(Clone)]
pub struct ModelClientSessionManager {
    engine: Engine,
    linker: Arc<Linker<ModelClientState>>,
    cache: ModelClientCache,
    fallback_name: Arc<str>,
}

impl ModelClientSessionManager {
    /// Creates a new manager, building the model client linker and scanning
    /// `model_client_dir` to determine the fallback plugin name.
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

    /// Instantiates a model client for a new session from the given spec.
    ///
    /// Selects the plugin named by `spec.client_name`, falling back to the first
    /// available plugin if the name is empty. Compiles and links the plugin on
    /// first use; subsequent calls for the same plugin name reuse the cached
    /// compiled component.
    pub async fn new_session(&self, spec: &ModelClientSpec) -> anyhow::Result<ModelClientSession> {
        let model_client_name = resolve_client_name(spec, &self.fallback_name);

        let (params_json, allowed_hosts, session_params) =
            extract_model_params(spec, model_client_name);

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
            "[session] params: model={}, max_tokens={}",
            session_params.model, session_params.max_tokens,
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

/// Resolves which model client plugin to instantiate: the spec's `client_name`
/// if present and non-empty, otherwise the scanned `fallback`.
fn resolve_client_name<'a>(spec: &'a ModelClientSpec, fallback: &'a str) -> &'a str {
    spec.client_name
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or(fallback)
}

/// Extracts model client parameters from a [`ModelClientSpec`].
///
/// Looks up the selected plugin's `params_json` (defaulting to `"{}"`), parses
/// the fields needed for session setup, and returns the raw `params_json` to
/// pass directly to the WASM plugin constructor.
fn extract_model_params(
    spec: &ModelClientSpec,
    model_client_name: &str,
) -> (String, Vec<String>, SessionParams) {
    let params_json = spec
        .plugin_params
        .get(model_client_name)
        .cloned()
        .unwrap_or_else(|| "{}".to_string());

    let config: serde_json::Value = serde_json::from_str(&params_json).unwrap_or_default();

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

    let allowed_hosts = derive_allowed_hosts(base_url);

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

/// A ready-to-use model client for a single session.
///
/// Pairs the session's LLM parameters (model name, max tokens) with the
/// instantiated WASM plugin that makes the actual HTTP calls.
pub struct ModelClientSession {
    params: SessionParams,
    client: WasmModelClient,
}

#[async_trait]
impl ModelClient for ModelClientSession {
    async fn complete(
        &mut self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> anyhow::Result<CompletionResponse> {
        let req = CompletionRequest {
            model: self.params.model.clone(),
            messages: messages.to_vec(),
            tools: tools.to_vec(),
            max_tokens: self.params.max_tokens,
        };

        self.client.complete(&req).await
    }

    async fn cleanup(mut self) {
        if let Err(e) = self.client.cleanup().await {
            eprintln!("[model_client] cleanup error: {e}");
        }
    }
}

/// Shared state threaded through each Wasmtime `Store` for model client plugins.
///
/// Includes `WasiHttpCtx` so that the plugin's `wasi:http/outgoing-handler`
/// import (used by `ein_http` via `wstd`) is satisfied by the host linker.
///
/// `allowed_hosts` is a set of hostnames the plugin is permitted to connect to.
/// Requests to any other host are rejected with `ErrorCode::HttpRequestDenied`.
struct ModelClientState {
    pub resource_table: ResourceTable,
    pub wasi_ctx: WasiCtx,
    pub http_ctx: WasiHttpCtx,
    pub allowed_hosts: HashSet<String>,
}

impl WasiView for ModelClientState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.resource_table,
        }
    }
}

impl wasmtime_wasi_http::WasiHttpView for ModelClientState {
    fn ctx(&mut self) -> &mut WasiHttpCtx {
        &mut self.http_ctx
    }

    fn table(&mut self) -> &mut wasmtime::component::ResourceTable {
        &mut self.resource_table
    }

    fn send_request(
        &mut self,
        request: hyper::Request<HyperOutgoingBody>,
        config: OutgoingRequestConfig,
    ) -> HttpResult<HostFutureIncomingResponse> {
        let host = request_host(&request);

        if !host_allowed(&self.allowed_hosts, &host) {
            eprintln!(
                "[model client] blocked request to '{host}' — not in allowlist {:?}. \
                 Set 'base_url' in ~/.ein/config.json to allow this host.",
                self.allowed_hosts
            );
            return Err(ErrorCode::HttpRequestDenied.into());
        }

        Ok(default_send_request(request, config))
    }
}

/// Extracts the target host from an outbound request.
///
/// The WASI HTTP request model stores authority separately from the path, so
/// `wasmtime_wasi_http` may not embed the host in the hyper URI. Falls back to
/// the `Host` header (port stripped) when the URI carries no host component, and
/// returns an empty string when neither does.
fn request_host<B>(request: &hyper::Request<B>) -> String {
    request
        .uri()
        .host()
        .map(|h| h.to_string())
        .or_else(|| {
            request
                .headers()
                .get("host")
                .and_then(|v| v.to_str().ok())
                .and_then(|h| h.split(':').next())
                .map(|h| h.to_string())
        })
        .unwrap_or_default()
}

/// Returns whether `host` is permitted by the session's outbound allowlist.
///
/// A `"*"` entry allows any host; otherwise the host must be listed exactly.
fn host_allowed(allowed_hosts: &HashSet<String>, host: &str) -> bool {
    allowed_hosts.contains("*") || allowed_hosts.contains(host)
}

/// A live model client WASM plugin instance with its Wasmtime store.
///
/// Each session owns one `WasmModelClient`. The store holds the plugin's WASI
/// context including the outbound HTTP allowlist enforced by
/// [`ModelClientState::send_request`].
pub struct WasmModelClient {
    store: Store<ModelClientState>,
    bindings: ModelClientPlugin,
    handle: ResourceAny,
}

impl WasmModelClient {
    async fn load(
        engine: &Engine,
        instance_pre: &ModelClientPre<ModelClientState>,
        params_json: &str,
        allowed_hosts: HashSet<String>,
    ) -> anyhow::Result<Self> {
        let wasi = WasiCtxBuilder::new().inherit_stdio().build();

        let mut store = Store::new(
            engine,
            ModelClientState {
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
        eprintln!("[model_client] RAW request: {request_json}");

        let result = self
            .bindings
            .model_client()
            .model_client()
            .call_complete(&mut self.store, self.handle, &request_json)
            .await?
            .map_err(|e| {
                eprintln!("[model_client] RAW error: {e}");

                anyhow::anyhow!(e)
            })?;

        eprintln!("[model_client] raw response: {result}");

        Ok(serde_json::from_str(&result)?)
    }
}

/// Builds the Wasmtime linker for model client plugins — called once at server startup.
///
/// Registers WASI p2, WASI HTTP, and the `ein:model-client/host` interface.
fn build_model_client_linker(engine: &Engine) -> anyhow::Result<Linker<ModelClientState>> {
    let mut linker: Linker<ModelClientState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::add_only_http_to_linker_async(&mut linker)?;
    ModelClientPlugin::add_to_linker::<ModelClientState, HasSelf<ModelClientState>>(
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
    instance_pre: &ModelClientPre<ModelClientState>,
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
        linker: &Linker<ModelClientState>,
        client_name: &str,
    ) -> anyhow::Result<ModelClientPre<ModelClientState>> {
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
    cache: Mutex<HashMap<String, OnceCell<ModelClientPre<ModelClientState>>>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(client_name: Option<&str>, params: &[(&str, &str)]) -> ModelClientSpec {
        ModelClientSpec {
            client_name: client_name.map(str::to_string),
            plugin_params: params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    // --- resolve_client_name ---

    #[test]
    fn resolve_client_name_prefers_spec_name() {
        let s = spec(Some("ein_anthropic"), &[]);
        assert_eq!(resolve_client_name(&s, "ein_openrouter"), "ein_anthropic");
    }

    #[test]
    fn resolve_client_name_falls_back_when_absent() {
        let s = spec(None, &[]);
        assert_eq!(resolve_client_name(&s, "ein_openrouter"), "ein_openrouter");
    }

    #[test]
    fn resolve_client_name_falls_back_when_empty() {
        let s = spec(Some(""), &[]);
        assert_eq!(resolve_client_name(&s, "ein_openrouter"), "ein_openrouter");
    }

    // --- derive_allowed_hosts ---

    #[test]
    fn derive_allowed_hosts_empty_denies_all() {
        assert!(derive_allowed_hosts("").is_empty());
    }

    #[test]
    fn derive_allowed_hosts_wildcard_allows_all() {
        assert_eq!(derive_allowed_hosts("*"), vec!["*".to_string()]);
    }

    #[test]
    fn derive_allowed_hosts_extracts_https_host() {
        assert_eq!(
            derive_allowed_hosts("https://openrouter.ai/api/v1"),
            vec!["openrouter.ai".to_string()]
        );
    }

    #[test]
    fn derive_allowed_hosts_strips_port_and_scheme() {
        assert_eq!(
            derive_allowed_hosts("http://localhost:11434"),
            vec!["localhost".to_string()]
        );
    }

    #[test]
    fn derive_allowed_hosts_host_only_no_path() {
        assert_eq!(
            derive_allowed_hosts("https://api.anthropic.com"),
            vec!["api.anthropic.com".to_string()]
        );
    }

    // --- extract_model_params ---

    #[test]
    fn extract_model_params_defaults_when_plugin_absent() {
        let s = spec(None, &[]);
        let (params_json, allowed_hosts, params) = extract_model_params(&s, "ein_openrouter");

        assert_eq!(params_json, "{}");
        assert!(
            allowed_hosts.is_empty(),
            "missing base_url must deny all outbound"
        );
        assert_eq!(params.model, "anthropic/claude-haiku-4.5");
        assert_eq!(params.max_tokens, 2500);
    }

    #[test]
    fn extract_model_params_parses_full_config() {
        let json = r#"{"api_key":"sk-or-x","base_url":"https://openrouter.ai/api/v1","model":"anthropic/claude-opus-4","max_tokens":4096}"#;
        let s = spec(Some("ein_openrouter"), &[("ein_openrouter", json)]);
        let (params_json, allowed_hosts, params) = extract_model_params(&s, "ein_openrouter");

        // The raw params JSON is forwarded verbatim to the plugin constructor.
        assert_eq!(params_json, json);
        assert_eq!(allowed_hosts, vec!["openrouter.ai".to_string()]);
        assert_eq!(params.model, "anthropic/claude-opus-4");
        assert_eq!(params.max_tokens, 4096);
    }

    #[test]
    fn extract_model_params_empty_model_falls_back_to_default() {
        let json = r#"{"base_url":"*","model":""}"#;
        let s = spec(Some("ein_ollama"), &[("ein_ollama", json)]);
        let (_, allowed_hosts, params) = extract_model_params(&s, "ein_ollama");

        assert_eq!(params.model, "anthropic/claude-haiku-4.5");
        assert_eq!(allowed_hosts, vec!["*".to_string()]);
    }

    #[test]
    fn extract_model_params_ignores_other_plugins_config() {
        // Params are looked up by the resolved client name, not the whole map.
        let s = spec(
            Some("ein_openrouter"),
            &[(
                "ein_anthropic",
                r#"{"base_url":"https://api.anthropic.com"}"#,
            )],
        );
        let (params_json, allowed_hosts, _) = extract_model_params(&s, "ein_openrouter");

        assert_eq!(params_json, "{}");
        assert!(allowed_hosts.is_empty());
    }

    // --- host_allowed ---

    fn host_set(hosts: &[&str]) -> HashSet<String> {
        hosts.iter().map(|h| h.to_string()).collect()
    }

    #[test]
    fn host_allowed_exact_match() {
        let hosts = host_set(&["openrouter.ai"]);
        assert!(host_allowed(&hosts, "openrouter.ai"));
        assert!(!host_allowed(&hosts, "evil.example.com"));
    }

    #[test]
    fn host_allowed_wildcard_allows_any() {
        let hosts = host_set(&["*"]);
        assert!(host_allowed(&hosts, "anything.example.com"));
        assert!(host_allowed(&hosts, ""));
    }

    #[test]
    fn host_allowed_empty_set_denies_all() {
        let hosts = host_set(&[]);
        assert!(!host_allowed(&hosts, "openrouter.ai"));
    }

    // --- request_host ---

    #[test]
    fn request_host_reads_uri_host() {
        let req = hyper::Request::builder()
            .uri("https://openrouter.ai/api/v1/chat/completions")
            .body(())
            .unwrap();
        assert_eq!(request_host(&req), "openrouter.ai");
    }

    #[test]
    fn request_host_falls_back_to_host_header() {
        // Authority-form absent from the path-only URI; host lives in the header.
        let req = hyper::Request::builder()
            .uri("/api/v1/chat/completions")
            .header("host", "api.anthropic.com:443")
            .body(())
            .unwrap();
        assert_eq!(request_host(&req), "api.anthropic.com");
    }

    #[test]
    fn request_host_empty_when_no_host_available() {
        let req = hyper::Request::builder()
            .uri("/api/v1/chat/completions")
            .body(())
            .unwrap();
        assert_eq!(request_host(&req), "");
    }
}
