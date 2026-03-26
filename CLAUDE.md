# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Ein

Ein is a Rust-based AI agent framework with a client-server architecture. A gRPC server drives an LLM agent loop (using Claude via OpenRouter) and executes tools implemented as pluggable WASM modules. A terminal UI client (`ein-tui`) connects to the server and provides an interactive chat interface.

## Setup

```bash
rustup target add wasm32-wasip2
cargo build                          # Build all crates
cargo build -p ein-tui               # Build just the TUI client
cargo build -p ein-server            # Build just the server
```

Plugins (Bash, Read, Write) are WASM components compiled separately:

```bash
./scripts/build_install_plugins.sh   # Compiles and installs plugins to ~/.ein/plugins/
```

In debug builds plugins are loaded from `./target/wasm32-wasip2/debug/` automatically — no installation needed.

Requires `OPENROUTER_API_KEY` environment variable.

## Running

```bash
# Terminal 1 — start the server
OPENROUTER_API_KEY=<key> cargo run -p ein-server

# Terminal 2 — start the TUI (connects to localhost:50051 by default)
cargo run -p ein-tui

# Optional: connect to a non-default server address
cargo run -p ein-tui -- http://my-server:50051
```

There are no tests yet.

## Architecture

### Client-server split

```
┌─────────────────────────────┐          ┌──────────────────────────────┐
│          ein-tui            │  gRPC    │          ein-server          │
│                             │ (proto)  │                              │
│  Ratatui terminal UI        │◄────────►│  Agent loop + tool executor  │
│  Keyboard / render loop     │          │  WASM plugin host            │
│  Slash command autocomplete │          │  OpenRouter LLM client       │
└─────────────────────────────┘          └──────────────────────────────┘
```

The protocol is defined in `crates/ein-proto/proto/ein.proto`. The client streams `UserInput` messages; the server streams back `AgentEvent` messages (`ContentDelta`, `ToolCallStart`, `ToolCallEnd`, `AgentFinished`, `AgentError`, `TokenUsage`).

### Session lifecycle

Each connection goes through two phases:

1. **Init** — the client sends a `SessionConfig` as the first `UserInput` (the `init` variant of the `oneof`). The server applies it before starting the prompt loop.
2. **Prompts** — subsequent `UserInput` messages carry the `prompt` string variant and drive `run_agent`.

`SessionConfig` carries:
- `allowed_paths` — filesystem paths preopened for WASM plugins via `WasiCtxBuilder::preopened_dir`
- `allowed_hosts` — hostnames plugins may connect to; resolved to IPs upfront and enforced via `WasiCtxBuilder::socket_addr_check` (empty = inherit network unrestricted)
- `model` — OpenRouter model ID
- `max_tokens` — token limit per LLM call

### Client config (`crates/ein-tui/src/config.rs`)

`ClientConfig` is loaded from (or created at) `~/.ein/config.json` on TUI startup. Fields mirror `SessionConfig`. At startup the TUI shows a floating modal asking whether to add the current working directory to `allowed_paths` for that session; this is never persisted to `config.json`.

### Server (`crates/ein-server/`)

| File | Role |
|------|------|
| `src/main.rs` | CLI arg parsing, `EinConfig`, `HarnessState`, server startup |
| `src/grpc.rs` | `AgentServer` — tonic `Agent` impl, spawns per-session tasks |
| `src/agent.rs` | `run_agent` — the LLM ↔ tool loop |
| `src/tools.rs` | `ToolRegistry` + `WasmTool` — loads and calls WASM plugins |
| `src/syscalls.rs` | Host functions exposed to WASM plugins (spawn, log, …) |

**Agent loop** (`src/agent.rs`): sends the message history to the LLM, streams `ContentDelta` events for text output, executes each requested `ToolCall` via the registry, appends results to history, and loops until `FinishReason::Stop`. On each iteration it checks for an `{"error": ...}` response from OpenRouter (e.g. 402 insufficient credits) and emits an `AgentError` event rather than panicking. Cumulative token usage is sent as `TokenUsage` events after each LLM call.

