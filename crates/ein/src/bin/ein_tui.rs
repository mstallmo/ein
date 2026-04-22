// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use clap::Parser;
use ein_tui::Args;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    ein_tui::run(Args::parse()).await
}
