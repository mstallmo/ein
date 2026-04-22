// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

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
