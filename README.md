# Ein

Ein is a Rust-based AI agent framework. A gRPC server drives an LLM agent loop and executes tools implemented as pluggable WASM modules. Multiple model backends are supported (OpenRouter, Anthropic, OpenAI, Ollama). A terminal UI client connects to the server and provides an interactive chat interface. Sessions are persisted to SQLite so conversations can be resumed across reconnects.

```
┌─────────────────────────┐          ┌──────────────────────────────┐
│        ein-tui          │   gRPC   │          ein-server          │
│                         │◄────────►│                              │
│  Interactive chat UI    │          │  LLM agent loop              │
│  Connection retry       │          │  WASM tool executor          │
│  Slash command hints    │          │  Pluggable model clients     │
│  Animated thinking UI   │          │  Per-session sandboxing      │
│                         │          │  SQLite session persistence  │
└─────────────────────────┘          └──────────────────────────────┘
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

On first launch a floating dialog asks whether to grant the agent access to your current working directory for that session. The TUI connects to `localhost:50051` by default. To connect to a different address:

```bash
cargo run -p ein-tui -- http://my-server:50051
```

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

In addition, at startup the TUI asks whether to add the current working directory to `allowed_paths` for that session only — this is never written back to `config.json`.

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

Type a message and press **Enter** to send it to the agent. Type `/` to see available slash commands.

| Key | Action |
|-----|--------|
| `Enter` | Send message / run slash command |
| `↑` / `↓` | Scroll conversation history |
| `Ctrl-C` | Force quit |
| `/exit` + `Enter` | Exit the TUI |

While the agent is working, an animated indicator appears in the chat panel. Tool invocations are shown inline as the agent uses them:

```
 ▸ Bash  ls -la
 ▸ Read  src/main.rs
 ▸ Write  src/main.rs
 ▸ Edit  src/main.rs
```

`Edit` calls display a syntax-highlighted diff showing the removed and added lines.

The status bar at the bottom shows the active model and cumulative token usage for the session.

### Connection behaviour

The TUI starts immediately regardless of whether the server is running. While disconnected, a red `●` icon and animated spinner appear in the conversation pane. The TUI reconnects automatically every 3 seconds. If the server goes away mid-session, an error message is shown and the TUI resumes connecting in the background.

## Tools

Tools are WASM components loaded at startup. Three are included out of the box:

| Tool | Description |
|------|-------------|
| `Bash` | Execute shell commands |
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
  ein_plugin/       WASM plugin interface (ToolPlugin trait, ToolDef, syscalls)
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

The protocol (`crates/ein-proto/proto/ein.proto`) defines a bidirectional streaming RPC. Each session opens with a `SessionConfig` message (global sandbox constraints + per-plugin config map + optional `session_id` for resume), followed by `UserInput` prompt messages. The server streams back `AgentEvent` messages as the agent thinks, calls tools, and produces output — starting with a `SessionStarted` event carrying the session's UUID and whether it was resumed. A `config_update` message variant allows the TUI to push plugin config changes to a live session without reconnecting.

Sessions are persisted to `~/.ein/sessions.db`. Supplying a previously assigned `session_id` in `SessionConfig` causes the server to restore the conversation history and resume as if the session never disconnected.

## License

Ein is licensed under the [Apache License, Version 2.0](LICENSE).

Copyright 2026 Mason Stallmo.

By submitting a pull request, you agree that your contribution is licensed under the Apache License, Version 2.0.
