# Writing a Tool Plugin

A tool plugin implements two traits from `ein_plugin::tool`:

- **`ConstructableToolPlugin`** — provides a zero-argument `new()` constructor called when the plugin is loaded
- **`ToolPlugin`** — the core interface: name, schema, and the `call` method

## The traits

```rust
pub trait ConstructableToolPlugin: ToolPlugin {
    fn new() -> Self;
}

pub trait ToolPlugin: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> ToolDef;
    fn call(&self, id: &str, args: &str) -> anyhow::Result<ToolResult>;

    // Optional — enable for tools that produce streaming output:
    fn enable_chunk_sender(&self) -> bool { false }

    // Optional — the arg name to display next to the tool name in the UI:
    fn primary_arg(&self) -> Option<&str> { None }
}
```

Tool plugins do **not** receive config JSON — there is no `config_json` parameter to `new()`. Per-session configuration reaches the plugin via the WASI context (preopened filesystem paths and allowed network hosts), not constructor arguments.

## Defining the tool schema

`ToolDef` describes the tool to the LLM: name, description, and parameters.

```rust
use ein_plugin::tool::ToolDef;

ToolDef::function("MyTool", "What this tool does")
    .param("required_param", "string", "Description of this param", true)
    .param("optional_param", "string", "Optional param", false)
    .build()
```

`param` arguments: `(name, type, description, required)`. The type is a JSON Schema type string — use `"string"`, `"number"`, `"boolean"`, or `"array"`.

## Returning results

```rust
use ein_plugin::tool::ToolResult;

// Simple result:
ToolResult::new(id, "output string".to_string())

// Result with metadata (used by the Edit tool for diff display):
ToolResult::with_metadata(id, content, serde_json::json!({
    "start_line": 42,
    "old_lines": ["old line 1"],
    "new_lines": ["new line 1"]
}))
```

The `id` is the tool call ID passed to `call` — always pass it through unchanged.

## Available syscalls

```rust
use ein_plugin::tool::syscalls;

// Execute a shell command; returns stdout+stderr as a String:
let output = syscalls::spawn("ls -la")?;

// Write a debug message to the server log:
syscalls::log("my_tool: doing something");
```

`spawn` returns `Result<String, String>`. Map the error with `anyhow::anyhow!`:

```rust
let result = syscalls::spawn(&command).map_err(|e| anyhow::anyhow!(e))?;
```

## Exporting the plugin

The `export_tool!` macro generates the WIT component exports. Gate it on `wasm32` so the crate still compiles natively for tests:

```rust
#[cfg(target_arch = "wasm32")]
ein_plugin::export_tool!(MyTool);
```

---

## Worked example: a note-taking tool

This example implements a `Note` tool that appends text to a notes file. It demonstrates argument deserialization, file I/O through the WASI filesystem, and error handling.

```rust
use anyhow::anyhow;
use ein_plugin::tool::{ConstructableToolPlugin, ToolDef, ToolPlugin, ToolResult};
use serde::Deserialize;
use std::fs::OpenOptions;
use std::io::Write;

#[derive(Deserialize)]
struct NoteArgs {
    text: String,
    /// Path to the notes file. Must be within allowed_paths.
    file: String,
}

pub struct NoteTool;

impl ConstructableToolPlugin for NoteTool {
    fn new() -> Self {
        NoteTool
    }
}

impl ToolPlugin for NoteTool {
    fn name(&self) -> &str {
        "Note"
    }

    fn schema(&self) -> ToolDef {
        ToolDef::function(self.name(), "Append a note to a text file")
            .param("text", "string", "The text to append", true)
            .param("file", "string", "Absolute path to the notes file", true)
            .build()
    }

    fn primary_arg(&self) -> Option<&str> {
        Some("file")
    }

    fn call(&self, id: &str, args: &str) -> anyhow::Result<ToolResult> {
        let NoteArgs { text, file } = serde_json::from_str(args)?;

        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
            .map_err(|e| anyhow!("cannot open {file}: {e}"))?;

        writeln!(f, "{text}").map_err(|e| anyhow!("write failed: {e}"))?;

        Ok(ToolResult::new(id, format!("Appended to {file}")))
    }
}

#[cfg(target_arch = "wasm32")]
ein_plugin::export_tool!(NoteTool);
```

**How `primary_arg` works**: returning `Some("file")` means the UI displays `▸ Note  /path/to/notes.txt` next to the tool call indicator — the value of the `file` argument pulled from the LLM's call.

## Testing

Gate tests on the native target so they run with `cargo test` without needing WASM tooling:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_has_required_params() {
        let tool = NoteTool::new();
        assert_eq!(tool.name(), "Note");
        // Serialize and inspect the schema:
        let schema_json = serde_json::to_string(&tool.schema()).unwrap();
        assert!(schema_json.contains("\"required\":[\"text\",\"file\"]"));
    }
}
```

Run native tests normally:

```bash
cargo test
```

To test the WASM build compiles, add a CI step:

```bash
cargo build --target wasm32-wasip2
```

## Enabling streaming output

For long-running tools (like `Bash`), enable streaming so the client sees output incrementally rather than waiting for completion:

```rust
fn enable_chunk_sender(&self) -> bool {
    true
}
```

When streaming is enabled, the server wires up a `ToolOutputChunk` channel before calling `call`. The plugin writes to stdout normally — the server captures it and forwards chunks to the client in real time. No changes are needed in `call` itself.
