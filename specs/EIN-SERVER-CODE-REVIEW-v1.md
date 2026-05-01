# eind Code Review Report

_Evaluation Date: 2026-04-03_

---

## Executive Summary

The `eind` (located in `eind`) is a sophisticated gRPC server that implements a secure WASM-based agent hosting model. It successfully implements the core architecture of:
- Loading and orchestrating WASM WASI plugins with **per-session isolation**
- Fine-grained **access controls** (filesystem paths, network hosts)
- Graceful **error handling** (preserving sessions after errors)
- Mid-session **config updates** (updating credentials without dropping conversation history)

However, several **critical security, robustness, and correctness issues** need attention before production use.

**Overall Assessment: ⭐⭐ (7/10)**

> **Verdict**: Innovative architecture but needs hardening.

---

## Security Analysis 🔒

### 1. **Host Function Injection Risk** (CRITICAL) ⚠️

**Location**: `src/syscalls.rs` — `spawn` syscall implementation

```rust
impl crate::bindings::ein::plugin::process::Host for crate::HarnessState {
    async fn spawn(&mut self, args: String) -> Result<String, String> {
        let mut child = tokio::process::Command::new("sh")
            .args(["-c", &args])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        // ...
    }
}
```

**Vulnerability**: Direct shell invocation with `sh -c "$args"` allows arbitrary shell injection. A malicious tool plugin (or compromised LLM) could:
- Spawn unexpected processes (`/bin/false`, `nc`, `wget`, etc.)
- Write files outside allowed paths via `cd` and then write
- Escapes the WASI sandbox entirely

**Exploit Example**:
```rust
user_prompt: "Write a bash script to run /bin/cat /etc/passwd && rm -rf /"
tool_call: {"id": "tool_1", "name": "Bash", "arguments": "/bin/cat /etc/passwd && rm -rf /"}
```
A tool that accepts a script file could execute:
```bash
cat > /tmp/pwn.sh <<EOF
/bin/cat /etc/passwd
EOF
sh /tmp/pwn.sh
rm -f /tmp/pwn.sh
```

**Remediation**:

1. **Parse commands as array**: Use `Command::new(arg).args([arg2, arg3])` instead of `["-c", "$args"]`
2. **Add validation**: Only allow alphanumeric, basic punctuation, and common flags (`-c`, `--`, `=`)
3. **Block dangerous constructs**: Reject `|`, `&&`, `;`, `>`, `>>`, `>` in arguments
4. **Limit argument length**: Cap at 1024 characters
5. **Add audit logging**: Log all spawned processes with their arguments

