# Building & Installing

## Compiling to WASM

Build your plugin for the `wasm32-wasip2` target:

```bash
# Release build (smaller, faster):
cargo build --release --target wasm32-wasip2

# Debug build (larger, with debug info):
cargo build --target wasm32-wasip2
```

The output is a `.wasm` file named after your crate:

```
target/wasm32-wasip2/release/my_tool.wasm
target/wasm32-wasip2/debug/my_tool.wasm
```

## Installing a tool plugin

Copy the `.wasm` file to `~/.ein/plugins/tools/`:

```bash
mkdir -p ~/.ein/plugins/tools
cp target/wasm32-wasip2/release/my_tool.wasm ~/.ein/plugins/tools/
```

## Installing a model client plugin

Copy to `~/.ein/plugins/model_clients/`:

```bash
mkdir -p ~/.ein/plugins/model_clients
cp target/wasm32-wasip2/release/my_model.wasm ~/.ein/plugins/model_clients/
```

## Applying changes

`eind` loads plugins at the start of a session. After copying a new or updated `.wasm` file, either stop and restart the server or [start a new session](../using-ein/sessions.md#creating-a-new-session):

```bash
# If running manually:
pkill eind && eind

# If running as a service (macOS):
launchctl kickstart -k gui/$(id -u)/com.mstallmo.eind

# If running as a service (Linux systemd):
systemctl --user restart eind
```

Then reconnect from the TUI — the new plugin appears automatically.

## Verifying installation

Open the plugin manager with `/plugins`. Your plugin should appear in the list with a ✓ mark. If it doesn't appear, check:

1. The file is in the right directory (`tools/` for tool plugins, `model_clients/` for model clients)
2. The file is a valid WASM component — try `wasm-tools validate my_tool.wasm` if you have it installed
3. `eind` was restarted after copying the file
4. Check the server log for load errors (run `eind` in a terminal to see stdout)

## Faster iteration during development

Restarting the server on every change is tedious. A few ways to speed up the cycle:

**Use a symlink** pointing from the install directory into your build output. The symlink itself never changes, so you only need to restart the server (not re-copy):

```bash
ln -sf "$(pwd)/target/wasm32-wasip2/debug/my_tool.wasm" \
    ~/.ein/plugins/tools/my_tool.wasm
```

Then iterate with:

```bash
cargo build --target wasm32-wasip2 && pkill -HUP eind
```

**Use native unit tests** for logic. The plugin traits are pure Rust — you can test argument parsing, schema generation, and output formatting with `cargo test` without any WASM involved. Only integration tests that need to run inside `eind` require the WASM build.

## A build script for multiple plugins

If you have several plugins, a small shell script keeps things tidy:

```bash
#!/usr/bin/env bash
set -e

TOOLS_DIR=~/.ein/plugins/tools

cargo build --release --target wasm32-wasip2

mkdir -p "$TOOLS_DIR"
cp target/wasm32-wasip2/release/my_tool.wasm "$TOOLS_DIR/"
cp target/wasm32-wasip2/release/my_other_tool.wasm "$TOOLS_DIR/"

echo "Plugins installed. Restart eind to apply."
```
