// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,

    /// TCP port for the gRPC server to listen on.
    #[arg(long, default_value = "50051")]
    port: u16,
}

#[derive(Subcommand)]
enum Commands {
    /// Download and install WASM plugins from the accompanying GitHub release.
    InstallPlugins {
        /// Plugin version to install (default: current binary version).
        #[arg(long)]
        version: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.command {
        Some(Commands::InstallPlugins { version }) => eind::install_plugins(version).await,
        None => eind::run(args.port).await,
    }
}