```diff
async fn spawn(&mut self, args: String) -> Result<String, String> {
    // Validate and sanitize input
    let args = args.trim();
    if args.len() > 1024 {
        return Err(format!("Command too long (>{} chars)", 1024));
    }
    
    // Block dangerous characters
    let blocked = ['|', ';', '&&', '||', '>', '<', '`', '$('\`]
    if args.split_whitespace().any(|w| {
        blocked.iter().any(|b| w.contains(b))
    }) {
        return Err("Command contains blocked operators".into());
    }
    
    // Parse as array instead of shell -c
    let args_vec: Vec<&str> = args.split_whitespace().collect();
    let args = if args_vec.is_empty() {
        return Err("Empty command".into());
    } else {
        vec![&args_vec[0]]
    };
    
    let mut child = tokio::process::Command::new(args[0])
        .args(args.iter().skip(1).map(|s| *s))
        .spawn()
        .map_err(|e| e.to_string())?;
    // ...
}
```

---

### 2. **Network Access Control** (HIGH) ⚠️

**Location**: `src/main.rs` — `ModelClientHarnessState::send_request`

```rust
fn send_request(
    &mut self,
    request: hyper::Request<HyperOutgoingBody>,
    config: OutgoingRequestConfig,
) -> HttpResult<HostFutureIncomingResponse> {
    let host = request.uri().host()
        .map(|h| h.to_string())
        .or_else(|| { /* fallback to Host header */ })
        .unwrap_or_default();
    let allowed = self.allowed_hosts.contains("*") || self.allowed_hosts.contains(&host);
    if !allowed {
        eprintln!("[model client] blocked request to '{host}' ...");
        return Err(ErrorCode::HttpRequestDenied.into());
    }
    Ok(default_send_request(request, config))
}
```

**Vulnerabilities**:

1. **Redirect vulnerabilities**: HTTP redirects (302, 301) to external hosts are not blocked. An attacker could make the browser/client follow a redirect to a different host.
2. **Host header stripping**: If the upstream proxy strips the `Host` header and sets it maliciously, the check would bypass.
3. **Preload resources**: The WASI HTTP client might make unexpected calls (like preloading images) that bypass the check.

**Remediation**:

```diff
fn send_request(
    &mut self,
    request: hyper::Request<HyperOutgoingBody>,
    config: OutgoingRequestConfig,
) -> HttpResult<HostFutureIncomingResponse> {
    let host = request.uri().host()
        .map(|h| h.to_string())
        .or_else(|| { /* fallback to Host header */ })
        .unwrap_or_default();
    
    let allowed = self.allowed_hosts.contains("*") || self.allowed_hosts.contains(&host);
    if !allowed {
        eprintln!("[model client] blocked request to '{host}' ...");
        return Err(ErrorCode::HttpRequestDenied.into());
    }
    
    // Log and alert on blocked requests with suspicious patterns
    if host.is_empty() || host == "*" {
        return Err(ErrorCode::HttpRequestDenied.into());
    }
    
    // Wrap the response to intercept redirects (if supported)
    let future = default_send_request(request, config);
    // ... implement redirect interception ...
    
    // Add span context to traces if using OpenTelemetry
    let _guard = self.tracing_span.enter(|| async move {
        future.await
    });
    
    future
}
```

**Additional Security Controls**:

1. Add a redirect callback to intercept 3xx responses and validate the new host.
2. Use `wasmtime_wasi_http`'s redirect handling if available, or add a wrapper around `default_send_request`.
3. Log and alert on blocked requests with suspicious patterns.
4. Add a global "allow/deny" list for model client hosts at the service level.

---

### 3. **WASI Filesystem Preopenings** (MEDIUM) ⚠️

**Location**: `src/tools.rs` — `WasmTool::load`

```rust
for host_path in allowed_paths {
    wasi_builder = wasi_builder
        .preopened_dir(host_path, host_path, DirPerms::all(), FilePerms::all())
        .expect("failed to preopen dir");
    if first {
        wasi_builder = wasi_builder
            .preopened_dir(host_path, ".", DirPerms::all(), FilePerms::all())
            .expect("failed to preopen dir as current directory");
        first = false;
    }
}
```

**Analysis**:
- **Good**: Uses preopened directories with specific permissions (no symlink traversal, no parent directory access).
- **Issues**:
  - `DirPerms::all()` and `FilePerms::all()` grant read/write/execute permissions. This is too permissive for read-only tools (Read, Edit).
  - If `allowed_paths` is empty, the WASI sandbox runs with no filesystem access (correct).
  - The first path is mounted as `.` — this could expose a sensitive directory if the user's home or project root is passed.

**Remediation**:

```diff
for host_path in allowed_paths {
    // Validate path is absolute and exists
    if !host_path.is_absolute() {
        return Err(anyhow!("Path must be absolute: {host_path}"));
    }
    
    // Grant specific permissions based on path
    let dir_perms = if host_path == "/*" {
        // Allow write access only to the first path (which is mounted as ".")
        DirPerms::new()
            .read()
            .write()
            .remove()
            .execute()
    } else {
        // Read-only for all other paths
        DirPerms::new()
            .read()
    };
    let file_perms = FilePerms::new()
        .read()
        .write();
    
    wasi_builder = wasi_builder
        .preopened_dir(host_path, host_path, dir_perms, file_perms)
        .expect("failed to preopen dir");
    
    if first {
        wasi_builder = wasi_builder
            .preopened_dir(host_path, ".", dir_perms, file_perms)
            .expect("failed to preopen dir as current directory");
        first = false;
    }
}
```

**Additional Controls**:
1. For read-only tools (Read, Edit), use `DirPerms::read()` only.
2. Do not mount the first path as `.` without explicit user consent (via a modal).
3. Add a path resolution check: ensure no symlinks lead outside allowed paths.

---

### 4. **WASIMutex for Caches** (LOW) ⚠️

**Location**: `src/model_client.rs` — `ModelClientCacheInner`

```rust
struct ModelClientCacheInner {
    model_client_dir: PathBuf,
    cache: Mutex<HashMap<String, OnceCell<ModelClientPre<ModelClientHarnessState>>>>,
}
```

**Analysis**: The cache uses `Mutex` for file operations. Since WASM execution is single-threaded, `Mutex` is over-conservative but acceptable. `OnceCell` is `Sync+Send`, so sharing across tasks is safe. **No urgent fix needed**, but consider using `RwLock` if concurrency is needed in the future.

---

## Architecture & Design 🏗️

### 1. **Session Scoping** (GOOD) ✅

**Location**: `src/grpc.rs`

Each gRPC session gets:
- Its own event channel `mpsc::channel(32)`
- Its own WASM runtime (`Engine` shared, but per-session stores)
- Per-session model client instantiation

**Status**: **Excellent isolation.** No state is shared between clients.

**Potential Concern**: The `Engine` is shared across all sessions. While this saves compilation time, if one plugin crashes, the entire engine is compromised. Consider per-session engines with engine pooling if this becomes a bottleneck.

---

### 2. **Graceful Error Handling** (GOOD) ✅

**Location**: `src/agent.rs`

```rust
match model_session.complete(messages, &tool_registry.schemas()?).await {
    Ok(r) => r,
    Err(e) => {
        eprintln!("[agent] model client error: {e}");
        // Send AgentEvent, return Ok(()) to preserve session
        return Ok(());
    }
}
```

**Status**: **Excellent practice.** Errors are streamed to the client as `AgentEvent::AgentError`, and the session is not terminated. This allows the user to fix credentials and retry.

**Improvement**: Returning `Ok(())` after error might hide state. Consider using a separate `ErrorKind` enum:
- `AuthError` — credential problem (don't retry, log to user)
- `TransientError` — network timeout (retry)
- `PermanentError` — server down (log and bubble)

---

### 3. **Config Update Mid-Session** (GOOD) ✅

**Location**: `src/grpc.rs`

The server supports updating model client credentials without dropping the conversation history:

```rust
Some(user_input::Input::ConfigUpdate(cfg)) => {
    match model_client_session_manager.new_session(&cfg).await {
        Ok(new_session) => {
            let old_session = mem::replace(&mut model_session, new_session);
            // Cleanup old session, use new one
        }
    }
}
```

**Status**: **Excellent feature.**

**Documentation Needed**: `allowed_paths` and `allowed_hosts` are **not** updated mid-session per the TUI docs — this is by design, but should be clearly documented in error messages.

---

### 4. **Session Cleanup on Disconnect** (MISSING) ⚠️

**Issue**: When the client closes the inbound stream, the session task exits cleanly. However, `ToolRegistry::unload` is not explicitly called. WASM components should be dropped automatically, but there's no explicit cleanup of registered tools.

**Remediation**:

```diff
// src/grpc.rs — inside the session task
while let Ok(Some(msg)) = inbound.message().await {
    // ... handle message ...
}

