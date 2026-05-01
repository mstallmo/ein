# Roadmap: MCP Server Support for `ein_agent`

_Date: 2026-04-20_

---

## Goal

Make `ein_agent` a competitive Rust SDK for building MCP servers. The primary differentiator is WASM-sandboxed tool execution — deny-by-default filesystem and network access per tool, with explicit per-tool capability grants. The Vercel/Context.ai incident (April 2026) demonstrated that the current generation of MCP tools has no answer to a compromised server exfiltrating OAuth tokens. Ein's existing WASM runtime directly addresses this, and the timing is good.

The target bar is parity with the official `rmcp` SDK on protocol coverage and ergonomics, with WASM sandboxing as a clear security advantage over everything in the ecosystem.

---

## Competitive Landscape

| Crate | Strengths | Weaknesses |
|---|---|---|
| `rmcp` (official) | Protocol-correct, proc macros (`#[tool]`), 4.7M+ downloads, tokio-native | No sandboxing, any tool can read env/fs/network freely |
| `rust-mcp-sdk` | Full spec (2025-11-25), proc macros, type-safe schemas via `schemars` | No sandboxing |
| `rust-mcp-server` | Opinionated Rust dev tooling wrapper | Narrow scope, not general-purpose |
| `rmcp-actix-web` | HTTP/SSE transport layer | Transport adapter only, not a standalone SDK |

None of them provide sandboxed tool execution. The post-Vercel conversation about MCP security is happening now and no SDK has a concrete answer to it.

---

## Phases

### Phase 1: `ein_agent` trait improvements (prerequisite)

These changes are required before any MCP work begins. They unblock concurrent MCP calls and remove constraints that don't make sense for a server use case.

**`ToolSet::schemas()` → async and fallible**

The current signature is synchronous and infallible:
```rust
fn schemas(&self) -> Vec<ToolDef>;
```

An MCP server that dynamically discovers tools (e.g. scanning a WASM plugin directory) cannot work with this. Change to:
```rust
async fn list_tools(&self) -> anyhow::Result<Vec<ToolDef>>;
```

`NativeToolSet` and `WasmToolSet` both have trivially correct implementations.

**`ToolSet::call_tool` → `&self` with interior mutability**

The current `&mut self` signature serializes all tool calls. MCP servers must handle concurrent requests. The fix is to require `&self` on the trait and push synchronization into implementations. `NativeToolSet` needs no change (tools are `Send + Sync`); `WasmToolSet` already uses internal locking.

**`ToolContent` enum to replace `String` content**

MCP tool results can be multi-part: text, images, and embedded resources. Replace the `content: String` field in `ToolResult` with:
```rust
pub enum ToolContent {
    Text { text: String },
    Image { data: String, mime_type: String },
    Resource { uri: String, mime_type: Option<String>, text: Option<String> },
}
```

**`ToolCallContext` for per-call identity**

Add a context type threaded into `call_tool` to carry caller identity and capability grants. This is the foundation for per-caller authorization in Phase 4.
```rust
pub struct ToolCallContext {
    pub request_id: String,
    pub caller_id: Option<String>,
}
```

---

### Phase 2: `ein-mcp` crate — protocol core

New crate. Implements the MCP wire protocol and exposes a server builder that accepts any `ToolSet`.

**MCP JSON-RPC 2.0 message types**

Full type coverage of the MCP 2025-11-25 spec:
- `initialize` / `initialized` request/response/notification
- `tools/list` and `tools/call` request/response
- Error codes and error objects
- Notification types (`notifications/cancelled`, etc.)

Use `serde` for serialization. Keep these types in `ein_core` so they can be shared with a future MCP client.

**Capability negotiation**

The server advertises its capabilities during the `initialize` handshake. At minimum: `{ "tools": {} }`. Resources and prompts can be added in Phase 6.

**`McpServer` builder**

```rust
McpServer::builder()
    .tool_set(my_tool_set)
    .server_info("my-server", "0.1.0")
    .build()
```

**Stdio transport**

The first transport to ship. Drives the JSON-RPC framing over stdin/stdout. This is what Claude Desktop, Cursor, and most local MCP clients expect. Read newline-delimited JSON-RPC from stdin, write responses to stdout.

