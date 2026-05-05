<img src="https://tangled.org/mstallmo.com/ein/raw/main/assets/corgi.svg" width="128" alt="Ein corgi logo" />

# Ein

An AI agent for your terminal — self-hosted, plugin-driven, and yours.


```
┌─────────────────────────────┐          ┌──────────────────────────────┐
│          ein                │  gRPC    │          eind                │
│                             │ (proto)  │                              │
│  Ratatui terminal UI        │◄────────►│  Agent loop + tool executor  │
│  Session picker on startup  │          │  WASM plugin host            │
│  Slash command autocomplete │          │  Pluggable model clients     │
│  Animated thinking spinner  │          │  Per-session sandboxing      │
│  Syntax-highlighted diffs   │          │  SQLite session persistence  │
└─────────────────────────────┘          └──────────────────────────────┘
```

`ein` is the terminal UI you interact with; `eind` is the agent server that runs as a background service. On first launch, `ein` downloads `eind` automatically and registers it as a system service — no separate server setup required. Tools and model clients are sandboxed WASM plugins: choose from OpenRouter, Anthropic, OpenAI, or Ollama, or write your own.

## Features

- Works with OpenRouter, Anthropic, OpenAI, and Ollama — bring your own key or run locally with no API key
- Sessions persist to SQLite; resume any conversation where you left off
- Tools run as sandboxed WASM components with fine-grained filesystem and network controls per session
- Extend with custom tools or model client backends
- Syntax-highlighted edit diffs shown inline as the agent works
- Live config reload — update credentials without restarting

## Installation

Install with [cargo binstall](https://github.com/cargo-bins/cargo-binstall):

```bash
cargo install cargo-binstall
cargo binstall --git https://github.com/mstallmo/ein ein
```

Or download pre-built archives directly from [GitHub Releases](https://github.com/mstallmo/ein/releases).

## Getting Started

Run `ein`. On first launch:

1. **Server setup** — `eind` is downloaded from GitHub Releases and registered as a background service automatically
2. **Setup wizard** — choose your model provider and enter your API credentials
3. **Session picker** — create a new session or resume an existing one; when starting a new session, a prompt asks whether to grant the agent access to your current working directory for that session

That's it. `ein` manages `eind` — you never need to run the server separately.

## Configuration

The TUI stores configuration at `~/.ein/config.json`, created on first run. You can also open it at any time with `/config`.

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

### Credentials by provider

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

Changes to `config.json` are picked up automatically while the TUI is running.

## Usage

Type a message and press **Enter** to send. Type `/` to see available slash commands with autocomplete hints.

| Key / Command | Action |
|---------------|--------|
| `Enter` | Send message / run slash command |
| `↑` / `↓` | Scroll conversation history |
| `Ctrl-C` | Force quit |
| `/exit` | Exit the TUI |
| `/config` | Open `~/.ein/config.json` in `$EDITOR` |
| `/clear` | Wipe the agent's in-memory context (SQLite history preserved) |
| `/new` | Drop current session and start a fresh one |
| `/sessions` | Re-open the session picker to switch sessions |
| `/compact` | Summarise the conversation via the LLM and replace history with the summary |
| `/plugins` | Manage installed plugins |
| `/setup` | Re-run the first-time setup wizard |
| `/uninstall` | Stop and remove the eind service and binary |

While the agent is working, an animated thinking spinner appears in the conversation pane. Tool invocations are shown inline as the agent uses them:

```
 ▸ Bash  ls -la
 ▸ Read  src/main.rs
 ▸ Write  src/main.rs
 ▸ Edit  src/main.rs
```

`Edit` calls display a syntax-highlighted diff showing removed and added lines (up to 5 each), with line numbers.

The status bar shows the active model and cumulative token usage. Use `Shift+D` on a session in the picker to permanently delete it.

### Connection behaviour

The TUI reconnects automatically every 3 seconds if the server is unavailable. Running `/new` or `/sessions` bypasses the retry delay and triggers an immediate reconnect attempt.

## Tools

Tools are WASM components loaded at startup. Four are included:

| Tool | Description |
|------|-------------|
| `Bash` | Execute shell commands (streams stdout in real time) |
| `Read` | Read a file from the filesystem |
| `Write` | Write content to a file |
| `Edit` | Replace a specific string in a file with new content |

### Adding a tool

1. Create a new package under `plugins/` implementing the `ToolPlugin` trait from `plugins/ein_tool/`
2. Add it to `scripts/build_install_plugins.sh`
3. Rebuild — the server picks it up automatically on next start

## Architecture

```
crates/
  ein_agent/        Agent component (agent loop, tool call logic, extension interface)
  ein_core/         Shared types and utilities between plugins and the harness
  ein_http/         WASM-native HTTP client (used by model client plugins)
  ein_plugin/       WASM plugin interface
  ein_proto/        Protocol Buffer definitions (gRPC service + message types)
ein/                Terminal UI client
eind/               gRPC server — WASM plugin host, session persistence
plugins/
  ein_anthropic/    Anthropic model client plugin
  ein_bash/         Bash tool plugin
  ein_edit/         Edit tool plugin
  ein_ollama/       Ollama model client plugin
  ein_openai/       OpenAI model client plugin
  ein_openrouter/   OpenRouter model client plugin
  ein_read/         Read tool plugin
  ein_write/        Write tool plugin
```

### Protocol

The protocol (`crates/ein_proto/proto/ein.proto`) defines a bidirectional streaming `AgentSession` RPC, a `ListSessions` RPC, and a `DeleteSession` RPC. Each session opens with a `SessionConfig` init message followed by user prompts; the server streams back `AgentEvent` messages starting with a `SessionStarted` event that carries the session UUID and a `resumed` boolean.

### Session persistence

Sessions are persisted to `~/.ein/sessions.db`. Supplying a previously assigned `session_id` in `SessionConfig` restores the full conversation history so the agent picks up exactly where it left off.

## Contributing

Contributions are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md) for how to build from source, run in development, and cut a release.

## License

Ein is licensed under the [Apache License, Version 2.0](LICENSE).

Copyright 2026 Mason Stallmo.

By submitting a pull request, you agree that your contribution is licensed under the Apache License, Version 2.0.
