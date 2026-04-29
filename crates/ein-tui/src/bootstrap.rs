// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

//! Bootstrap logic: downloads `ein-server` on first run and registers it as a
//! system service (macOS LaunchAgent or Linux systemd user service).

// These items are only called from the #[cfg(not(debug_assertions))] block in
// lib.rs, so they appear unused in debug builds. That's intentional.
#![cfg_attr(debug_assertions, allow(dead_code))]

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use std::{
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use tar::Archive;
use tokio::{fs, process::Command, task};

const GITHUB_REPO: &str = "mstallmo/ein";

/// Path where `ein` installs the server binary: `~/.ein/bin/ein-server`.
pub fn server_bin_path() -> PathBuf {
    dirs::home_dir()
        .expect("home directory not found")
        .join(".ein")
        .join("bin")
        .join("ein-server")
}

/// Compile-time target triple used to select the right GitHub release asset.
pub fn target_triple() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return "aarch64-apple-darwin";
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return "x86_64-apple-darwin";
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return "aarch64-unknown-linux-gnu";
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return "x86_64-unknown-linux-gnu";
    #[allow(unreachable_code)]
    ""
}

/// Downloads the `ein-server` binary for the current platform from GitHub
/// releases and writes it to `~/.ein/bin/ein-server` with executable permissions.
pub async fn download_server(version: &str) -> Result<()> {
    let ver = version.trim_start_matches('v');
    let tag = format!("v{ver}");
    let triple = target_triple();
    let archive_name = format!("ein-server-{tag}-{triple}.tar.gz");
    let url = format!("https://github.com/{GITHUB_REPO}/releases/download/{tag}/{archive_name}");

    let dest = server_bin_path();
    fs::create_dir_all(dest.parent().unwrap())
        .await
        .context("failed to create ~/.ein/bin")?;

    println!("Downloading {archive_name}...");

    let response = reqwest::get(&url)
        .await
        .with_context(|| format!("failed to fetch {url}"))?;

    if !response.status().is_success() {
        anyhow::bail!("download failed: HTTP {}", response.status());
    }

    let bytes = response
        .bytes()
        .await
        .context("failed to read response body")?;

    let dest_clone = dest.clone();
    task::spawn_blocking(move || extract_server(&bytes, &dest_clone))
        .await
        .context("extraction task panicked")??;

    // Make the binary executable.
    let mut perms = fs::metadata(&dest).await?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&dest, perms).await?;

    println!("ein-server installed to {}", dest.display());
    Ok(())
}

/// Extracts the `ein-server` binary from a tar.gz archive into `dest`.
fn extract_server(bytes: &[u8], dest: &Path) -> Result<()> {
    let gz = GzDecoder::new(io::Cursor::new(bytes));
    let mut archive = Archive::new(gz);

    for entry in archive
        .entries()
        .context("failed to read archive entries")?
    {
        let mut entry = entry.context("corrupt archive entry")?;
        let entry_path = entry.path().context("entry has no path")?;

        // The archive contains exactly one file: the `ein-server` binary.
        // Accept it regardless of any leading directory component.
        let file_name = entry_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");

        if file_name == "ein-server" {
            let mut file = std::fs::File::create(dest)
                .with_context(|| format!("failed to create {}", dest.display()))?;
            io::copy(&mut entry, &mut file).context("failed to write ein-server")?;
            return Ok(());
        }
    }

    anyhow::bail!("ein-server binary not found in archive")
}

// ---------------------------------------------------------------------------
// Service registration
// ---------------------------------------------------------------------------

/// Ensures `ein-server` is registered as a system service.
///
/// On macOS, installs a LaunchAgent plist and loads it.
/// On Linux, writes a systemd user unit and enables it.
/// On other platforms, does nothing (the TUI's retry loop handles reconnects).
pub async fn ensure_service_installed() -> Result<()> {
    #[cfg(target_os = "macos")]
    return ensure_launchagent_installed().await;

    #[cfg(target_os = "linux")]
    return ensure_systemd_installed().await;

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Ok(())
}

// ---------------------------------------------------------------------------
// macOS LaunchAgent
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
const LAUNCH_AGENT_LABEL: &str = "com.ein.server";

#[cfg(target_os = "macos")]
fn launchagent_plist_path() -> PathBuf {
    dirs::home_dir()
        .expect("home directory not found")
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LAUNCH_AGENT_LABEL}.plist"))
}

#[cfg(target_os = "macos")]
async fn ensure_launchagent_installed() -> Result<()> {
    // Check if already loaded.
    let status = Command::new("launchctl")
        .args(["list", LAUNCH_AGENT_LABEL])
        .output()
        .await
        .context("launchctl not found")?;

    if status.status.success() {
        return Ok(()); // Already running.
    }

    let plist_path = launchagent_plist_path();
    let bin = server_bin_path();
    let log = dirs::home_dir()
        .expect("home directory not found")
        .join(".ein")
        .join("server.log");

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCH_AGENT_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        LAUNCH_AGENT_LABEL = LAUNCH_AGENT_LABEL,
        bin = bin.display(),
        log = log.display(),
    );

    fs::create_dir_all(plist_path.parent().unwrap())
        .await
        .context("failed to create LaunchAgents directory")?;
    fs::write(&plist_path, plist)
        .await
        .context("failed to write plist")?;

    let output = Command::new("launchctl")
        .args(["load", plist_path.to_str().unwrap()])
        .output()
        .await
        .context("failed to run launchctl load")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("launchctl load failed: {stderr}");
    }

    println!("ein-server registered as LaunchAgent ({LAUNCH_AGENT_LABEL})");
    Ok(())
}

// ---------------------------------------------------------------------------
// Linux systemd user service
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
const SYSTEMD_SERVICE_NAME: &str = "ein-server";

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> PathBuf {
    dirs::home_dir()
        .expect("home directory not found")
        .join(".config")
        .join("systemd")
        .join("user")
        .join(format!("{SYSTEMD_SERVICE_NAME}.service"))
}

#[cfg(target_os = "linux")]
async fn ensure_systemd_installed() -> Result<()> {
    // Check if already enabled.
    let status = Command::new("systemctl")
        .args(["--user", "is-enabled", SYSTEMD_SERVICE_NAME])
        .output()
        .await
        .context("systemctl not found")?;

    if status.status.success() {
        return Ok(()); // Already enabled.
    }

    let unit_path = systemd_unit_path();
    let bin = server_bin_path();

    let unit = format!(
        "[Unit]\nDescription=Ein server\n\n[Service]\nExecStart={bin}\nRestart=always\n\n[Install]\nWantedBy=default.target\n",
        bin = bin.display(),
    );

    fs::create_dir_all(unit_path.parent().unwrap())
        .await
        .context("failed to create systemd user directory")?;
    fs::write(&unit_path, unit)
        .await
        .context("failed to write systemd unit")?;

    let reload = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()
        .await
        .context("failed to run systemctl daemon-reload")?;

    if !reload.status.success() {
        let stderr = String::from_utf8_lossy(&reload.stderr);
        anyhow::bail!("systemctl daemon-reload failed: {stderr}");
    }

    let enable = Command::new("systemctl")
        .args(["--user", "enable", "--now", SYSTEMD_SERVICE_NAME])
        .output()
        .await
        .context("failed to run systemctl enable")?;

    if !enable.status.success() {
        let stderr = String::from_utf8_lossy(&enable.stderr);
        anyhow::bail!("systemctl enable failed: {stderr}");
    }

    println!("ein-server registered as systemd user service ({SYSTEMD_SERVICE_NAME})");
    Ok(())
}