At the end of Phase 2, a user can write:
```rust
let server = McpServer::builder()
    .tool_set(my_tools)
    .build();
server.serve_stdio().await?;
```

---

### Phase 3: Ergonomics — proc macros

This is the table-stakes ergonomics layer. The official `rmcp` SDK has `#[tool]` attribute macros; we need an equivalent or better. This phase makes `ein-mcp` competitive for developers who don't need WASM sandboxing.

**`#[mcp_tool]` attribute macro**

Derive a `Tool` impl from a regular async Rust function:
```rust
#[mcp_tool(description = "Run a shell command and return its output")]
async fn bash(
    /// The shell command to run
    command: String,
) -> anyhow::Result<ToolResult> {
    // ...
}
```

The macro:
- Generates `fn name() -> &str`
- Generates `fn schema() -> ToolDef` by reflecting on the function signature and doc comments
- Registers parameter types using `schemars` for JSON Schema generation
- Handles the `args: &str` → typed struct deserialization

**`schemars` integration**

Use `schemars::JsonSchema` as the parameter schema source. Derive it on input structs to get JSON Schema for free:
```rust
#[derive(Deserialize, JsonSchema)]
struct BashArgs {
    /// The command to run
    command: String,
    /// Working directory (optional)
    cwd: Option<String>,
}
```

**`#[mcp_server]` router macro**

```rust
#[mcp_server]
impl MyServer {
    #[tool]
    async fn bash(&self, args: BashArgs) -> anyhow::Result<ToolResult> { ... }

    #[tool]
    async fn read_file(&self, args: ReadArgs) -> anyhow::Result<ToolResult> { ... }
}
```

Generates the `ToolSet` impl, routing `call_tool` by name to the right method and collecting schemas from all `#[tool]` methods.

---

### Phase 4: HTTP/SSE transport + per-caller authorization

**HTTP + SSE transport**

Remote MCP servers (accessed by web clients, CI systems, shared tooling) need HTTP. Implement an HTTP/SSE transport using `axum`:
- `POST /mcp` — JSON-RPC request/response
- `GET /mcp/sse` — server-sent events for streaming responses and notifications
- Session lifecycle via a `session_id` header

**OAuth token validation**

The Vercel incident was partly enabled by MCP servers receiving OAuth tokens with no validation layer. Add an optional middleware hook:
```rust
McpServer::builder()
    .tool_set(my_tools)
    .auth(|token| async move { validate_jwt(token).await })
    .build()
```

The validated identity is threaded into `ToolCallContext.caller_id`.

**Per-caller tool authorization**

Gate which tools a caller can invoke based on their identity:
```rust
McpServer::builder()
    .tool_set(my_tools)
    .authorize(|caller, tool_name| {
        // return true/false
    })
    .build()
```

---

### Phase 5: WASM tool sandboxing — the differentiator

This is what makes `ein-mcp` uniquely positioned. Each tool is a WASM component running in a Wasmtime sandbox with explicit, per-tool capability grants. A compromised or malicious tool cannot exfiltrate data, read credentials, or make unauthorized network calls.

**`WasmMcpServer`**

A pre-wired `McpServer` variant that loads tools from a directory of `.wasm` components:
```rust
WasmMcpServer::builder()
    .tools_dir("~/.my-server/tools/")
    .build()
    .serve_stdio()
    .await?;
```

Each `.wasm` file is a WASM component implementing the `ToolPlugin` WIT interface (already defined in `packages/ein_tool/`). The filename stem is the tool's config identity.

**Per-tool capability config**

```toml
[tools.ein_bash]
allowed_hosts = []          # no outbound network
allowed_paths = ["/tmp"]    # only /tmp

[tools.ein_github]
allowed_hosts = ["api.github.com"]
allowed_paths = []
```

Tools that attempt to open a disallowed file path or connect to an unauthorized host get a WASI error — not just a runtime error, but a sandbox boundary violation before any packet leaves the process.

**JSON Schema validation before dispatch**

Before calling into WASM, validate the `args` JSON against the tool's declared parameter schema. A tool that never receives malformed input cannot be exploited via argument injection. This is a middleware step in `call_tool` that runs before the WASM boundary.

**Structured audit log**

