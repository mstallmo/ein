# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Ein

Ein is a Rust-based AI agent framework with a client-server architecture. A gRPC server drives an LLM agent loop and executes tools implemented as pluggable WASM modules. Multiple model client plugins are supported (OpenRouter, Anthropic, OpenAI, Ollama). A terminal UI client (`ein-tui`) connects to the server and provides an interactive chat interface. Sessions are persisted to SQLite so conversations can be resumed across reconnects.

## Setup

```bash
rustup target add wasm32-wasip2
cargo build                          # Build all crates
cargo build -p ein-tui               # Build just the TUI client
cargo build -p ein-server            # Build just the server
```

Plugins (tool plugins and model client plugins) are WASM components compiled separately:

```bash
./scripts/build_install_plugins.sh   # Compiles and installs plugins to ~/.ein/plugins/
```

In debug builds plugins are loaded from `./target/wasm32-wasip2/debug/` automatically — no installation needed. In release builds, tool plugins are loaded from `~/.ein/plugins/tools/` and model client plugins from `~/.ein/plugins/model_clients/`.

Credentials are configured in `~/.ein/config.json` (created on first TUI launch). Add `api_key` and `base_url` under `plugin_configs["ein_openrouter"].config` before running:

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

## Running

```bash
# Terminal 1 — start the server (no env vars needed)
cargo run -p ein-server

# Terminal 2 — start the TUI (connects to localhost:50051 by default)
cargo run -p ein-tui

# Optional: connect to a non-default server address
cargo run -p ein-tui -- http://my-server:50051
```

The server creates `~/.ein/sessions.db` on first run to persist session history.

## Architecture

### Client-server split

```
┌─────────────────────────────┐          ┌──────────────────────────────┐
│          ein-tui            │  gRPC    │          ein-server          │
│                             │ (proto)  │                              │
│  Ratatui terminal UI        │◄────────►│  Agent loop + tool executor  │
│  Keyboard / render loop     │          │  WASM plugin host            │
│  Slash command autocomplete │          │  Pluggable LLM model clients │
│                             │          │  SQLite session persistence  │
└─────────────────────────────┘          └──────────────────────────────┘
```

The protocol is defined in `crates/ein-proto/proto/ein.proto`. The client streams `UserInput` messages; the server streams back `AgentEvent` messages (`SessionStarted`, `ContentDelta`, `ToolCallStart`, `ToolCallEnd`, `ToolOutputChunk`, `AgentFinished`, `AgentError`, `TokenUsage`).

### Session lifecycle

Each connection goes through several message types (all variants of `UserInput`):

1. **Init** — the client sends a `SessionConfig` as the first message (the `init` variant). The server creates or resumes a persisted session, instantiates the model client, and loads tool plugins before starting the prompt loop.
2. **Prompts** — subsequent messages carry the `prompt` string variant and drive `run_agent`.
3. **Config update** — a `config_update` message (same shape as `SessionConfig`) may arrive at any time after init. The server re-instantiates the model client with the new credentials mid-session without resetting conversation history. Sent automatically by the TUI when `~/.ein/config.json` changes on disk.
4. **Clear context** — a `clear_context` boolean message wipes the server's in-memory message history for this session without touching SQLite. Sent by the TUI's `/clear` command.
5. **Compact context** — a `compact_context` boolean message asks the server to summarise the conversation via the LLM and replace both the in-memory history and the persisted SQLite record with the summary. The server streams the summary back as `ContentDelta` events followed by `AgentFinished`. Sent by the TUI's `/compact` command.

`SessionConfig` carries:
- `allowed_paths` — filesystem paths preopened for all WASM plugins via `WasiCtxBuilder::preopened_dir`
- `allowed_hosts` — hostnames all WASM plugins may connect to (empty = deny all; `"*"` = allow all)
- `plugin_configs` — map of plugin filename stem → `PluginConfig`; each entry has its own `allowed_paths`/`allowed_hosts` (merged with the global lists) and a `config` JSON object for plugin-specific parameters
- `model_client_name` — plugin filename stem of the model client to use (e.g. `"ein_openrouter"`, `"ein_ollama"`); if empty the server uses the first available plugin
- `session_id` — UUID v7 identifying the session; if empty the server generates a new one; if set and a matching session exists in the store, its message history is restored

The server emits a `SessionStarted` event immediately after setup (before any agent events) containing the assigned `session_id` and a `resumed` boolean.

Known `plugin_configs` keys and their `config` entries:
- `"ein_openrouter"` — `api_key`, `base_url` (empty = deny all outbound; `"*"` = allow all; real URL = restrict to that host), `model`, `max_tokens`
- `"ein_anthropic"` — `api_key`, `model`, `max_tokens`
- `"ein_openai"` — `api_key`, `base_url`, `model`, `max_tokens`
- `"ein_ollama"` — `base_url` (e.g. `"http://localhost:11434"`), `model`, `max_tokens`

### Session persistence

Sessions are persisted to a SQLite database at `~/.ein/sessions.db` (opened on server startup via `src/persistence.rs`). `SessionStore` provides:

- **Create** — new sessions are assigned a UUID v7 and written to the `sessions` table on first prompt.
- **Save** — after each agent turn, the full message history is serialised and written (`save_messages`).
- **Resume** — when a client supplies a known `session_id`, `load_messages` restores the conversation so the agent picks up where it left off.

Database migrations live in `crates/ein-server/migrations/`.

### Client config (`crates/ein-tui/src/config.rs`)

`ClientConfig` is loaded from (or created at) `~/.ein/config.json` on TUI startup. Structure mirrors `SessionConfig`. At startup the TUI shows a floating modal asking whether to add the current working directory to `allowed_paths` for that session; this is never persisted to `config.json`.

The TUI watches `~/.ein/config.json` for changes using `notify` (platform-native: FSEvents/inotify/ReadDirectoryChangesW). When the file changes, the new config is read and a `config_update` message is sent to the server if a session is live, or used on the next reconnect if not. `allowed_paths` and `allowed_hosts` are session-scoped (set at init) and are not updated mid-session by config changes.

Legacy flat config files (with top-level `api_key`, `base_url`, `model`, `max_tokens`) are automatically migrated to the nested format on load.

### Server (`crates/ein-server/`)

| File | Role |
|------|------|
| `src/main.rs` | CLI arg parsing, `EinConfig`, `HarnessState`, `ModelClientHarnessState` (incl. HTTP host filtering), server startup |
| `src/grpc.rs` | `AgentServer` — tonic `Agent` impl, spawns per-session tasks; handles session persistence, `ConfigUpdate` mid-session |
| `src/agent.rs` | `run_agent` — the LLM ↔ tool loop |
| `src/model_client.rs` | `ModelClientSessionManager`, WASM model client compilation and instantiation |
| `src/persistence.rs` | `SessionStore` — SQLite-backed session storage; create, save, and load message history |
| `src/tools.rs` | `ToolRegistry` + `WasmTool` — loads and calls WASM plugins |
| `src/syscalls.rs` | Host functions exposed to WASM tool plugins (spawn, log, …) |

**Agent loop** (`src/agent.rs`): sends the message history to the LLM, streams `ContentDelta` events for text output, executes each requested `ToolCall` via the registry, appends results to history, and loops until `FinishReason::Stop`. Transport errors from the model client (e.g. `HttpRequestDenied`, network failures) and API-level errors (e.g. 402 insufficient credits) both emit `AgentError` events and return `Ok(())` — the session is preserved and the user can retry after fixing their config. Cumulative token usage is sent as `TokenUsage` events after each LLM call.

**Plugin loading** (`src/tools.rs`): scans the plugin directory for `.wasm` files and instantiates each as a Wasmtime component. The filename stem (e.g. `ein_bash`) is used as the plugin's config identity to look up its entry in `plugin_configs`; global `allowed_paths`/`allowed_hosts` are merged with any plugin-specific overrides before the WASI context is built. After instantiation, `name()`/`schema()` are called to get the display name (e.g. `"Bash"`) and tool schema exposed to the model. In debug mode both tool and model client plugins are loaded from `./target/wasm32-wasip2/debug/`; in release mode tool plugins come from `~/.ein/plugins/tools/` and model client plugins from `~/.ein/plugins/model_clients/`.

### TUI (`crates/ein-tui/`)

Six source files under `crates/ein-tui/src/`:

| File | Role |
|------|------|
| `main.rs` | Entry point, CLI args (`--debug`), event loop, terminal lifecycle, `KeyAction` dispatch |
| `app.rs` | `App` state struct, `DisplayMessage` variants, `AppEvent`, `SessionPickerState`, `CwdState` |
| `config.rs` | `ClientConfig` — load, save, and migrate `~/.ein/config.json` |
| `connection.rs` | `connection_manager` — background reconnect loop, `ListSessions` handshake, `DeleteSession`, config file watcher |
| `input.rs` | Slash command registry (`COMMANDS`), key event handler, server event handler |
| `render.rs` | Full render pass — conversation pane, input area, autocomplete, session picker and CWD modals, status bar |

Uses **Ratatui** (v0.29) for rendering and **crossterm** for keyboard events.

**Layout** (top → bottom):
1. **Conversation pane** — scrollable message history; streams agent output in real time
2. **Input area** — single/multi-line text field with character-level wrapping; dark-peach border
3. **Autocomplete section** — always 3 lines tall; shows slash-command hints when input starts with `/`
4. **Status bar** — model name (vendor prefix stripped) and cumulative token usage; shows model name only while connecting

**Color palette** — all colors are named constants at the top of `render.rs`:
- `INPUT_BORDER_COLOR` — muted dark-peach/terracotta border on the input area
- `TOOL_NAME_COLOR` — steel blue for the `▸ ToolName` tool call indicator
- `THINKING_COLOR` — soft sky blue for the animated thinking spinner
- `MUTED_COLOR` — dark grey for secondary text (args, autocomplete descriptions, connecting animation)
- `AUTOCOMPLETE_TOP_COLOR` — muted white for the top autocomplete match
- `DISCONNECTED_COLOR` — muted red for the disconnected `●` icon and error messages

