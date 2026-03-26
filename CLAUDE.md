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

The protocol is defined in `crates/ein-proto/proto/ein.proto`. The client streams `UserInput` messages; the server streams back `AgentEvent` messages (`ContentDelta`, `ToolCallStart`, `ToolCallEnd`, `AgentFinished`, `AgentError`).

### Server (`crates/ein-server/`)

| File | Role |
|------|------|
| `src/main.rs` | CLI arg parsing, `EinConfig`, `HarnessState`, server startup |
| `src/grpc.rs` | `AgentServer` — tonic `Agent` impl, spawns per-session tasks |
| `src/agent.rs` | `run_agent` — the LLM ↔ tool loop |
| `src/tools.rs` | `ToolRegistry` + `WasmTool` — loads and calls WASM plugins |
| `src/syscalls.rs` | Host functions exposed to WASM plugins (spawn, log, …) |

**Agent loop** (`src/agent.rs`): sends the message history to the LLM, streams `ContentDelta` events for text output, executes each requested `ToolCall` via the registry, appends results to history, and loops until `FinishReason::Stop`.

**Plugin loading** (`src/tools.rs`): scans the plugin directory for `.wasm` files, instantiates each as a Wasmtime component, and calls `name()`/`schema()` to self-describe. In debug mode this is `./target/wasm32-wasip2/debug/`; in release mode `~/.ein/plugins/`.

### TUI (`crates/ein-tui/`)

Single file: `src/main.rs`. Uses **Ratatui** (v0.29) for rendering and **crossterm** for keyboard events.

**Layout** (top → bottom):
1. **Conversation pane** — scrollable message history; streams agent output in real time
2. **Input area** — single/multi-line text field with character-level wrapping; dark-peach border
3. **Autocomplete section** — always 3 lines tall; shows slash-command hints when input starts with `/`

**Color palette** — all colors are named constants at the top of `main.rs`:
- `INPUT_BORDER_COLOR` — muted dark-peach/terracotta border on the input area
- `TOOL_NAME_COLOR` — steel blue for the `▸ ToolName` tool call indicator
- `THINKING_COLOR` — soft sky blue for the animated thinking spinner
- `MUTED_COLOR` — dark grey for secondary text (args, autocomplete descriptions)

**Thinking animation**: a braille spinner (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`) appears in the conversation pane while the agent is busy, driven by an 80 ms ticker.

**Tool call display**: `▸ ToolName  primary_arg` — for `Bash` the command is shown; for `Read`/`Write` the file path is shown.

**Slash commands**: defined in the `COMMANDS` constant. Currently only `/exit`. Adding a command requires appending a `CommandDef` entry there.

**Scrolling**: `↑`/`↓` arrows scroll the conversation. `scroll_offset` counts lines up from the bottom; auto-scroll re-engages when the view reaches the bottom again.

### WASM plugin interface (`packages/`)

| Package | Tool name | Description |
|---------|-----------|-------------|
| `ein_bash` | `Bash` | Executes shell commands via the `spawn` syscall |
| `ein_read` | `Read` | Reads a file from the filesystem |
| `ein_write` | `Write` | Writes content to a file |

Plugins implement the `ToolPlugin` trait from `packages/ein_tool/` and declare their name, description, and JSON parameter schema via `ToolDef`. They are compiled to `wasm32-wasip2`.

To add a new tool, create a package under `packages/` implementing `ToolPlugin`, add it to `build_install_plugins.sh`, and rebuild.