// Add explicit cleanup
let _ = tool_registry.unload().await;
println!("[session] session ended, cleaning up");
```

---

## Correctness Issues 🐛

### 1. **Empty `messages` Vector on First Call** (MEDIUM) ⚠️

**Location**: `src/agent.rs`

```rust
let mut messages: Vec<Value> = vec![];
// ... later in loop
messages.push(json!({ "role": "user", "content": prompt }));
```

**Issue**:
- On the first prompt, `messages` is empty. The LLM receives no context.
- The system message with `allowed_paths` is prepended **before** the loop, but the first user message is still sent to an empty history.

**Impact**: The first LLM response may be poor or unrelated because it lacks context about what files are accessible.

**Remediation**:

```diff
pub async fn run_agent(
    messages: &mut Vec<Value>,
    tool_registry: &mut ToolRegistry,
    model_session: &mut ModelClientSession,
    tx: &mpsc::Sender<Result<AgentEvent, Status>>,
) -> anyhow::Result<()> {
    // Prepend a system message before the loop
    let allowed_paths_list = if !tool_registry.is_allowed_paths_empty() {
        let paths = tool_registry.get_allowed_paths();
        let paths_list = paths.iter()
            .map(|p| format!("{}: {}", p.canonicalize().unwrap_or_else(|_| p.to_string_lossy()), p.to_string_lossy()))
            .collect::<Vec<_>>()
            .join("\n");
        Some(json!({
            "role": "system",
            "content": format!(
                "The following filesystem paths are accessible to file tools:\n{paths_list}"
            ),
        }))
    } else {
        None
    };
    
    // ... rest of function ...
}
```

Actually, system messages are already prepended in `grpc.rs` before `run_agent`. The issue is that they're not always shown in the UI, which is fine.

**Status**: Acceptable.

---

### 2. **Tool Call Streaming Not Always Enabled** (LOW) ⚠️

**Location**: `src/agent.rs` — `handle_tool_call`

```rust
match tool.enable_chunk_sender().await {
    Ok(should_enable_chunk_sender) => {
        if should_enable_chunk_sender {
            tool.set_chunk_sender(tx.clone(), id.to_owned())
        }
    }
}
```

**Issue**:
- `enable_chunk_sender` may return `false` if the tool doesn't support streaming (e.g., Read, Write).
- The `spawn` syscall always streams, causing potential inconsistency.

**Impact**: Unnecessary overhead on `spawn` calls, potential for dropped events if the channel can't keep up.

**Remediation**:

```diff
async fn handle_tool_call(
    tx: &mpsc::Sender<Result<AgentEvent, Status>>,
    tool_registry: &mut ToolRegistry,
    id: &str,
    function: &FunctionCall,
) -> (String, String) {
    // Enable chunk sender only for tools that support it
    if function.name == "Batch" && tool_registry.enable_bash_streaming() {
        tool.set_chunk_sender(tx.clone(), id.to_owned())
        let _ = tx.send(Ok(AgentEvent {
            event: Some(Event::ToolOutputChunk(ToolOutputChunk {
                tool_call_id: id.to_string(),
                output: "Batch tool started".to_string(),
            })),
        })).await;
    }
    
    // ... rest of function ...
}
```

**Status**: Optional improvement.

---

### 3. **Message History Growth** (HIGH) ⚠️

**Location**: `src/agent.rs` — inside loop

```rust
// Loop: append assistant and tool results
messages.push(serde_json::to_value(&choice.message)?);
messages.push(json!({"role": "tool", "tool_call_id": id, "content": result_str}));
```

**Issue**:
- `messages` grows indefinitely with each turn.
- For long conversations, this will:
  - OOM the server
  - Cause the LLM to exceed token limits

**Impact**: A 100-turn conversation could exceed 16KB of history, potentially causing out-of-memory errors.

**Remediation**:

```rust
const MAX_HISTORY_TOKENS: usize = 8000;

