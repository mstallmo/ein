// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

//! Ein TUI library.
//!
//! Exposes [`run`] so both the standalone `ein-tui` binary and the `ein`
//! meta-package binary can share the same entry-point without duplicating code.

mod app;
mod bootstrap;
mod config;
mod connection;
mod input;
mod render;

use crate::app::{
    App, AppEvent, ConnectionStatus, CwdState, DisplayMessage, Modal, PluginModalState,
    SessionPickerState, SetupWizardState, UninstallPhase,
};
use crate::config::load_or_create_config;
use crate::connection::{
    check_plugins, connection_manager, delete_session, install_plugins, spawn_config_watcher,
    to_proto_session_config,
};
use crate::input::{KeyAction, handle_key_event, handle_server_event};
use crate::render::render;
use crossterm::{
    event::{Event, EventStream, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ein_proto::ein::{SessionConfig, UserInput, user_input};
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tracing::info;

// ---------------------------------------------------------------------------
// CLI arguments
// ---------------------------------------------------------------------------

#[derive(clap::Parser)]
#[command(about = "Ein terminal UI")]
pub struct Args {
    /// gRPC server address
    #[arg(default_value = "http://localhost:50051")]
    server_addr: String,

    /// Write debug logs to ~/.ein/tui.log
    #[arg(long)]
    debug: bool,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Derives a short model name for the status bar from the client config.
///
/// Strips the vendor prefix (e.g. "anthropic/claude-haiku-4.5" → "claude-haiku-4.5").
/// Falls back to a placeholder when no model is configured.
pub fn model_display_from_config(cfg: &config::ClientConfig) -> String {
    let model_full = cfg
        .plugin_configs
        .get(&cfg.model_client_name)
        .and_then(|pc| pc.params.get("model"))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| "unknown".to_string());
    model_full
        .split_once('/')
        .map(|(_, m)| m.to_string())
        .unwrap_or(model_full)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the Ein TUI. Parses CLI arguments and blocks until the user exits.
pub async fn run(args: Args) -> anyhow::Result<()> {
    // Initialize the file-based tracing subscriber when --debug is passed.
    // Must happen before enable_raw_mode() takes over the terminal.
    // The guard is held for the lifetime of run() to flush the non-blocking writer.
    let _tracing_guard = if args.debug {
        let log_dir = dirs::home_dir().unwrap_or_default().join(".ein");
        let file_appender = tracing_appender::rolling::never(&log_dir, "tui.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        tracing_subscriber::fmt()
            .with_writer(non_blocking)
            .with_ansi(false)
            .with_target(false)
            .init();
        Some(guard)
    } else {
        None
    };

    info!(server_addr = %args.server_addr, "ein-tui starting");

    // In release builds: download ein-server if absent, then register it as a
    // system service. Runs before raw mode so stdout is visible for progress.
    #[cfg(not(debug_assertions))]
    {
        let bin = bootstrap::server_bin_path();
        if !bin.exists() {
            println!("Downloading ein-server {}...", env!("CARGO_PKG_VERSION"));
            bootstrap::download_server(env!("CARGO_PKG_VERSION")).await?;
        }
        bootstrap::ensure_service_installed().await?;
    }

    // Load (or create) the client config before opening the gRPC session.
    let cfg = load_or_create_config()?;
    let first_run = config::is_first_run(&cfg);

    // Derive a short model name for the status bar by stripping the vendor
    // prefix (e.g. "anthropic/claude-haiku-4.5" → "claude-haiku-4.5").
    let model_display = model_display_from_config(&cfg);

    // Capture the cwd for the "New Session" CWD modal and the welcome header.
    let cwd_str = std::env::current_dir()
        .ok()
        .map(|p| p.display().to_string());
    let cwd_display = cwd_str.clone().unwrap_or_else(|| "unknown".to_string());

    let (event_tx, mut event_rx) = mpsc::channel::<AppEvent>(64);

    // Watch ~/.ein/config.json for changes and send ConfigChanged events.
    spawn_config_watcher(event_tx.clone());

    // Cache for the chosen SessionConfig; shared with the connection manager so
    // reconnects reuse the same config without reshowing the session picker.
    let session_config_cache: std::sync::Arc<tokio::sync::Mutex<Option<SessionConfig>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(None));

    // Shared notify used by /new to skip the connection manager's 3 s retry delay.
    let reconnect_notify = std::sync::Arc::new(tokio::sync::Notify::new());

    // Spawn the connection manager immediately — the session picker is shown
    // as part of the first connection handshake, not before it.
    tokio::spawn(connection_manager(
        args.server_addr.clone(),
        event_tx.clone(),
        session_config_cache.clone(),
        reconnect_notify.clone(),
    ));

    // Configure the terminal for raw / alternate-screen rendering.
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(model_display, cwd_str, cwd_display, cfg.clone());

    if first_run {
        app.messages.push(DisplayMessage::SetupPrompt);
        app.modal = Some(Modal::SetupWizard(SetupWizardState::new()));
    }

    let mut term_events = EventStream::new();
    // Ticker drives the thinking spinner; only app.tick is incremented when
    // the agent is busy, so the timer is cheap when idle.
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(80));

    loop {
        terminal.draw(|f| render(&app, f))?;

        tokio::select! {
            _ = ticker.tick() => {
                let plugin_busy = app.modal
                    .as_ref()
                    .is_some_and(|m| {
                        match m {
                            Modal::PluginManager(m) => {
                                m.loading || m.installing
                            },
                            _ => false
                        }
                    });

                if app.agent_busy
                    || matches!(app.connection_status, ConnectionStatus::Connecting)
                    || plugin_busy
                {
                    app.tick = app.tick.wrapping_add(1);
                }
            }

            Some(Ok(event)) = term_events.next() => {
                let Event::Key(key) = event else { continue };
                if key.kind != KeyEventKind::Press { continue; }

                match handle_key_event(&mut app, key).await {
                    KeyAction::Quit => break,
                    KeyAction::OpenConfig(path) => {
                        let editor =
                            std::env::var("EDITOR").unwrap_or_else(|_| "nano".to_string());
                        disable_raw_mode()?;
                        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                        let _ = std::process::Command::new(&editor).arg(&path).status();
                        enable_raw_mode()?;
                        execute!(terminal.backend_mut(), EnterAlternateScreen)?;
                        terminal.clear()?;
                    }
                    KeyAction::NewSession => {
                        // Build a fresh config from the current ~/.ein/config.json — the
                        // same source "New Session" in the picker uses — rather than
                        // recycling the cached SessionConfig from the old session.
                        let base = to_proto_session_config(&app.current_cfg, String::new());

                        // Drop the sender → closes the gRPC request stream → server
                        // receives EOF and closes the response stream → try_connect returns.
                        app.prompt_tx = None;
                        app.connection_status = ConnectionStatus::Connecting;
                        app.session_id = None;
                        app.agent_busy = false;
                        app.connection_error = None;

                        // Clear the conversation display, keeping the welcome banner.
                        app.messages.retain(|m| matches!(m, DisplayMessage::Header { .. }));
                        app.scroll_offset = 0;
                        app.auto_scroll = true;

                        if let Some(cwd) = app.cwd.clone() {
                            // Show the CWD modal — identical to the "New Session" picker
                            // path. A bridge task receives the final config from the modal
                            // and updates the cache before signalling the connection manager.
                            let (tx, rx) = tokio::sync::oneshot::channel::<SessionConfig>();
                            app.modal = Some(Modal::CwdPrompt(
                                CwdState {
                                    cwd,
                                    base_config: base,
                                    session_tx: tx,
                                }
                            ));
                            let cache = session_config_cache.clone();
                            let notify = reconnect_notify.clone();
                            tokio::spawn(async move {
                                if let Ok(cfg) = rx.await {
                                    *cache.lock().await = Some(cfg);
                                    notify.notify_one();
                                }
                            });
                        } else {
                            // No CWD to ask about — update the cache and reconnect now.
                            *session_config_cache.lock().await = Some(base);
                            reconnect_notify.notify_one();
                        }
                    }
                    KeyAction::OpenSessionPicker => {
                        app.prompt_tx = None;
                        app.connection_status = ConnectionStatus::Connecting;
                        app.session_id = None;
                        app.agent_busy = false;
                        app.connection_error = None;
                        app.messages.retain(|m| matches!(m, DisplayMessage::Header { .. }));
                        app.scroll_offset = 0;
                        app.auto_scroll = true;
                        // Clear the cache so try_connect shows the session picker on reconnect.
                        *session_config_cache.lock().await = None;
                        reconnect_notify.notify_one();
                    }
                    KeyAction::DeleteSession(session_id) => {
                        let addr = args.server_addr.clone();
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            if delete_session(&addr, session_id.clone()).await.is_ok() {
                                let _ = tx.send(AppEvent::SessionDeleted(session_id)).await;
                            }
                        });
                    }
                    KeyAction::OpenPluginModal => {
                        app.modal = Some(Modal::PluginManager(PluginModalState {
                            sources: vec![],
                            selected: 0,
                            installing: false,
                            loading: true,
                            status_message: None,

                        }));
                        let addr = args.server_addr.clone();
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            let sources = check_plugins(&addr).await.unwrap_or_default();
                            let _ = tx.send(AppEvent::PluginStatusLoaded(sources)).await;
                        });
                    }
                    KeyAction::InstallPlugin { source_id } => {
                        let addr = args.server_addr.clone();
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            match install_plugins(&addr, source_id).await {
                                Ok(resp) => {
                                    let _ = tx
                                        .send(AppEvent::PluginInstallResult {
                                            success: resp.success,
                                            message: resp.message,
                                        })
                                        .await;
                                }
                                Err(e) => {
                                    let _ = tx
                                        .send(AppEvent::PluginInstallResult {
                                            success: false,
                                            message: e.to_string(),
                                        })
                                        .await;
                                }
                            }
                        });
                    }
                    KeyAction::OpenSetupWizard => {
                        app.modal = Some(Modal::SetupWizard(SetupWizardState::new()));
                    }
                    KeyAction::RunUninstall => {
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            #[cfg(not(debug_assertions))]
                            {
                                match bootstrap::uninstall().await {
                                    Ok(steps) => {
                                        let _ = tx
                                            .send(AppEvent::UninstallComplete {
                                                success: true,
                                                steps,
                                            })
                                            .await;
                                    }
                                    Err(e) => {
                                        let _ = tx
                                            .send(AppEvent::UninstallComplete {
                                                success: false,
                                                steps: vec![format!("Error: {e}")],
                                            })
                                            .await;
                                    }
                                }
                            }
                            #[cfg(debug_assertions)]
                            {
                                let _ = tx
                                    .send(AppEvent::UninstallComplete {
                                        success: true,
                                        steps: vec![
                                            "(debug build — service removal skipped)".to_string()
                                        ],
                                    })
                                    .await;
                            }
                        });
                    }
                    KeyAction::SetupComplete => {
                        app.prompt_tx = None;
                        app.connection_status = ConnectionStatus::Connecting;
                        app.session_id = None;
                        app.agent_busy = false;
                        app.connection_error = None;
                        // Remove the "No provider configured" banner now that setup is done.
                        app.messages
                            .retain(|m| !matches!(m, DisplayMessage::SetupPrompt));
                        // Reload the freshly-saved config, then ask about CWD access the
                        // same way the /new command and session picker do.
                        if let Ok(new_cfg) = load_or_create_config() {
                            app.current_cfg = new_cfg.clone();
                            app.model_display = model_display_from_config(&new_cfg);
                            let base = to_proto_session_config(&new_cfg, String::new());
                            if let Some(cwd) = app.cwd.clone() {
                                let (tx, rx) =
                                    tokio::sync::oneshot::channel::<SessionConfig>();
                                app.modal = Some(Modal::CwdPrompt(CwdState {
                                    cwd,
                                    base_config: base,
                                    session_tx: tx,
                                }));
                                let cache = session_config_cache.clone();
                                let notify = reconnect_notify.clone();
                                tokio::spawn(async move {
                                    if let Ok(cfg) = rx.await {
                                        *cache.lock().await = Some(cfg);
                                        notify.notify_one();
                                    }
                                });
                            } else {
                                *session_config_cache.lock().await = Some(base);
                                reconnect_notify.notify_one();
                            }
                        } else {
                            *session_config_cache.lock().await = None;
                            reconnect_notify.notify_one();
                        }
                    }
                    KeyAction::Continue => {}
                }
            }

            Some(app_event) = event_rx.recv() => {
                match app_event {
                    AppEvent::Server(event) => handle_server_event(&mut app, event),
                    AppEvent::Connected(sender) => {
                        info!("connected to server");
                        app.prompt_tx = Some(sender);
                        app.connection_status = ConnectionStatus::Connected;
                        app.cumulative_tokens = 0;
                        app.connection_error = None;
                    }
                    AppEvent::Disconnected(msg) => {
                        info!(error = ?msg, "disconnected from server");
                        app.connection_error = msg;
                        app.prompt_tx = None;
                        app.connection_status = ConnectionStatus::Connecting;
                        app.agent_busy = false;
                        app.auto_scroll = true;
                        app.session_id = None;
                    }
                    AppEvent::ConfigChanged(new_cfg) => {
                        app.current_cfg = new_cfg.clone();
                        app.model_display = model_display_from_config(&new_cfg);
                        info!(model = %app.model_display, "config reloaded");
                        if let Some(tx) = &app.prompt_tx {
                            let _ = tx
                                .send(UserInput {
                                    input: Some(user_input::Input::ConfigUpdate(
                                        // session_id is ignored by the server on ConfigUpdate
                                        to_proto_session_config(&new_cfg, String::new()),
                                    )),
                                })
                                .await;
                        }
                    }
                    AppEvent::SessionsLoaded(sessions, session_tx) => {
                        if app.modal.is_none() {
                            app.modal = Some(Modal::SessionPicker(SessionPickerState {
                                sessions,
                                selected: 0,
                                session_tx,
                            }));
                        }
                    }
                    AppEvent::SessionDeleted(id) => {
                        if let Some(Modal::SessionPicker(picker_state)) = &mut app.modal {
                            picker_state.sessions.retain(|s| s.id != id);
                            // Clamp selection: index 0 is always "New Session".
                            let max_idx = picker_state.sessions.len();
                            if picker_state.selected > max_idx {
                                picker_state.selected = max_idx;
                            }
                        }
                    }
                    AppEvent::PluginStatusLoaded(sources) => {
                        if let Some(Modal::PluginManager(modal_state)) = &mut app.modal {
                            modal_state.sources = sources;
                            modal_state.loading = false;
                        }
                    }
                    AppEvent::PluginInstallResult { success, message } => {
                        if let Some(Modal::PluginManager(modal_state)) = &mut app.modal {
                            modal_state.installing = false;
                            modal_state.status_message = Some(message);
                            // Optimistically mark the source as installed on success.
                            if success {
                                for source in &mut modal_state.sources {
                                    source.installed = true;
                                }
                            }
                        }
                    }
                    AppEvent::UninstallComplete { success, steps } => {
                        if let Some(Modal::UninstallConfirm(s)) = &mut app.modal {
                            s.phase = UninstallPhase::Done { success };
                            s.log = steps;
                        }
                    }
                }
            }
        }
    }

    // Restore the terminal to its original state before exiting.
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ClientConfig, PluginConfig};
    use std::collections::HashMap;

    fn config_with_model(model: &str) -> ClientConfig {
        let mut params = HashMap::new();
        params.insert("model".to_string(), serde_json::json!(model));
        let mut plugin_configs = HashMap::new();
        plugin_configs.insert(
            "ein_openrouter".to_string(),
            PluginConfig {
                allowed_paths: vec![],
                allowed_hosts: vec![],
                params,
            },
        );
        ClientConfig {
            model_client_name: "ein_openrouter".to_string(),
            plugin_configs,
            ..Default::default()
        }
    }

    #[test]
    fn model_display_strips_vendor_prefix() {
        let cfg = config_with_model("anthropic/claude-sonnet-4-5");
        assert_eq!(model_display_from_config(&cfg), "claude-sonnet-4-5");
    }

    #[test]
    fn model_display_no_model_returns_unknown() {
        let cfg = ClientConfig::default();
        assert_eq!(model_display_from_config(&cfg), "unknown");
    }

    #[test]
    fn model_display_no_prefix_passthrough() {
        let cfg = config_with_model("llama3");
        assert_eq!(model_display_from_config(&cfg), "llama3");
    }
}
