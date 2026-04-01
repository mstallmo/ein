// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use ein_plugin::model_client::{CompletionRequest, CompletionResponse};
use std::collections::HashSet;
use std::path::Path;
use wasmtime::{Engine, Store, component::*};
use wasmtime_wasi::WasiCtxBuilder;
use wasmtime_wasi_http::WasiHttpCtx;

use crate::ModelClientHarnessState;
use crate::model_client_bindings::ModelClient;

pub struct WasmModelClient {
    store: Store<ModelClientHarnessState>,
    bindings: ModelClient,
    handle: ResourceAny,
}

impl WasmModelClient {
    async fn load(
        engine: &Engine,
        linker: &Linker<ModelClientHarnessState>,
        component: &Component,
        config_json: &str,
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

        let bindings = ModelClient::instantiate_async(&mut store, component, linker).await?;

        let accessor = bindings.model_client().model_client();
        let handle = accessor.call_constructor(&mut store, config_json).await?;

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

        Ok(serde_json::from_str(&result)?)
    }
}

/// Builds the Wasmtime linker for model client plugins — called once at server startup.
///
/// Registers WASI p2, WASI HTTP, and the `ein:model-client/host` interface.
pub fn build_model_client_linker(
    engine: &Engine,
) -> anyhow::Result<Linker<ModelClientHarnessState>> {
    let mut linker: Linker<ModelClientHarnessState> = Linker::new(engine);
    wasmtime_wasi::p2::add_to_linker_async(&mut linker)?;
    wasmtime_wasi_http::add_only_http_to_linker_async(&mut linker)?;
    ModelClient::add_to_linker::<ModelClientHarnessState, HasSelf<ModelClientHarnessState>>(
        &mut linker,
        |state| state,
    )?;

    Ok(linker)
}

/// Scans `model_client_dir` for the first `.wasm` file, compiles it into a
/// [`Component`], and returns both the component and the plugin name derived
/// from the filename stem (e.g. `ein_openrouter.wasm` → `"ein_openrouter"`).
/// Called once at server startup.
pub async fn load_model_client_component(
    engine: &Engine,
    model_client_dir: &Path,
) -> anyhow::Result<(Component, String)> {
    let mut entries = tokio::fs::read_dir(model_client_dir).await?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("wasm") {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            println!("[model client] loading plugin from {}", path.display());
            return Ok((Component::from_file(engine, path)?, name));
        }
    }

    anyhow::bail!(
        "no model client plugin found in {}",
        model_client_dir.display()
    )
}

/// Instantiates a model client for a single session — called per session.
///
/// `allowed_hosts` lists the hostnames the plugin may connect to via
/// `wasi:http/outgoing-handler`. Pass `["*"]` to allow all hosts (used when
/// `base_url` is absent and the plugin chooses its own endpoint).
pub async fn instantiate_model_client(
    engine: &Engine,
    linker: &Linker<ModelClientHarnessState>,
    component: &Component,
    config_json: &str,
    allowed_hosts: &[String],
) -> anyhow::Result<WasmModelClient> {
    let allowed_hosts: HashSet<String> = if allowed_hosts.iter().any(|h| h == "*") {
        std::iter::once("*".to_string()).collect()
    } else {
        allowed_hosts.iter().cloned().collect()
    };

    WasmModelClient::load(engine, linker, component, config_json, allowed_hosts).await
}