Emit structured events for every tool invocation:
```json
{
  "ts": "2026-04-20T14:23:01Z",
  "request_id": "req_abc123",
  "caller_id": "user@example.com",
  "tool": "ein_bash",
  "args": {"command": "ls /tmp"},
  "result_bytes": 142,
  "duration_ms": 38,
  "sandbox_violations": []
}
```

Write these to a configurable sink (stderr, file, syslog). This is the audit trail the Vercel incident lacked.

**Security documentation**

Ship a dedicated security guide that explains the threat model, how each protection works, and how to configure capability grants. Make the comparison to unprotected MCP servers explicit. The Vercel incident should be cited as a motivating example.

---

### Phase 6: Resources and Prompts

Full MCP spec compliance. The previous phases cover tools, which is the most-used primitive. This phase adds the remaining two.

**Resources**

Resources are read-only data sources (files, database rows, API responses) that MCP clients can subscribe to. Add a `Resource` trait and `ResourceSet` parallel to `Tool`/`ToolSet`:
```rust
#[async_trait]
pub trait Resource: Send + Sync {
    fn uri(&self) -> &str;
    fn description(&self) -> &str;
    async fn read(&self) -> anyhow::Result<ResourceContent>;
}
```

WASM-backed resources get the same sandbox treatment as tools.

**Prompts**

Prompts are parameterized message templates the server exposes to clients:
```rust
#[async_trait]
pub trait Prompt: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> PromptDef;
    async fn render(&self, args: &str) -> anyhow::Result<Vec<Message>>;
}
```

**Full spec compliance**

Target the `2025-11-25` spec version. Implement remaining notifications, pagination on `list` responses, and capability flags for all three primitives.

---

### Phase 7: MCP client in `ein_agent`

Close the loop: let Ein agents consume tools from any MCP server, not just tools registered locally. This makes Ein bidirectional.

**`McpToolSet`**

A `ToolSet` implementation backed by a remote MCP server connection:
```rust
let mcp_tools = McpToolSet::connect_stdio("npx @modelcontextprotocol/server-filesystem /path").await?;

let agent = Agent::builder(model_client)
    .with_tool_set(mcp_tools)
    .build();
```

`McpToolSet::list_tools()` calls `tools/list` on the remote server; `call_tool()` sends `tools/call`. The agent loop sees no difference from native tools.

This also enables Ein to act as a **proxy**: an Ein `McpServer` can front a `McpToolSet` that calls out to another MCP server, adding sandboxing and auth to an existing unprotected server.

---

## Crate structure

```
crates/
  ein_core/       existing — add MCP JSON-RPC types here
  ein_agent/      existing — Phase 1 trait improvements
  ein_mcp/        new — McpServer, transports, auth middleware
  ein_mcp_macros/ new — #[mcp_tool], #[mcp_server] proc macros
```

`ein_mcp` depends on `ein-agent` (for `ToolSet`) and `ein_core` (for shared types). The macro crate is a separate proc-macro crate depended on by `ein_mcp`.

---

## Sequencing rationale

Phases 1–3 bring `ein-mcp` to parity with `rmcp` on the features developers evaluate first: protocol correctness, transports, and ergonomics. Phase 5 (WASM sandboxing) is the reason to build this at all — but it needs the foundation of Phases 1–3 to be usable. Phase 4 (HTTP/SSE + auth) is ordered before Phase 5 because the OAuth token validation story is directly tied to the Vercel incident narrative and should ship together with the sandboxing story.

Phases 6–7 are valuable for completeness and unique positioning (the proxy pattern in Phase 7 is genuinely novel) but are not prerequisites for a compelling initial release.

---

## Sources

- [MCP Specification 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25)
- [modelcontextprotocol/rust-sdk](https://github.com/modelcontextprotocol/rust-sdk)
- [4t145/rmcp](https://github.com/4t145/rmcp)
- [rust-mcp-sdk on crates.io](https://crates.io/crates/rust-mcp-sdk)
- [Context.ai OAuth Token Compromise — Wiz](https://www.wiz.io/blog/contextai-oauth-token-compromise)
- [A Timeline of MCP Security Breaches — AuthZed](https://authzed.com/blog/timeline-mcp-breaches)
- [`notes/ein-agent-mcp-fit-analysis.md`](./ein-agent-mcp-fit-analysis.md)
- [`notes/vercel-mcp-incident-analysis.md`](./vercel-mcp-incident-analysis.md)
