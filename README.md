# Ein

Ein is a Rust-based AI agent framework. A gRPC server drives an LLM agent loop (powered by Claude via OpenRouter) and executes tools implemented as pluggable WASM modules. A terminal UI client connects to the server and provides an interactive chat interface.

```
┌─────────────────────────┐          ┌──────────────────────────────┐
│        ein-tui          │   gRPC   │          ein-server          │
│                         │◄────────►│                              │
│  Interactive chat UI    │          │  LLM agent loop              │
│  Slash command hints    │          │  WASM tool executor          │
│  Animated thinking UI   │          │  OpenRouter client           │
└─────────────────────────┘          └──────────────────────────────┘
```

## Getting Started

### Prerequisites

- [Sign up for OpenRouter](https://openrouter.ai/) and [create an API key](https://openrouter.ai/settings/keys)
- Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Setup

**Add the WASM compile target**
```bash
rustup target add wasm32-wasip2
```

**Build the WASM plugins** (Bash, Read, Write tools)
```bash
./scripts/build_install_plugins.sh
```

> In debug builds, plugins are loaded automatically from `./target/wasm32-wasip2/debug/` — no installation needed.

### Running

Start the server in one terminal:

```bash
OPENROUTER_API_KEY=<your-key> cargo run -p ein-server
```

Start the TUI client in another:

```bash
cargo run -p ein-tui
```

The TUI connects to `localhost:50051` by default. To connect to a different address:

```bash
cargo run -p ein-tui -- http://my-server:50051
```

## Usage

Type a message and press **Enter** to send it to the agent. Type `/` to see available slash commands.

| Key | Action |
|-----|--------|
| `Enter` | Send message / run slash command |
| `↑` / `↓` | Scroll conversation history |
| `/exit` + `Enter` | Exit the TUI |

While the agent is working, an animated indicator appears in the chat panel. Tool invocations are shown inline as the agent uses them:

```
 ▸ Bash  ls -la
 ▸ Write  src/main.rs
```

## Tools

Tools are WASM components loaded at startup. Three are included out of the box:

| Tool | Description |
|------|-------------|
| `Bash` | Execute shell commands |
| `Read` | Read a file from the filesystem |
| `Write` | Write content to a file |

### Adding a tool

1. Create a new package under `packages/` implementing the `ToolPlugin` trait from `packages/ein_tool/`
2. Add it to `scripts/build_install_plugins.sh`
3. Rebuild — the server picks it up automatically on next start

## Architecture

```
crates/
  ein-proto/    Protocol Buffer definitions (gRPC service + message types)
  ein-server/   gRPC server — agent loop, WASM plugin host
  ein-tui/      Terminal UI client
packages/
  ein_tool/     WASM plugin interface (ToolPlugin trait, ToolDef, syscalls)
  ein_bash/     Bash tool plugin
  ein_read/     Read tool plugin
  ein_write/    Write tool plugin
```

The protocol (`crates/ein-proto/proto/ein.proto`) defines a bidirectional streaming RPC. The client streams `UserInput` messages; the server streams back `AgentEvent` messages as the agent thinks, calls tools, and produces output.
