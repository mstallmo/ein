# Memory System Spec

Inspired by [Letta's memory architecture](https://docs.letta.com/guides/agents/memory/).

## Overview

Ein agents currently have no persistent memory — each session starts with a blank slate. This spec adds a two-tier memory system that persists state across sessions:

1. **Core memory** — a small set of named text blocks always visible in the system prompt (in-context)
2. **Archival memory** — an unlimited append-only store queried on demand via semantic/keyword search (out-of-context)

Both tiers are implemented as WASM tool plugins so the agent can read and write its own memory using the existing tool-call mechanism.

---

## Goals

- Agents remember facts across sessions without user repetition
- Memory is agent-scoped and stored on disk under `~/.ein/memory/<agent-id>/`
- Adding memory support does not require changes to the core agent loop
- Memory tools are optional; sessions without them behave exactly as today

---

## Memory Tiers

### Core Memory (In-Context)

Core memory blocks are short, named text sections prepended to the system prompt on every LLM call. Because they are always in-context, the model can read them without a tool call.

**Properties per block:**

| Field | Type | Description |
|-------|------|-------------|
| `label` | `string` | Unique identifier (e.g. `"human"`, `"persona"`) |
| `description` | `string` | Guidance for the agent on what to store here |
| `value` | `string` | Current content |
| `char_limit` | `u32` | Max characters for `value` (default: 2000) |
| `read_only` | `bool` | If true, agent cannot modify this block |

**Default blocks:**

| Label | Description | Default value |
|-------|-------------|---------------|
| `persona` | The agent's personality and working style | Empty |
| `human` | Facts about the user — name, preferences, context | Empty |

Additional blocks can be added via config.

**System prompt injection:**

Core memory blocks are rendered into the system prompt as XML-like sections before any other content:

```
<memory>
  <persona>
You are a thoughtful, concise assistant. You prefer direct answers.
  </persona>
  <human>
Name: Mason. Senior software engineer. Working on a Rust agent framework called Ein.
  </human>
</memory>
```

The existing system message (listing preopened paths) follows after.

### Archival Memory (Out-of-Context)

Archival memory is an append-only log of text passages. Entries are not injected into the system prompt; the agent retrieves them explicitly via tool call when needed.

**Properties per passage:**

| Field | Type | Description |
|-------|------|-------------|
| `id` | `u64` | Monotonically increasing identifier |
| `created_at` | `u64` | Unix timestamp |
| `text` | `string` | Content of the passage |
| `tags` | `Vec<string>` | Optional labels for filtering |

**Search:** Uses BM25 full-text ranking via the [`bm25`](https://crates.io/crates/bm25) crate. BM25 handles stemming and term-frequency weighting, so a query like `"communication style"` will match a passage like `"prefers concise, direct responses"` where substring matching would not. True semantic search (e.g. matching `"artificial memories"` to `"implanted memories"`) requires vector embeddings and is addressed in the [Archival Search](#archival-search) section below.

---

## On-Disk Format

All memory files live under `~/.ein/memory/<agent-id>/`.

For sessions that do not specify an `agent_id`, a default agent ID of `"default"` is used.

```
~/.ein/memory/
  default/
    core.json         # Core memory blocks
    archival.jsonl    # Archival passages, one JSON object per line
```

### `core.json`

```json
{
  "blocks": [
    {
      "label": "persona",
      "description": "The agent's personality and working style",
      "value": "You are a thoughtful, concise assistant.",
      "char_limit": 2000,
      "read_only": false
    },
    {
      "label": "human",
      "description": "Facts about the user — name, preferences, ongoing projects",
      "value": "",
      "char_limit": 2000,
      "read_only": false
    }
  ]
}
```

If the file does not exist, it is created with the two default blocks on first load.

### `archival.jsonl`

```jsonl
{"id":1,"created_at":1743000000,"text":"Mason prefers terse responses with no trailing summaries.","tags":["preference"],"embedding":null}
{"id":2,"created_at":1743001234,"text":"The Ein project uses Wasmtime for plugin sandboxing.","tags":["project"],"embedding":null}
```

New passages are appended. Entries are never mutated or deleted in-place (archival-only semantics). The file is created empty if it does not exist. The `embedding` field is `null` in v1 and reserved for a future vector search upgrade.

---

## Memory Tools (WASM Plugins)

Four new tools are added as WASM plugins under `plugins/`:

### `ein_core_memory`

Provides two tools:

#### `core_memory_append`

Appends text to the end of a core memory block.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `label` | string | yes | Block to append to |
| `content` | string | yes | Text to append |

Returns: the updated block value, or an error if the label does not exist, the block is read-only, or the append would exceed `char_limit`.

#### `core_memory_replace`

Replaces the full contents of a core memory block.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `label` | string | yes | Block to replace |
| `content` | string | yes | New value (empty string clears the block) |

Returns: the updated block value, or an error if the label does not exist, the block is read-only, or `content` exceeds `char_limit`.

### `ein_archival_memory`

Provides two tools:

#### `archival_memory_insert`

Adds a new passage to archival memory.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `text` | string | yes | Content to store |
| `tags` | array of strings | no | Labels for filtering |

Returns: the ID assigned to the new passage.

#### `archival_memory_search`

Searches archival memory and returns matching passages.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `query` | string | yes | Search string |
| `page` | integer | no | 0-indexed page (default: 0) |
| `page_size` | integer | no | Results per page (default: 10) |

Returns: a JSON array of matching passages (id, created_at, text, tags), ranked by BM25 score.

### Archival Search

BM25 covers most practical retrieval needs but does not do semantic matching across vocabulary gaps. The table below summarises the options considered:

| Approach | Semantic match | External dependency | Verdict |
|----------|---------------|---------------------|---------|
| Substring | No | None | Too weak — rejects obvious paraphrases |
| BM25 | Partial (term overlap) | `bm25` crate (pure Rust) | **Chosen for v1** |
| Embedding API (e.g. OpenRouter) | Yes | Embedding endpoint + `allowed_hosts` | Future upgrade path |
| Local embedding model (WASM) | Yes | Bundled model weights (~100 MB+) | Possible but heavy |

**V1 uses BM25.** The `archival.jsonl` format includes an optional `embedding` field (null in v1) so that a future migration to vector search can populate embeddings for existing passages without changing the storage schema.

When vector search is added, the `ein_archival_memory` plugin config will accept `embedding_base_url` and `embedding_model` keys (mirroring how `ein_openrouter` accepts `base_url` and `model`), and the plugin's `allowed_hosts` will be set accordingly. The tool interface (`archival_memory_insert` / `archival_memory_search`) does not change.

---

## Session Integration

### `SessionConfig` changes

Two new optional fields are added to `SessionConfig` (proto and Rust):

| Field | Type | Description |
|-------|------|-------------|
| `agent_id` | `string` | Memory namespace. Defaults to `"default"` if empty. |
| `memory_enabled` | `bool` | Whether to load memory tools and inject core memory. Defaults to `false`. |

The TUI does not need to set these fields; they default to off for backward compatibility.

### Server startup with memory enabled

When `memory_enabled = true` in the received `SessionConfig`:

1. Load `~/.ein/memory/<agent-id>/core.json` (create with defaults if absent)
2. Render core blocks into a `<memory>…</memory>` XML string
3. Prepend that string to the system message that is injected at the start of `messages`
4. Load `ein_core_memory.wasm` and `ein_archival_memory.wasm` into the tool registry, passing `agent_id` as a config parameter
5. Pass `agent_id` to each memory plugin via `plugin_configs["ein_core_memory"].config["agent_id"]` and `plugin_configs["ein_archival_memory"].config["agent_id"]`

When `memory_enabled = false` (default), none of the above happens and behaviour is identical to the current implementation.

### Memory plugin WASI context

Memory plugins use the standard preopened-directory mechanism, the same as `ein_read`/`ein_write`. The server sets each plugin's `allowed_paths` to include `~/.ein/memory/<agent-id>/` (the resolved absolute path), and the path is also passed as `plugin_configs["ein_core_memory"].config["memory_dir"]` so the plugin knows which directory to open. `allowed_hosts` is `[]` (no network access in v1). No new syscalls or WIT interfaces are required.

Atomic writes (to avoid partial reads of `core.json` mid-update) are handled inside the plugin: write to `core.json.tmp`, then rename to `core.json` using standard WASI filesystem calls. The server does not need to participate.

### Core memory refresh

After any tool call that returns successfully from `ein_core_memory`, the server re-reads `core.json` and updates the system message at index 0 of `messages`. This ensures subsequent LLM calls see the updated core memory without requiring a new session.

---

## TUI Changes

### Config additions

`ClientConfig` gains two optional fields mirroring the proto additions:

```rust
pub struct ClientConfig {
    // ... existing fields ...
    pub agent_id: Option<String>,    // default: None → server uses "default"
    pub memory_enabled: bool,        // default: false
}
```

Both are persisted to `~/.ein/config.json` and included in `SessionConfig` at init time.

### Memory status in status bar

When `memory_enabled = true`, a small `[M]` indicator is appended to the status bar between the model name and token usage. No other TUI changes are required.

---

## Example Interaction

```
User:  My name is Mason and I prefer short answers.

Agent: Got it!
       [calls core_memory_append(label="human", content="Name: Mason. Prefers short answers.")]
       I'll remember that.

--- new session ---

User:  What do you know about me?

Agent: You're Mason and you prefer short answers.
```

The second session sees the `human` block already populated because `core.json` was written in the first session.

---

## Non-Goals (out of scope for this spec)

- Vector embedding search (BM25 chosen for v1; embedding upgrade path is documented above)
- Multi-agent shared memory blocks
- Memory block versioning or rollback
- Conversation history search (separate from archival memory)
- A `/memory` slash command in the TUI (can be added later)
- Automatic memory consolidation or summarization ("sleeptime" in Letta)

---

## Implementation Order

1. Create `plugins/ein_core_memory/` — preopened-dir file I/O + two tools
2. Create `plugins/ein_archival_memory/` — preopened-dir file I/O + BM25 search + two tools
3. Add `agent_id` and `memory_enabled` to proto + server `SessionConfig` handling
4. Add core memory injection to `grpc.rs` session init (preopen memory dir, pass `memory_dir` config)
5. Add post-tool core memory refresh in `agent.rs`
6. Add `agent_id` / `memory_enabled` to `ClientConfig` and TUI `[M]` indicator
7. Add both plugins to `build_install_plugins.sh`