pub async fn run_agent(
    messages: &mut Vec<Value>,
    // ...
) -> anyhow::Result<()> {
    // Check for history overflow
    let total_tokens = estimate_token_count(messages);
    if total_tokens > MAX_HISTORY_TOKENS {
        // Summarize or drop old messages
        if let Some(summarized) = summarize_messages(&messages[..15]) {
            messages[15] = summarized;
        } else {
            // Remove oldest messages until under limit
            while estimate_token_count(messages) > MAX_HISTORY_TOKENS {
                messages.remove(0);
            }
        }
    }
    
    // ... rest of loop ...
}

fn estimate_token_count(messages: &[Value]) -> usize {
    // Rough estimate: 4 tokens per character
    messages.iter()
        .map(|msg| msg["content"].as_str()
            .map_or(0, |c| c.len() * 4))
        .sum()
}
```

**Alternative**: Add a `/clear_history` slash command and document the limit.

---

### 4. **No Retry Logic for LLM Calls** (HIGH) ⚠️

**Location**: `src/agent.rs`

```rust
let resp = match model_session.complete(messages, &tool_registry.schemas()?).await {
    Ok(r) => r,
    Err(e) => {
        eprintln!("[agent] model client error: {e}");
        // Send AgentEvent, return Ok(()) to preserve session
        return Ok(());
    }
};
```

**Issue**: Transient network failures (timeouts, 5xx) are treated as terminal. The session is preserved, but the request is not retried.

**Impact**: If the LLM API is temporarily unavailable, the user gets a dead end with no way to recover.

**Remediation**:

```rust
const MAX_RETRIES: usize = 3;
const RETRY_DELAY_MS: u64 = 500;

