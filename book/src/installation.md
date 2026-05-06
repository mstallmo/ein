# Installation

## Prerequisites

Ein requires Rust and the `wasm32-wasip2` target (for building plugins from source). If you're installing a pre-built binary you only need the target if you plan to compile plugins yourself.

```bash
rustup target add wasm32-wasip2
```

## Installing Ein

### cargo binstall (recommended)

[`cargo binstall`](https://github.com/cargo-bins/cargo-binstall) downloads a pre-built binary for your platform:

```bash
cargo binstall ein
```

### Pre-built archives

Download a release archive from the [GitHub releases page](https://github.com/mstallmo/ein/releases), extract it, and place the `ein` binary somewhere on your `PATH`.

### Build from source

```bash
git clone https://github.com/mstallmo/ein
cd ein
cargo build --release -p ein -p eind
```

Binaries land in `target/release/`.

## Server startup

In release builds, `ein` automatically starts `eind` as a system service (macOS: launchd, Linux: systemd) on first launch. You don't need to manage the server manually.

For development or manual control:

```bash
# Terminal 1 — start the server
eind

# Terminal 2 — start the TUI
ein
```

The server listens on `localhost:50051` by default. Pass `--port` to change it:

```bash
eind --port 8080
```

## Connecting to a non-default server

```bash
ein http://my-server:50051
```

## Debug logging

Pass `--debug` to write verbose logs to `~/.ein/tui.log`:

```bash
ein --debug
```

## Plugin installation

In release builds Ein auto-installs the bundled WASM plugins on first launch. To install or update plugins manually:

```bash
eind install-plugins
# or a specific version:
eind install-plugins --version 0.2.0
```

Plugins are installed to `~/.ein/plugins/tools/` and `~/.ein/plugins/model_clients/`.
