# Contributing to Ein

Contributions are welcome. Please open an issue to discuss your idea before submitting a pull request.

## Building from source

Install Rust:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Add the WASM compile target:

```bash
rustup target add wasm32-wasip2
```

Build all crates:

```bash
cargo build
```

Build and install the WASM plugins (tool plugins and model client plugins):

```bash
./scripts/build_install_plugins.sh
```

> In debug builds, plugins are loaded automatically from `./target/wasm32-wasip2/debug/` — no installation needed. In release builds, tool plugins are installed to `~/.ein/plugins/tools/` and model client plugins to `~/.ein/plugins/model_clients/`.

## Running in development

Start the server in one terminal:

```bash
cargo run --bin eind
```

Start the TUI in another:

```bash
cargo run --bin ein
```

The TUI connects to `localhost:50051` by default. To connect to a different address:

```bash
cargo run --bin ein -- http://my-server:50051
```

To enable debug logging to `~/.ein/tui.log`:

```bash
cargo run --bin ein -- --debug
```

## Adding a tool plugin

1. Create a new package under `plugins/` implementing the `ToolPlugin` trait from `plugins/ein_tool/`
2. Declare `name()`, `description()`, and a JSON parameter schema via `ToolDef`
3. Add the package to `scripts/build_install_plugins.sh`
4. Rebuild — the server picks it up automatically on next start

## Adding a model client plugin

1. Implement the `ModelClient` WIT interface defined at `wit/model_client/`
2. Add the package to `scripts/build_install_plugins.sh`
3. Rebuild

The plugin's filename stem (e.g. `ein_openrouter`) is used as its config identity in `plugin_configs`.

## Project structure

See [CLAUDE.md](CLAUDE.md) for a detailed walkthrough of every source module.

## Releasing

Releases are fully automated via CI using [cargo-dist](https://axodotdev.github.io/cargo-dist/).

**1. Bump the version**

Edit `version` in `[workspace.package]` in `Cargo.toml`. All crates inherit this value.

**2. Commit and tag**

```bash
git commit -am "chore: bump version to vX.Y.Z"
git tag vX.Y.Z
git push origin main --tags
```

**3. CI publishes the release**

Pushing a tag matching `vX.Y.Z` triggers `.github/workflows/release.yml`, which builds multi-platform binaries (macOS arm64/x86, Linux arm64/x86, Windows x86) and publishes a GitHub Release with archives and checksums.

Once the release is live, `cargo binstall --git https://github.com/mstallmo/ein ein` will resolve the new binaries automatically — no crates.io publish required.

> **Note:** `release.yml` contains a manually-added `protoc` install step. If you ever regenerate the workflow with `cargo dist generate`, preserve that step — it is required for the proto compilation during the build.
