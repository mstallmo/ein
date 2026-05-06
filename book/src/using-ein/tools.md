# Built-in Tools

Ein ships with four tool plugins that give the agent the ability to execute shell commands and work with files. Each tool call appears inline in the conversation pane as it runs.

## How tool calls appear

```
▸ Bash  ls -la src/
  total 40
  drwxr-xr-x  8 you  staff  256 May  6 11:30 .
  drwxr-xr-x  15 you staff  480 May  6 11:00 ..
  -rw-r--r--  1 you  staff  4096 May  6 10:45 main.rs
```

The `▸ ToolName` indicator (steel blue) shows the tool name and its primary argument. Output appears below, capped at 8 lines in the display (the full output is still sent to the model).

Tool calls are blocked by the `allowed_paths` and `allowed_hosts` sandbox — see [Security & Sandboxing](../configuration/security.md).

---

## Bash

Executes a shell command and returns its output.

```
▸ Bash  cargo test --lib
  running 12 tests
  test config::tests::build_config_openrouter_default_base_url ... ok
  ...
  test result: ok. 12 passed; 0 failed
```

**Streaming**: Bash output is streamed live as the command runs, so you see progress in real time without waiting for completion.

**Empty output**: if the command produces no output, the display shows `(no output)`.

The `command` parameter is passed as a shell string and executed via the host `spawn` syscall. Standard shell features (pipes, redirects, globbing) work as expected, subject to the plugin's `allowed_paths`.

---

## Read

Reads a file from the filesystem and returns its contents.

```
▸ Read  src/main.rs
```

The file content is sent to the model but is not displayed in full in the conversation pane (it would be too verbose). The tool call indicator shows the file path.

The file must be within the session's `allowed_paths`.

---

## Write

Writes content to a file, creating it if it doesn't exist or overwriting it if it does.

```
▸ Write  src/new_module.rs
```

The file path is shown in the indicator. The agent provides the full file contents; the write is atomic.

The target path must be within the session's `allowed_paths`.

---

## Edit

Replaces an exact string in a file with new content. More precise than Write for targeted changes.

```
▸ Edit  src/config.rs
  @@ -42,7 +42,7 @@
  -    let config_path = dirs::home_dir()
  -        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
  +    let config_path = dirs::home_dir()
  +        .ok_or_else(|| anyhow::anyhow!("Home directory not found"))?
```

The Edit tool renders a **syntax-highlighted diff** using the `base16-ocean.dark` theme. Up to 5 removed lines (red) and 5 added lines (green) are shown. For larger diffs the display is truncated but the full edit is applied.

The tool fails if the `old_string` is not found exactly in the file, or if it matches more than once. The agent handles retries.