**Thinking animation**: a braille spinner (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`) appears in the conversation pane while the agent is busy, driven by an 80 ms ticker.

**Connecting animation**: when disconnected, a red `●` icon + grey braille spinner + italic "connecting to server" text appears in the conversation pane. If a previous session dropped with an error, the error message is shown above the spinner (replaced in-place, never appended).

**Session picker**: shown immediately on first connection. Use `↑`/`↓` to navigate, `Enter` to select. Row 0 is always "New Session"; subsequent rows are existing sessions (newest-first). Press `Shift+D` on an existing session to delete it (sends `DeleteSession` RPC; removes the row immediately on success).

**CWD modal**: shown after choosing "New Session" in the picker. A centered floating window asks whether to allow access to the current working directory. Press `Y` to add it to `allowed_paths` for the session; `N`, `Enter`, or `Esc` to skip.

**Connection management** (`connection_manager` / `try_connect`): a background Tokio task retries the gRPC connection every 3 seconds. A `reconnect_notify` (`Arc<Notify>`) lets `/new` and `/sessions` bypass the delay. State transitions are communicated to the main loop via `AppEvent` (an mpsc channel). `AppEvent::Connected` carries the outbound `mpsc::Sender<UserInput>`; `AppEvent::Disconnected` carries an optional error string; `AppEvent::ConfigChanged` carries a freshly parsed `ClientConfig` from the file watcher. A `session_config_cache` (`Arc<Mutex<Option<SessionConfig>>>`) stores the chosen config so reconnects reuse it without reshowing the picker.

**Tool call display**: `▸ ToolName  primary_arg` — for `Bash` the command is shown; for `Read`/`Write`/`Edit` the file path is shown. `Edit` additionally renders a syntax-highlighted diff (up to `DIFF_MAX_LINES` = 5 lines each of removed/added content) using `syntect` with the `base16-ocean.dark` theme.

**Slash commands**: defined in the `COMMANDS` constant in `input.rs`. Adding a command requires appending a `CommandDef` entry there.

| Command | Description |
|---------|-------------|
| `/exit` | Exit Ein (works regardless of connection state) |
| `/config` | Open `~/.ein/config.json` in `$EDITOR` |
| `/clear` | Wipe the in-memory context for this session (sends `clear_context`; SQLite history is kept; clears local display) |
| `/new` | Drop the current session and start a fresh one (shows CWD modal, then reconnects) |
| `/sessions` | Re-open the session picker to switch sessions |
| `/compact` | Summarise the conversation via the LLM, replace context and SQLite history with the summary (sends `compact_context`) |

**Scrolling**: `↑`/`↓` arrows scroll the conversation. `scroll_offset` counts lines up from the bottom; auto-scroll re-engages when the view reaches the bottom again.

**Ctrl-C**: always force-quits, even while the agent is busy.

### WASM plugin interface (`packages/`)

**Tool plugins** implement the `ToolPlugin` trait from `packages/ein_tool/` and declare their name, description, and JSON parameter schema via `ToolDef`. They are compiled to `wasm32-wasip2`.

| Package | Tool name | Description |
|---------|-----------|-------------|
| `ein_bash` | `Bash` | Executes shell commands via the `spawn` syscall |
| `ein_read` | `Read` | Reads a file from the filesystem |
| `ein_write` | `Write` | Writes content to a file |
| `ein_edit` | `Edit` | Replaces an exact string in a file with new content; returns `metadata` with `start_line`, `old_lines`, and `new_lines` for the TUI diff view |

To add a new tool, create a package under `packages/` implementing `ToolPlugin`, add it to `build_install_plugins.sh`, and rebuild.

**Model client plugins** implement the `ModelClient` WIT interface (`wit/model_client/`). The server compiles each plugin once at startup via `ModelClientSessionManager` and instantiates it per session with the session's credentials. The active plugin is selected by `model_client_name` in `SessionConfig`; if omitted the first available plugin is used.

| Package | Description |
|---------|-------------|
| `ein_openrouter` | OpenRouter chat completions client |
| `ein_anthropic` | Direct Anthropic API client |
| `ein_openai` | OpenAI-compatible API client |
| `ein_ollama` | Local Ollama inference client |
| `ein_http` | `wasm32-wasip2`-only HTTP client backed by `wstd` (`wasi:http/outgoing-handler`); reqwest-like builder API |
| `ein_model_client` | Shared types (`CompletionRequest`, `CompletionResponse`) and WIT bindings used by model client plugins |

Outbound HTTP from model client plugins is intercepted by `ModelClientHarnessState::send_request` (in `src/main.rs`), which enforces the per-session hostname allowlist (derived from `base_url` + any extra `allowed_hosts` in the plugin's config entry) before forwarding to `default_send_request`.

The model client plugin has no `name()` WIT method — its config identity is its filename stem (e.g. `"ein_openrouter"`), consistent with tool plugins.
