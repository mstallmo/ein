// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

//! Runtime integration tests that instantiate real `wasm32-wasip2` components.
//!
//! These require the plugins to be built first:
//!
//! ```bash
//! cargo build --target wasm32-wasip2 \
//!     -p ein_bash -p ein_read -p ein_write -p ein_edit -p ein_openrouter
//! ```
//!
//! Because that artifact is not present in a plain `cargo test` run, every test
//! here is `#[ignore]`d by default. Run them explicitly with:
//!
//! ```bash
//! cargo test -p ein_wasm --test plugin_loading -- --ignored
//! ```

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use ein_wasm::{ModelClientSpec, PluginConstraints, PluginRuntime, ToolSessionSpec};

/// The workspace `target/wasm32-wasip2/debug` directory, where debug plugin
/// builds land.
fn debug_wasm_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/wasm32-wasip2/debug")
        .canonicalize()
        .expect("debug wasm dir must exist — build the plugins for wasm32-wasip2 first")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Copies only the tool components into an isolated, uniquely-named dir so
/// `new_tool_set` does not try to instantiate the model-client components (which
/// live in the same debug dir) as tools, and so parallel tests don't collide.
fn isolated_tool_dir(debug: &Path, label: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("ein_wasm_it_tools_{}_{label}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    for name in ["ein_bash", "ein_read", "ein_write", "ein_edit"] {
        fs::copy(
            debug.join(format!("{name}.wasm")),
            dir.join(format!("{name}.wasm")),
        )
        .expect("tool plugin must be built");
    }
    dir
}

fn openrouter_params(json: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    m.insert("ein_openrouter".to_string(), json.to_string());
    m
}

#[tokio::test]
#[ignore = "requires prebuilt wasm32-wasip2 plugins"]
async fn new_tool_set_loads_all_tool_plugins() {
    let debug = debug_wasm_dir();
    let tool_dir = isolated_tool_dir(&debug, "load_all");

    let runtime = PluginRuntime::new(&tool_dir, &debug)
        .await
        .expect("PluginRuntime::new should build both managers");

    let spec = ToolSessionSpec {
        global: PluginConstraints {
            allowed_paths: vec![repo_root().display().to_string()],
            allowed_hosts: vec![],
        },
        overrides: HashMap::new(),
    };

    let tool_set = runtime
        .tools()
        .new_tool_set(&spec)
        .await
        .expect("new_tool_set should load the tool components");

    assert_eq!(
        tool_set.schemas().len(),
        4,
        "expected Bash/Read/Write/Edit to load"
    );

    let _ = fs::remove_dir_all(&tool_dir);
}

#[tokio::test]
#[ignore = "requires prebuilt wasm32-wasip2 plugins"]
async fn new_session_instantiates_model_client() {
    let debug = debug_wasm_dir();
    // `new_tool_set` is never called here, so the debug dir doubles as the tool
    // dir — `PluginRuntime::new` only builds the tool linker, it never scans it.
    let runtime = PluginRuntime::new(&debug, &debug).await.unwrap();

    let spec = ModelClientSpec {
        client_name: Some("ein_openrouter".to_string()),
        plugin_params: openrouter_params(
            r#"{"api_key":"sk-or-test","base_url":"https://openrouter.ai/api/v1","model":"anthropic/claude-haiku-4.5","max_tokens":16}"#,
        ),
    };

    // Compiles, links, and constructs the component — no network call is made
    // until `complete` is invoked.
    runtime
        .model_clients()
        .new_session(&spec)
        .await
        .expect("new_session should instantiate the model client component");
}

#[tokio::test]
#[ignore = "requires prebuilt wasm32-wasip2 plugins"]
async fn new_session_rejects_missing_base_url() {
    let debug = debug_wasm_dir();
    let runtime = PluginRuntime::new(&debug, &debug).await.unwrap();

    // No `base_url` → empty outbound allowlist → the session must be refused
    // before the component is ever compiled.
    let spec = ModelClientSpec {
        client_name: Some("ein_openrouter".to_string()),
        plugin_params: openrouter_params(r#"{"api_key":"sk-or-test"}"#),
    };

    let err = match runtime.model_clients().new_session(&spec).await {
        Ok(_) => panic!("a model client without a valid host must be rejected"),
        Err(e) => e,
    };

    assert!(
        err.to_string().contains("No valid host configured"),
        "unexpected error: {err}"
    );
}
