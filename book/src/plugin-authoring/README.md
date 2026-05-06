# Plugin Authoring

Ein's tools and model adapters are **WASM components** — self-contained binaries compiled to `wasm32-wasip2` and loaded by `eind` at startup. Writing a plugin means writing a Rust crate that implements one of Ein's plugin traits, compiling it to WASM, and dropping the `.wasm` file in the right directory.

This section covers writing plugins for your own personal use. You don't need to be in the Ein source tree — plugins are standalone Rust projects.

## Two plugin types

### Tool plugins

Tool plugins expose capabilities to the LLM. When the agent decides to use a tool, the server calls the plugin's `call` method with JSON-encoded arguments and collects the result, which is fed back into the conversation.

Ein ships with four tool plugins: `Bash`, `Read`, `Write`, and `Edit`. You might write a tool plugin to fetch data from an API, run a custom script, query a database, or do anything else that could be useful in the agent loop.

Tool plugins live in `~/.ein/plugins/tools/`.

### Model client plugins

Model client plugins translate between Ein's internal `CompletionRequest` format and a specific LLM API (Anthropic, OpenAI, etc.). The server calls `complete` with a serialized request and expects a serialized response back.

You would write a model client plugin to add support for a new API provider, proxy through a custom middleware, or log/transform requests for debugging.

Model client plugins live in `~/.ein/plugins/model_clients/`.

## Plugin identity

A plugin's identity is its **filename stem**. The file `~/.ein/plugins/tools/my_weather.wasm` becomes the `"my_weather"` plugin. This is how the server looks up per-plugin config in `plugin_configs`.

The tool's display name shown to the LLM and in the UI is returned by the `name()` method — it can differ from the filename.

## Loading behavior

- **Debug builds** of `eind` (i.e., `cargo run --bin eind`): plugins are loaded from `./target/wasm32-wasip2/debug/`. Useful for iterating inside the Ein workspace.
- **Release builds**: tool plugins from `~/.ein/plugins/tools/`, model clients from `~/.ein/plugins/model_clients/`.

For standalone plugins, you'll always install to `~/.ein/plugins/`.

## Sections

- [Project Setup](setup.md) — creating a new Rust project with the right dependencies
- [Writing a Tool Plugin](tool-plugins.md) — implementing `ToolPlugin` with a worked example
- [Writing a Model Client Plugin](model-client-plugins.md) — implementing `ModelClientPlugin` with a worked example
- [Building & Installing](building-installing.md) — compiling to WASM and installing
