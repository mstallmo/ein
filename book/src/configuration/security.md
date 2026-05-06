# Security & Sandboxing

All WASM plugins in Ein run inside a Wasmtime sandbox. Two allowlists govern what each plugin can do: `allowed_paths` for filesystem access and `allowed_hosts` for network access. Plugins cannot reach anything outside these lists.

## Filesystem access: `allowed_paths`

`allowed_paths` is a list of absolute filesystem paths that plugins may read and write. Any path not in this list is invisible to the plugin — attempts to open it return a permission error.

```json
{
  "allowed_paths": [
    "/Users/you/myproject",
    "/tmp"
  ]
}
```

**Session-scoped**: `allowed_paths` at the top level of the config is read when a session starts and does not change mid-session even if you edit the config file. If you need a different set of paths for a new task, start a new session with `/new`.

**Per-plugin overrides**: individual plugins can have their own `allowed_paths` under `plugin_configs.<name>.allowed_paths`. These are merged with the global list — a plugin sees the union of both.

```json
{
  "allowed_paths": ["/Users/you/projects"],
  "plugin_configs": {
    "ein_bash": {
      "allowed_paths": ["/tmp/scratch"]
    }
  }
}
```

**CWD prompt**: when you start a new session, Ein asks whether to add the current working directory to `allowed_paths` for that session. This is ephemeral — it is not written to `~/.ein/config.json`. If you want a directory available in all sessions, add it to the config file.

### Recommendations

- Add only the directories you're actively working in, not your entire home directory.
- Add `/tmp` if you need tools to create temporary files.
- Avoid adding directories containing credentials (SSH keys, `.env` files) unless the agent specifically needs them.

## Network access: `allowed_hosts`

`allowed_hosts` is a list of hostnames that plugins may contact. Outbound HTTP requests to any host not in this list are blocked.

```json
{
  "allowed_hosts": ["api.github.com", "registry.npmjs.org"]
}
```

**Special values:**
- Empty list (default): no outbound connections allowed for tool plugins
- `"*"`: allow all outbound connections (use with caution)

**Model client plugins** are handled differently: the hostname from their configured `base_url` is automatically allowlisted. For example, if `ein_openrouter` has `base_url = "https://openrouter.ai/api/v1"`, then `openrouter.ai` is automatically reachable. You do not need to add it to `allowed_hosts`.

To allow a model client plugin to contact additional hosts:

```json
{
  "plugin_configs": {
    "ein_openrouter": {
      "allowed_hosts": ["cdn.extra-resource.com"]
    }
  }
}
```

**Like `allowed_paths`**, `allowed_hosts` is session-scoped and merged per-plugin.

## Why sandboxing matters

The agent loop runs LLM-generated tool calls on your machine. Sandboxing limits the blast radius of a misbehaving model or a malicious prompt injection:

- A malicious tool plugin or prompt cannot read your SSH keys or `.aws/credentials`.
- A malicious tool plugin or prompt cannot exfiltrate data to arbitrary servers.
- WASM's memory isolation means a buggy plugin cannot corrupt the `eind` process or other plugins.

The sandboxing is defense-in-depth, not a hard security boundary. Do not grant broader access than the task requires.
