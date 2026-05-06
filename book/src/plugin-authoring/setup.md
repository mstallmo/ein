# Project Setup

Plugins are standalone Rust library crates compiled to `wasm32-wasip2`. You don't need to clone or modify the Ein repository.

## Prerequisites

You need Rust stable and the WASM target:

```bash
rustup target add wasm32-wasip2
```

Verify it's installed:

```bash
rustup target list --installed | grep wasm32
# wasm32-wasip2
```

## Create a new crate

```bash
cargo new --lib my_tool
cd my_tool
```

## Configure `Cargo.toml`

Two things are required: the crate must produce a `cdylib` (the WASM component format), and it must depend on `ein_plugin`.

`ein_plugin` is not published to crates.io — add it as a git dependency:

```toml
[package]
name = "my_tool"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
ein_plugin  = { git = "https://github.com/mstallmo/ein" }
anyhow      = "1"
serde       = { version = "1", features = ["derive"] }
serde_json  = "1"
wit-bindgen = "0.53"
```

The `wit-bindgen` version should match what Ein uses. Check the [releases page](https://github.com/mstallmo/ein/releases) or the workspace `Cargo.toml` if you're unsure.

## Locking to a specific Ein release

By default the git dependency tracks the `main` branch. To pin to a release tag:

```toml
ein_plugin = { git = "https://github.com/mstallmo/ein", tag = "v0.1.0" }
```

Or a specific commit:

```toml
ein_plugin = { git = "https://github.com/mstallmo/ein", rev = "abc1234" }
```

## Verify the setup

Try a build before writing any plugin code:

```bash
cargo build --target wasm32-wasip2
```

If this succeeds (producing `target/wasm32-wasip2/debug/my_tool.wasm`), your environment is configured correctly.

> **Tip**: `cargo check --target wasm32-wasip2` is faster for iterating — it type-checks without producing a binary.

## Next steps

- [Writing a Tool Plugin](tool-plugins.md) — implement `ToolPlugin` and register your tool with the LLM
- [Writing a Model Client Plugin](model-client-plugins.md) — implement `ModelClientPlugin` to add a new LLM provider
