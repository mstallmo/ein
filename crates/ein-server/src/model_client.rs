use async_trait::async_trait;
use ein_model_client::{CompletionRequest, CompletionResponse};
use std::path::Path;
use tokio::sync::Mutex;
use wasmtime::{Engine, Store, component::*};
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi_http::WasiHttpCtx;

use crate::model_client_bindings::ModelClient;
use crate::ModelClientHarnessState;

/// Trait implemented by both the real WASM model client and any test doubles.
#[async_trait]
pub trait CompletionProvider: Send + Sync {
    async fn complete(&self, req: &CompletionRequest) -> anyhow::Result<CompletionResponse>;
}

pub struct WasmModelClient {
    inner: Mutex<WasmModelClientInner>,
}

struct WasmModelClientInner {
    store: Store<ModelClientHarnessState>,
    bindings: ModelClient,
    handle: ResourceAny,
}

impl WasmModelClient {
    async fn load<P: AsRef<Path>>(
        engine: &Engine,
        linker: &Linker<ModelClientHarnessState>,
        path: P,
        config_json: &str,
    ) -> anyhow::Result<Self> {
        let wasi = WasiCtxBuilder::new().inherit_stdio().build();

        let mut store = Store::new(
            engine,
            ModelClientHarnessState {
                resource_table: ResourceTable::new(),
                wasi_ctx: wasi,
                http_ctx: WasiHttpCtx::new(),
                http_client: reqwest::Client::new(),
            },
        );

        let component = Component::from_file(engine, path)?;
        let bindings = ModelClient::instantiate_async(&mut store, &component, linker).await?;

        let accessor = bindings.model_client().model_client();
        let handle = accessor.call_constructor(&mut store, config_json).await?;

        Ok(Self {
            inner: Mutex::new(WasmModelClientInner {
                store,
                bindings,
                handle,
            }),
        })
    }
}

#[async_trait]
impl CompletionProvider for WasmModelClient {
    async fn complete(&self, req: &CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let request_json = serde_json::to_string(req)?;
        let mut inner = self.inner.lock().await;
        let WasmModelClientInner {
            store,
            bindings,
            handle,
        } = &mut *inner;

        let result = bindings
            .model_client()
            .model_client()
            .call_complete(store, *handle, &request_json)
            .await?
            .map_err(|e| anyhow::anyhow!(e))?;

        Ok(serde_json::from_str(&result)?)
    }
}

/// Scans `model_client_dir` for the first `.wasm` file, builds a dedicated
/// linker with WASI + WASI HTTP + `ein:model-client/host`, instantiates it,
/// and returns a ready-to-use [`WasmModelClient`].
pub async fn load_model_client(
    engine: &Engine,
    model_client_dir: &Path,
    config_json: &str,
) -> anyhow::Result<WasmModelClient> {
    let mut linker: Linker<ModelClientHarnessState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::add_only_http_to_linker_async(&mut linker)?;
    ModelClient::add_to_linker::<ModelClientHarnessState, HasSelf<ModelClientHarnessState>>(
        &mut linker,
        |state| state,
    )?;

    let mut entries = tokio::fs::read_dir(model_client_dir).await?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.path().extension().and_then(|e| e.to_str()) == Some("wasm") {
            println!(
                "[model client] loading plugin from {}",
                entry.path().display()
            );
            return WasmModelClient::load(engine, &linker, entry.path(), config_json).await;
        }
    }

    anyhow::bail!(
        "no model client plugin found in {}",
        model_client_dir.display()
    )
}
