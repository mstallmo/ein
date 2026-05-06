# Configuration

Ein's configuration lives in a single JSON file at `~/.ein/config.json`. It is created automatically on first launch and can be edited with `/config` (opens in `$EDITOR`, defaulting to `nano`).

The file is **watched for changes**: if you edit it while Ein is running, the new configuration is sent to the server immediately without restarting.

## File structure

```json
{
  "model_client_name": "ein_openrouter",
  "allowed_paths": [],
  "allowed_hosts": [],
  "plugin_configs": {
    "ein_openrouter": {
      "params": {
        "api_key": "sk-or-...",
        "base_url": "https://openrouter.ai/api/v1",
        "model": "anthropic/claude-sonnet-4-5"
      }
    }
  }
}
```

**Top-level fields:**

| Field | Type | Description |
|-------|------|-------------|
| `model_client_name` | string | Plugin name for the active model provider (e.g. `"ein_openrouter"`). If empty, the server picks the first available plugin. |
| `allowed_paths` | array of strings | Filesystem paths all WASM plugins may access. Session-scoped; not updated mid-session by config changes. |
| `allowed_hosts` | array of strings | Network hostnames all WASM plugins may contact. Session-scoped. |
| `plugin_configs` | object | Per-plugin configuration, keyed by plugin name. |

**Per-plugin config** (under `plugin_configs.<name>`):

| Field | Type | Description |
|-------|------|-------------|
| `params` | object | Plugin-specific parameters (api key, model, base URL, etc.). Values may be strings, numbers, or booleans. |
| `allowed_paths` | array | Additional paths for this plugin only, merged with the global list. |
| `allowed_hosts` | array | Additional hosts for this plugin only, merged with the global list. |

## Sections

- [Providers](providers.md) — how to configure OpenRouter, Anthropic, OpenAI, and Ollama
- [Security & Sandboxing](security.md) — how `allowed_paths` and `allowed_hosts` work
