// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use clap::Parser;
use ein::Args;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ein::run(Args::parse()).await
}
