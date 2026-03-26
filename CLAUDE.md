# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Ein

Ein is a Rust-based AI agent framework. It takes a natural language `--prompt`, communicates with Claude via the OpenRouter API, and executes tools (Bash, Read, Write) in a loop until the task is complete. Tools are implemented as pluggable WASM modules loaded at runtime from `~/.ein/plugins/`.

## Setup

```bash
rustup target add wasm32-wasip2
./scripts/build_install_plugins.sh   # Compiles and installs WASM plugins to ~/.ein/plugins/
```

Requires `OPENROUTER_API_KEY` environment variable.

## Commands

```bash
cargo build --release
cargo run --release -- --prompt "<task>"
OPENROUTER_API_KEY=<key> cargo run --release -- --prompt "Create a Python file that prints hello world"
```

There are no tests yet.

## Architecture

**Agent loop** (`src/main.rs`): Initializes the OpenRouter client (Claude Haiku 4.5), loads WASM plugins, builds a tool registry, then iterates: send prompt → receive tool calls → execute tools → send results back → repeat until `FinishReason::Stop`.

**Tool system** (`src/tools/tools.rs`): Dynamically loads `.wasm` files from the plugin directory using Wasmtime (WebAssembly Component Model). Each tool exposes a JSON schema for Claude and a `call` handler. The `Tool` trait is implemented by both native tools (Bash) and WASM-backed tools.

**WASM plugin interface** (`wit/plugin/plugin.wit` + `packages/ein_plugin/`): Plugins implement the `ToolPlugin` trait and use `ToolDef` to declare their name, description, and JSON parameter schema. Compiled to `wasm32-wasip2`.

**Built-in tools**:
- `bash` — native Rust, executes shell commands (`src/tools/bash.rs`)
- `read` / `write` — WASM plugins in `packages/ein_read/` and `packages/ein_write/`

To add a new tool, either implement the `Tool` trait in Rust and register it in `main.rs`, or create a new package under `packages/` that implements the WASM plugin interface and add it to `build_install_plugins.sh`.
