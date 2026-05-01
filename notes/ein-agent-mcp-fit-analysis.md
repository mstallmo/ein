# `ein_agent` Architecture Fit for MCP Server Building

_Evaluation Date: 2026-04-20_

---

## Summary

The `ein_agent` architecture maps well onto the *client* side of MCP but is missing the abstractions needed to be a proper MCP *server* framework. It is well-positioned to grow in that direction, but there are meaningful gaps.

---

## What Maps Cleanly

### `Tool` / `ToolSet` traits (`crates/ein_agent/src/tools/mod.rs`)

The best fit. MCP's core concept is a server that exposes named tools with JSON schemas — that is exactly what `Tool` and `ToolSet` model. An MCP server implementation would be a new `ToolSet` adapter that routes calls to MCP rather than local functions.

### `ToolDef` / `ToolFunction` / `ToolFunctionParams` (`crates/ein_core/src/types.rs`)

Already serialize to OpenAI function-calling format, which is structurally identical to MCP's tool definition schema. Very little translation needed.

### `ToolResult` with `metadata`

A good primitive — MCP tool responses can carry structured content alongside text, and `metadata: Option<serde_json::Value>` supports that pattern.

### `AgentBuilder` / `ToolSet` composition

The `builder_with_tool_set` pattern means you can drop in a `McpToolSet` impl without changing `Agent` at all.

---

## Gaps That Would Need Addressing

### No transport layer

MCP servers communicate over stdio or HTTP/SSE. The `ToolSet` trait has no notion of a connection, session negotiation, or protocol framing. A new crate (`ein-mcp` or similar) would be needed to wrap a `ToolSet` and serve it over a transport. The current design does not obstruct this — it just does not provide it.

### No capability negotiation

MCP has a handshake phase (`initialize` / `initialized`) where client and server exchange capability lists. Nothing in `ToolSet` or `Agent` models this. An MCP server needs to advertise which tools it exposes *before* any calls are made. `schemas()` is close but it is pull-only and has no async or fallible path.

### `ToolSet::schemas()` is synchronous and infallible

```rust
fn schemas(&self) -> Vec<ToolDef>;
```

For an MCP server that lazily loads or discovers tools (e.g. from WASM plugins loaded on demand, or from a remote source), this is too rigid. An async, fallible signature would be more appropriate:

```rust
async fn list_tools(&self) -> Result<Vec<ToolDef>>;
```

### `ToolSet::call_tool` takes `&mut self`

MCP servers must handle concurrent tool calls. The `&mut self` requirement serializes all calls. `NativeToolSet` does not need mutual exclusion (tools are `Send + Sync`), but the trait forces it. This would require redesign — likely pushing concurrency to `Arc<RwLock<_>>` internally or changing the trait to `&self` with interior mutability.

### No request context / caller identity

MCP has a concept of the calling client, session, and capabilities. The current `call_tool(name, id, args)` signature carries no caller context. For a secure MCP server you would want to pass auth tokens, rate-limit state, or per-caller capability restrictions through to tool execution.

### `ToolResult.content` is a plain `String`

MCP tool results can be multi-part (text + image + embedded resources). The content model would need to support a `Vec<ContentPart>` shape.

---

## Security-Specific Considerations

MCP security concerns map onto existing Ein patterns in some places and are absent in others:

| Concern | Ein today | Gap |
|---|---|---|
| Filesystem sandboxing | WASM plugins use `WasiCtxBuilder::preopened_dir` — strong boundary | Native tools have no sandboxing |
| Network allowlisting | `allowed_hosts` in `SessionConfig`, enforced in `ModelClientHarnessState` | No equivalent for MCP transport connections |
| Input validation | Not present in `Tool` trait | MCP clients can send arbitrary `args` JSON — schema validation before `call()` would be a clear security boundary to add |
| Tool authorization | Not present — any tool in the registry can be called by name | MCP needs per-caller tool authorization |
| Audit logging | Only `tracing` spans at info level in `agents.rs` | A security-oriented MCP server needs structured, tamper-evident call logs |

The WASM-based isolation story (`WasmToolSet` backed by Wasmtime) is the strongest existing security primitive — if each tool is a WASM component, the sandboxing would be genuinely meaningful. Native `Tool` impls have no isolation at all.

---

## Recommended Path

1. Make `ToolSet::schemas()` async and fallible.
2. Change `call_tool` from `&mut self` to `&self` (push locking into implementations).
3. Add an `McpToolSet` adapter in a new `ein-mcp` crate that wraps any `ToolSet` and serves it over stdio/SSE.
4. Add per-call context (caller identity, capabilities) to the `call_tool` signature.
5. Add JSON Schema validation of `args` before dispatch as a middleware layer.