async fn make_llm_call<T: Serialize>(
    model_session: &mut ModelClientSession,
    messages: &Vec<Value>,
    tools: &Vec<Value>,
) -> anyhow::Result<CompletionResponse> {
    let mut retries = 0;
    while retries < MAX_REtries {
        match model_session.complete(messages, tools).await {
            Ok(r) => return Ok(r),
            Err(e) if is_retryable_error(&e) => {
                retries += 1;
                eprintln!("[agent] retrying in {}ms: {}", RETRY_DELAY_MS * retries, e);
                tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS * retries)).await;
            }
            Err(e) => return Err(anyhow!("LLM failed after {} retries: {}", MAX_RETRIES, e)),
        }
    }
    Err(anyhow!("LLM failed after {} retries", MAX_RETRIES))
}

fn is_retryable_error(e: &anyhow::Error) -> bool {
    e.to_string().contains("timeout") 
        || e.to_string().contains("500") 
        || e.to_string().contains("unavailable")
}
```

**Status**: **High priority fix.**

---

### 5. **Channel Capacity Hardcoded** (LOW) ⚠️

**Location**: `src/grpc.rs`

```rust
let (tx, rx) = mpsc::channel(32);
```

**Issue**: Buffer size of 32 might overflow on high concurrency (many parallel tool calls).

**Remediation**:

```rust
#[derive(Debug, Clone)]
pub struct ServerArgs {
    #[arg(long, default_value = "50051")]
    port: u16,
    #[arg(long, default_value = "32")]
    channel_buffer: usize,
    #[arg(long, default_value = "INFO")]
    log_level: LogLevel,
}
```

---

## Code Quality

### 1. **Missing Unit/Integration Tests** (HIGH) ⚠️

**Current State**: As noted in `CLAUDE.md`, there are no tests yet.

**Recommendation**:

1. Add property-based tests for message serialization.
2. Mock WASM calls for integration tests.
3. Test session lifecycle (init, update, terminate).
4. Add fuzzing for message parsing.

**Example Test Setup**:

```rust
// tests/server_moderation.rs
#[cfg(test)]
mod tests {
    use ein_proto::ein::SessionConfig;
    use tokio::test;
    
    #[test]
    fn test_session_initialization() {
        // Test that a new session initializes correctly with no plugins
        let server = AgentServer::new().await.unwrap();
        assert!(server);
    }
    
    #[test]
    fn test_config_update() {
        // Test that config_update mid-session works
        // ...
    }
}
```

**Status**: **High priority** — no tests is unacceptable for production code.

---

### 2. **Verbose Logging** (MEDIUM) ⚠️

**Current State**:

```rust
println!("[session] new session started");
println!("[session] loading plugins from {}...", config.plugin_dir.display());
eprintln!("[agent] api error: {msg}");
```

**Issue**: Too noisy for production.

**Remediation**:

```rust
// Use tracing
use tracing::{debug, info, error, warn};

info!(session_id = %session_id, "New session started");
debug!(plugin_dir = %config.plugin_dir.display(), "Loading plugins");
error!(error = %msg, "API error");
```

**Status**: Medium priority.

---

### 3. **Unused `#[expect]` Attributes** (LOW) ⚠️

**Location**: `src/main.rs`

```rust
#[expect(unused)]
ein_dir: PathBuf,
```

**Issue**: The `ein_dir` field is stored but unused. If it was meant to be used, keep it. Otherwise, remove or document why it exists.

**Status**: Low priority.

---

### 4. **String Concatenation Errors** (MEDIUM) ⚠️

**Location**: `src/agent.rs` — `handle_tool_call`

```rust
(format!("Error: {e}"), String::new())
```

**Issue**: If the tool result `result_str` is very long, the metadata string might be truncated.

**Remediation**:

```rust
const MAX_OUTPUT_LENGTH: usize = 10 * 1024;

fn truncate_with_marker(str: &str) -> (String, bool) {
    if str.len() <= MAX_OUTPUT_LENGTH {
        return (str.to_string(), false);
    }
    let truncated = format!("{}", &str[..MAX_OUTPUT_LENGTH]);
    (truncated, true)
}

(match tool.call(id, &function.arguments).await {
    Ok(res) => {
        let (content, is_truncated) = truncate_with_marker(&res.content);
        let meta = res.metadata.as_ref()
            .map(|v| truncate_with_marker(&v.to_string()).0)
            .unwrap_or_default();
        let meta_metadata = truncate_with_marker(&meta).1;
        // ...
    }
```

**Status**: Medium priority.

---

## Performance Considerations ⚡

