# Introduction

Ein is a self-hosted, plugin-driven AI agent for your terminal. It runs a gRPC server (`eind`) that drives an LLM agent loop and executes tools, while a terminal UI client (`ein`) provides an interactive chat interface.

```
┌─────────────────────────────┐          ┌──────────────────────────────┐
│          ein                │  gRPC    │          eind                │
│                             │◄────────►│                              │
│  Terminal UI (Ratatui)      │          │  Agent loop + tool executor  │
│  Interactive chat           │          │  WASM plugin sandbox         │
│  Session picker             │          │  LLM model client plugins    │
│                             │          │  SQLite session persistence  │
└─────────────────────────────┘          └──────────────────────────────┘
```

**Key properties:**

- **Self-hosted** — `eind` runs on your machine. Your conversations stay local.
- **Plugin-driven** — tools (Bash, Read, Write, Edit) and model client adapters (OpenRouter, Anthropic, OpenAI, Ollama) are compiled WASM modules loaded at runtime. You can write your own.
- **Sandboxed** — WASM plugins run inside Wasmtime. Filesystem access and network connections require explicit allowlists you control.
- **Persistent sessions** — conversations are stored in SQLite and can be resumed across restarts.

## This Book

This book is for two audiences:

**Ein users** — people who want to run Ein and use it day-to-day. Start with [Installation](installation.md), then [Getting Started](getting-started.md).

**Plugin authors** — users who want to extend Ein by writing custom tool or model client plugins in Rust. After getting comfortable as a user, see the [Plugin Authoring](plugin-authoring/README.md) section.
