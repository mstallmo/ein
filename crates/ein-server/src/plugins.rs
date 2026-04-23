// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use std::{io, path::Path};
use tar::Archive;
use tokio::{fs, task};

pub async fn install_plugins(version: Option<String>) -> Result<()> {
    let ver = version.unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    let ver = ver.trim_start_matches('v');
    let tag = format!("v{ver}");
    let url =
        format!("https://github.com/mstallmo/ein/releases/download/{tag}/ein-plugins-{tag}.tar.gz");

    let plugins_dir = dirs::home_dir()
        .context("Failed to find home directory")?
        .join(".ein")
        .join("plugins");

    fs::create_dir_all(plugins_dir.join("tools")).await?;
    fs::create_dir_all(plugins_dir.join("model_clients")).await?;

    println!("Downloading plugins from {url}...");

    let response = reqwest::get(&url)
        .await
        .with_context(|| format!("Failed to download {url}"))?;

    if !response.status().is_success() {
        anyhow::bail!("Download failed: HTTP {}", response.status());
    }

    let bytes = response
        .bytes()
        .await
        .context("Failed to read response body")?;

    task::spawn_blocking(move || {
        let gz = GzDecoder::new(io::Cursor::new(bytes));
        Archive::new(gz)
            .unpack(&plugins_dir)
            .context("Failed to extract plugin archive")
    })
    .await??;

    println!("Plugins installed successfully");
    Ok(())
}

/// Returns true if the tools plugin directory has no `.wasm` files.
pub async fn plugins_missing(plugin_dir: &Path) -> bool {
    let Ok(mut entries) = fs::read_dir(plugin_dir).await else {
        return true;
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        if entry.path().extension().is_some_and(|e| e == "wasm") {
            return false;
        }
    }

    true
}