### 1. **Engine Shared Across Sessions** (INFO)

```rust
// grpc.rs
let engine = Engine::default();
let server = AgentServer {
    engine,
    // ...
};
```

**Impact**: Good for compilation, but a single crashed plugin can affect all sessions.

**Recommendation**: Acceptable for now, but consider per-session engines with a pool if this becomes a bottleneck.

---

### 2. **Blocking Compilation in `spawn_blocking`** (INFO)

```rust
// model_client.rs
Ok(tokio::task::spawn_blocking(move || Component::from_file(&engine, &path)).await??)
```

**Impact**: Good — avoids blocking the async runtime during WASM compilation.

**Status**: Excellent.

---

### 3. **Memory Usage of Model Client Cache** (INFO)

```rust
// model_client.rs
struct ModelClientCacheInner {
    // Holds compiled WASM components in memory
    cache: Mutex<HashMap<String, OnceCell<...>>>,
}
```

**Impact**: Each model client plugin is loaded once and cached. For most deployments, this is fine. However, if you support multiple LLM APIs (e.g., OpenRouter + Ollama), the cache could grow large.

**Recommendation**: Monitor memory usage and add an LRU eviction policy if cache grows too large.

---

## Security Recommendations 🛡️

**Priority Order**:

1. **CRITICAL**: Harden `spawn` syscall (shell injection risk)
2. **HIGH**: Add redirect validation for model client HTTP calls
3. **HIGH**: Implement retry logic for LLM calls
4. **HIGH**: Control message history size to avoid OOM
5. **MEDIUM**: Add file permission controls for WASI preopen
6. **MEDIUM**: Replace `println!` with `tracing`
7. **LOW**: Add explicit session cleanup

---

## Testing Recommendations 🧪

### 1. **Unit Tests**

- Test message serialization/deserialization
- Test path validation for WASI preopens
- Test network allowlist derivation

### 2. **Integration Tests**

- Simulate a session lifecycle (init, prompt, tool call, finish)
- Test config update mid-session
- Test error recovery (network failures)

### 3. **Fuzzing**

- Fuzz message parsing
- Fuzz path validation

### 4. **Load Testing**

- Test with concurrent sessions
- Test with large message histories
- Test with slow model responses

---

## Documentation Gaps 📚

1. **Error Messages**: When config update fails, explain which fields were ignored (e.g., `allowed_paths`).
2. **Tool Streaming**: Document which tools support streaming output.
3. **Message History**: Document the token limit and how messages are dropped.
4. **Session Cleanup**: Document the expected behavior when clients disconnect.

---

## Conclusion

**The `eind` is a well-architected and innovative implementation** of a secure WASM-based agent hosting model. Its design prioritizes:
- ✅ Session isolation
- ✅ Fine-grained access control
- ✅ Graceful error handling

However, **several critical security and robustness issues** need attention before production use.

**Estimated Effort to Production-Ready**:
| Task | Effort |
|------|--------|
| Security hardening (`spawn`, redirects) | 2–3 days |
| Retry logic for LLM calls | 1 day |
| Message history control | 1 day |
| Test suite | 3–5 days |
| Logging improvements | 0.5 day |
| **Total** | **~1–2 weeks** |

---

## Action Items

1. **Immediate** (This week):
   - Add retry logic for LLM calls
   - Control message history size
   - Add explicit session cleanup

2. **Short-term** (Next sprint):
   - Harden `spawn` syscall
   - Add redirect validation
   - Replace `println!` with `tracing`

3. **Long-term** (Ongoing):
   - Build comprehensive test suite
   - Add fuzzing
   - Performance benchmarking

---

## Appendix A: Security Checklist

- [ ] Validate `spawn` syscall arguments for shell injection
- [ ] Add redirect handling for HTTP client
- [ ] Limit WASI file permissions per tool type
- [ ] Add audit logging for all security-relevant operations
- [ ] Add rate limiting for tool calls

## Appendix B: Error Classification

```rust
#[derive(Debug, Clone)]
pub enum ErrorKind {
    AuthFailed(String),        // Credential error — don't retry
    Transient(String),         // Network error — retry
    Permanent(String),         // Server down — log and bubble
    ResourceExhausted(String), // OOM, rate limit — backoff
}
```

---

**Report Generated By**: AI Code Review System  
**Reviewed By**: Mason Stallmo  
**Date**: 2026-04-03
