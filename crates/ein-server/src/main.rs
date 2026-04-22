// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

//! Ein server binary.
//!
//! Starts a gRPC server that exposes the `Agent` service defined in
//! `ein-proto`. Clients (e.g. `ein-tui`) open a bidirectional streaming
//! session, stream user prompts in, and receive a sequence of `AgentEvent`
//! messages back as the agent thinks, invokes tools, and produces output.
//!
//! # Configuration
//!
//! | Variable | Description                         | Default |
//! |----------|-------------------------------------|---------|
//! | `--port` | TCP port the gRPC server listens on | `50051` |
//!
//! API credentials (`api_key`, `base_url`) are supplied by the client via
//! `SessionConfig` at connection time, read from `~/.ein/config.json`.
//!
//! # Plugin loading
//!
//! In debug builds, WASM plugins are loaded from `./target/wasm32-wasip2/debug/`.
//! In release builds tool plugins are loaded from `~/.ein/plugins/tools/` and
//! model client plugins from `~/.ein/plugins/model_clients/`.
//! Run `./scripts/build_install_plugins.sh` to compile and install them.

use clap::Parser;

#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    /// TCP port for the gRPC server to listen on.
    #[arg(long, default_value = "50051")]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ein_server::run(Args::parse().port).await
}
