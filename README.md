# Ein

Ein is a Rust-based AI agent framework. A gRPC server drives an LLM agent loop and executes tools implemented as pluggable WASM modules. Multiple model backends are supported (OpenRouter, Anthropic, OpenAI, Ollama). A terminal UI client connects to the server and provides an interactive chat interface. Sessions are persisted to SQLite so conversations can be resumed across reconnects.

```
┌─────────────────────────────┐          ┌──────────────────────────────┐
│          ein-tui            │  gRPC    │          ein-server          │
│                             │ (proto)  │                              │
│  Ratatui terminal UI        │◄────────►│  Agent loop + tool executor  │
│  Session picker on startup  │          │  WASM plugin host            │
│  Slash command autocomplete │          │  Pluggable model clients     │
│  Animated thinking spinner  │          │  Per-session sandboxing      │
│  Syntax-highlighted diffs   │          │  SQLite session persistence  │
└─────────────────────────────┘          └──────────────────────────────┘
```

## Getting Started

### Prerequisites

- API credentials for your chosen model backend (e.g. [OpenRouter](https://openrouter.ai/settings/keys), Anthropic, OpenAI) or a running [Ollama](https://ollama.com) instance
- Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Setup

**Add the WASM compile target**
```bash
rustup target add wasm32-wasip2
```

**Build the WASM plugins** (tool plugins and model client plugins)
```bash
./scripts/build_install_plugins.sh
```

> In debug builds, plugins are loaded automatically from `./target/wasm32-wasip2/debug/` — no installation needed. In release builds, tool plugins are installed to `~/.ein/plugins/tools/` and model client plugins to `~/.ein/plugins/model_clients/`.

### Configure credentials

Add your model backend credentials to `~/.ein/config.json` (created automatically on first TUI launch). Examples for each supported backend:

**OpenRouter**
```json
{
  "plugin_configs": {
    "ein_openrouter": {
      "config": {
        "api_key": "sk-or-...",
        "base_url": "https://openrouter.ai/api/v1"
      }
    }
  }
}
```

**Anthropic**
```json
{
  "plugin_configs": {
    "ein_anthropic": {
      "config": {
        "api_key": "sk-ant-..."
      }
    }
  }
}
```

**OpenAI**
```json
{
  "plugin_configs": {
    "ein_openai": {
      "config": {
        "api_key": "sk-..."
      }
    }
  }
}
```

**Ollama** (no API key required)
```json
{
  "plugin_configs": {
    "ein_ollama": {
      "config": {
        "base_url": "http://localhost:11434",
        "model": "llama3"
      }
    }
  }
}
```

### Running

Start the server in one terminal:

```bash
cargo run -p ein-server
```

Start the TUI client in another:

```bash
cargo run -p ein-tui
```

The TUI connects to `localhost:50051` by default. To connect to a different address:

```bash
cargo run -p ein-tui -- http://my-server:50051
```

To enable debug logging to `~/.ein/tui.log`:

```bash
cargo run -p ein-tui -- --debug
```

On first connection a **session picker** modal appears. Use `↑`/`↓` to navigate, `Enter` to select:
- **New Session** — starts a fresh conversation; a follow-up modal asks whether to grant the agent access to your current working directory for that session
- **Existing session** — resumes a prior conversation from where it left off
- **Shift+D** on an existing session — permanently deletes it from the server

## Configuration

The TUI stores its configuration at `~/.ein/config.json`. The file is created with defaults on first run.

```json
{
  "allowed_paths": [],
  "allowed_hosts": [],
  "plugin_configs": {
    "ein_openrouter": {
      "config": {
        "api_key": "sk-or-...",
        "base_url": "https://openrouter.ai/api/v1",
        "model": "anthropic/claude-haiku-4.5",
        "max_tokens": "2500"
      }
    }
  }
}
```

### Global fields

| Field | Description |
|-------|-------------|
| `allowed_paths` | Filesystem paths all WASM plugins may read/write (preopened for every session) |
| `allowed_hosts` | Hostnames all WASM plugins may connect to (empty = deny all; `"*"` = allow all) |

When starting a new session, the TUI asks whether to add the current working directory to `allowed_paths` for that session only — this is never written back to `config.json`.

### Per-plugin configuration (`plugin_configs`)

`plugin_configs` is a map keyed by plugin filename stem (e.g. `"ein_openrouter"`, `"ein_bash"`). Each entry can contain:

| Field | Description |
|-------|-------------|
| `allowed_paths` | Additional filesystem paths for this plugin only, merged with the global list |
| `allowed_hosts` | Additional hostnames for this plugin only, merged with the global list |
| `config` | Arbitrary key-value pairs forwarded to the plugin at instantiation |

Known `config` keys per plugin:

| Plugin | Key | Description |
|--------|-----|-------------|
| `ein_openrouter` | `api_key` | OpenRouter API key |
| `ein_openrouter` | `base_url` | API endpoint; restricts outbound connections to that host |
| `ein_openrouter` | `model` | OpenRouter model ID (e.g. `anthropic/claude-haiku-4.5`) |
| `ein_openrouter` | `max_tokens` | Maximum tokens per LLM response |
| `ein_anthropic` | `api_key` | Anthropic API key |
| `ein_anthropic` | `model` | Anthropic model ID (e.g. `claude-haiku-4-5`) |
| `ein_anthropic` | `max_tokens` | Maximum tokens per LLM response |
| `ein_openai` | `api_key` | OpenAI API key |
| `ein_openai` | `base_url` | API endpoint (defaults to OpenAI; set for compatible providers) |
| `ein_openai` | `model` | Model ID (e.g. `gpt-4o`) |
| `ein_openai` | `max_tokens` | Maximum tokens per LLM response |
| `ein_ollama` | `base_url` | Ollama server URL (e.g. `http://localhost:11434`) |
| `ein_ollama` | `model` | Model name (e.g. `llama3`) |
| `ein_ollama` | `max_tokens` | Maximum tokens per LLM response |

Changes to `config.json` are picked up automatically while the TUI is running — plugin config updates take effect without restarting.

## Usage

Type a message and press **Enter** to send it to the agent. Type `/` to see available slash commands with autocomplete hints.

| Key / Command | Action |
|---------------|--------|
| `Enter` | Send message / run slash command |
| `↑` / `↓` | Scroll conversation history (also navigate session picker) |
| `Ctrl-C` | Force quit |
| `/exit` | Exit the TUI |
| `/config` | Open `~/.ein/config.json` in `$EDITOR` |
| `/clear` | Wipe the agent's in-memory context (SQLite history preserved; clears display) |
| `/new` | Drop current session and start a fresh one |
| `/sessions` | Re-open the session picker to switch sessions |
| `/compact` | Summarise the conversation via the LLM and replace history with the summary |

While the agent is working, an animated thinking spinner appears in the conversation pane. Tool invocations are shown inline as the agent uses them:

```
 ▸ Bash  ls -la
 ▸ Read  src/main.rs
 ▸ Write  src/main.rs
 ▸ Edit  src/main.rs
```

`Edit` calls display a syntax-highlighted diff showing the removed and added lines (up to 5 each), with line numbers.

The status bar at the bottom shows the active model and cumulative token usage on the left, and the current session UUID on the right.

### Connection behaviour

The TUI connects in the background immediately on startup. The session picker modal is shown as part of the first successful connection handshake. While disconnected, a red `●` icon and animated spinner appear in the conversation pane. The TUI reconnects automatically every 3 seconds. If the server goes away mid-session, an error message is shown and the TUI resumes connecting in the background — the session picker reappears on the next successful reconnect. Running `/new` or `/sessions` bypasses the 3-second retry delay and triggers an immediate reconnect.

## Tools

Tools are WASM components loaded at startup. Four are included out of the box:

| Tool | Description |
|------|-------------|
| `Bash` | Execute shell commands (streams stdout in real time) |
| `Read` | Read a file from the filesystem |
| `Write` | Write content to a file |
| `Edit` | Replace a specific string in a file with new content |

### Adding a tool

1. Create a new package under `packages/` implementing the `ToolPlugin` trait from `packages/ein_tool/`
2. Add it to `scripts/build_install_plugins.sh`
3. Rebuild — the server picks it up automatically on next start

## Architecture

```
crates/
  ein-proto/    Protocol Buffer definitions (gRPC service + message types)
  ein-server/   gRPC server — agent loop, WASM plugin host, session persistence
  ein-tui/      Terminal UI client
packages/
  ein_tool/         WASM tool plugin interface (ToolPlugin trait, ToolDef, syscalls)
  ein_bash/         Bash tool plugin
  ein_read/         Read tool plugin
  ein_write/        Write tool plugin
  ein_edit/         Edit tool plugin
  ein_model_client/ Shared model client types and WIT bindings
  ein_http/         WASM-native HTTP client (used by model client plugins)
  ein_openrouter/   OpenRouter model client plugin
  ein_anthropic/    Anthropic model client plugin
  ein_openai/       OpenAI model client plugin
  ein_ollama/       Ollama model client plugin
```

### Protocol

The protocol (`crates/ein-proto/proto/ein.proto`) defines a bidirectional streaming RPC (`AgentSession`), a unary `ListSessions` RPC, and a unary `DeleteSession` RPC. Each session opens with a `SessionConfig` message (global sandbox constraints + per-plugin config map + optional `session_id` for resume), followed by `UserInput` prompt messages. The server streams back `AgentEvent` messages as the agent thinks, calls tools, and produces output — starting with a `SessionStarted` event carrying the session's UUID, a `resumed` boolean, and the prior conversation history when resuming.

`UserInput` variants after `init`:
- `prompt` — a user message driving `run_agent`
- `config_update` — push new plugin credentials to the live session without reconnecting
- `clear_context` — wipe the server's in-memory message history (SQLite history preserved)
- `compact_context` — summarise the conversation via the LLM; replaces both in-memory and persisted history with the summary

`ListSessions` returns a list of `SessionSummary` records (newest-first), each containing the session ID, creation timestamp, a preview of the first user message, and the stored `SessionConfig` JSON needed to reconstruct the session on resume. `DeleteSession` permanently removes a session and its message history from the store.

### Session persistence

Sessions are persisted to `~/.ein/sessions.db`. Supplying a previously assigned `session_id` in `SessionConfig` causes the server to restore the full conversation history and resume as if the session never disconnected.

### TUI modules (`crates/ein-tui/src/`)

| File | Role |
|------|------|
| `main.rs` | Entry point, CLI args, event loop, terminal lifecycle |
| `app.rs` | `App` state struct, `DisplayMessage` variants, session picker / CWD modal state |
| `config.rs` | `ClientConfig` — load, save, and migrate `~/.ein/config.json` |
| `connection.rs` | `connection_manager` — background reconnect loop, `ListSessions` handshake, `DeleteSession`, config file watcher |
| `input.rs` | Slash command registry (`COMMANDS`), key event handler, server event handler |
| `render.rs` | Full render pass — conversation pane, input area, autocomplete, session picker and CWD modals, status bar |

Uses **Ratatui** (v0.29) for rendering and **crossterm** for keyboard events. The conversation pane renders a corgi pixel-art header on startup. Edit diffs are syntax-highlighted using `syntect` with the `base16-ocean.dark` theme.

### Server modules (`crates/ein-server/src/`)

| File | Role |
|------|------|
| `main.rs` | CLI arg parsing, `EinConfig`, `HarnessState`, `ModelClientHarnessState` (HTTP host filtering), server startup |
| `grpc.rs` | `AgentServer` — tonic `Agent` impl; `AgentSession` and `ListSessions` handlers; session persistence; `ConfigUpdate` mid-session |
| `agent.rs` | `run_agent` — the LLM ↔ tool loop |
| `model_client.rs` | `ModelClientSessionManager`, WASM model client compilation and instantiation |
| `persistence.rs` | `SessionStore` — SQLite-backed session storage; create, save, and load message history |
| `tools.rs` | `ToolRegistry` + `WasmTool` — loads and calls WASM plugins |
| `syscalls.rs` | Host functions exposed to WASM tool plugins (spawn, log, …) |

## License

Ein is licensed under the [Apache License, Version 2.0](LICENSE).

Copyright 2026 Mason Stallmo.

By submitting a pull request, you agree that your contribution is licensed under the Apache License, Version 2.0.