**Plugin loading** (`src/tools.rs`): scans the plugin directory for `.wasm` files, instantiates each as a Wasmtime component, and calls `name()`/`schema()` to self-describe. In debug mode this is `./target/wasm32-wasip2/debug/`; in release mode `~/.ein/plugins/`. Each `WasmTool` gets its own `WasiCtx` built from the session's `allowed_paths` and `allowed_hosts`.

### TUI (`crates/ein-tui/`)

Two files: `src/main.rs` (app logic + rendering) and `src/config.rs` (config load/save).

Uses **Ratatui** (v0.29) for rendering and **crossterm** for keyboard events.

**Layout** (top → bottom):
1. **Conversation pane** — scrollable message history; streams agent output in real time
2. **Input area** — single/multi-line text field with character-level wrapping; dark-peach border
3. **Autocomplete section** — always 3 lines tall; shows slash-command hints when input starts with `/`
4. **Status bar** — model name (vendor prefix stripped) and cumulative token usage; shows model name only while connecting

**Color palette** — all colors are named constants at the top of `main.rs`:
- `INPUT_BORDER_COLOR` — muted dark-peach/terracotta border on the input area
- `TOOL_NAME_COLOR` — steel blue for the `▸ ToolName` tool call indicator
- `THINKING_COLOR` — soft sky blue for the animated thinking spinner
- `MUTED_COLOR` — dark grey for secondary text (args, autocomplete descriptions, connecting animation)
- `AUTOCOMPLETE_TOP_COLOR` — muted white for the top autocomplete match
- `DISCONNECTED_COLOR` — muted red for the disconnected `●` icon and error messages

**Thinking animation**: a braille spinner (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`) appears in the conversation pane while the agent is busy, driven by an 80 ms ticker.

**Connecting animation**: when disconnected, a red `●` icon + grey braille spinner + italic "connecting to server" text appears in the conversation pane. If a previous session dropped with an error, the error message is shown above the spinner (replaced in-place, never appended).

**CWD modal**: at startup a centered floating window (`Clear` + bordered `Block`) overlays the TUI asking whether to allow access to the current working directory. Press `Y` to add it to `allowed_paths` for the session; `N`, `Enter`, or `Esc` to skip. The connection manager is spawned only after this modal is dismissed.

**Connection management** (`connection_manager` / `try_connect`): a background Tokio task retries the gRPC connection every 3 seconds. State transitions are communicated to the main loop via `AppEvent` (an mpsc channel). `AppEvent::Connected` carries the outbound `mpsc::Sender<UserInput>`; `AppEvent::Disconnected` carries an optional error string.

**Tool call display**: `▸ ToolName  primary_arg` — for `Bash` the command is shown; for `Read`/`Write` the file path is shown.

**Slash commands**: defined in the `COMMANDS` constant. Currently only `/exit`. Adding a command requires appending a `CommandDef` entry there. `/exit` works regardless of connection state.

**Scrolling**: `↑`/`↓` arrows scroll the conversation. `scroll_offset` counts lines up from the bottom; auto-scroll re-engages when the view reaches the bottom again.

**Ctrl-C**: always force-quits, even while the agent is busy.

### WASM plugin interface (`packages/`)

| Package | Tool name | Description |
|---------|-----------|-------------|
| `ein_bash` | `Bash` | Executes shell commands via the `spawn` syscall |
| `ein_read` | `Read` | Reads a file from the filesystem |
| `ein_write` | `Write` | Writes content to a file |

Plugins implement the `ToolPlugin` trait from `packages/ein_tool/` and declare their name, description, and JSON parameter schema via `ToolDef`. They are compiled to `wasm32-wasip2`.

To add a new tool, create a package under `packages/` implementing `ToolPlugin`, add it to `build_install_plugins.sh`, and rebuild.
