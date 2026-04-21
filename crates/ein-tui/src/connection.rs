// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use ein_proto::ein::{
    ListSessionsRequest, SessionConfig, UserInput, agent_client::AgentClient, user_input,
};
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;

use crate::app::AppEvent;
use crate::config::{ClientConfig, load_or_create_config};

// ---------------------------------------------------------------------------
// Config → proto conversion
// ---------------------------------------------------------------------------

/// Converts a `ClientConfig` to its proto `SessionConfig` equivalent.
pub(crate) fn to_proto_session_config(cfg: &ClientConfig, session_id: String) -> SessionConfig {
    use ein_proto::ein::PluginConfig as ProtoPluginConfig;
    SessionConfig {
        allowed_paths: cfg.allowed_paths.clone(),
        allowed_hosts: cfg.allowed_hosts.clone(),
        plugin_configs: cfg
            .plugin_configs
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    ProtoPluginConfig {
                        allowed_paths: v.allowed_paths.clone(),
                        allowed_hosts: v.allowed_hosts.clone(),
                        params_json: serde_json::to_string(&v.params)
                            .unwrap_or_else(|_| "{}".to_string()),
                    },
                )
            })
            .collect(),
        model_client_name: cfg.model_client_name.clone(),
        session_id,
    }
}

// ---------------------------------------------------------------------------
// Config file watcher
// ---------------------------------------------------------------------------

/// Spawns a background task that watches `~/.ein/config.json` for changes.
///
/// On each change the config is re-read and a `ConfigChanged` event is sent to
/// the main TUI loop, which forwards the new credentials to the server via a
/// `ConfigUpdate` message on the live session (if connected).
pub(crate) fn spawn_config_watcher(event_tx: mpsc::Sender<AppEvent>) {
    use notify::Watcher;

    let config_dir = match dirs::home_dir() {
        Some(h) => h.join(".ein"),
        None => return,
    };

    let (notify_tx, mut notify_rx) = mpsc::channel(4);
    let mut watcher = match notify::recommended_watcher(move |res| {
        let _ = notify_tx.blocking_send(res);
    }) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[config watcher] failed to create watcher: {e}");
            return;
        }
    };
    if let Err(e) = watcher.watch(&config_dir, notify::RecursiveMode::NonRecursive) {
        eprintln!(
            "[config watcher] failed to watch {}: {e}",
            config_dir.display()
        );
        return;
    }

    tokio::spawn(async move {
        let _watcher = watcher; // keep the watcher alive for the duration of the task

        while let Some(Ok(event)) = notify_rx.recv().await {
            let is_config = event.paths.iter().any(|p| p.ends_with("config.json"));
            if !is_config {
                continue;
            }

            // Brief debounce — editors often fire several events per save.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            while notify_rx.try_recv().is_ok() {} // drain duplicates

            match load_or_create_config() {
                Ok(new_cfg) => {
                    if event_tx
                        .send(AppEvent::ConfigChanged(new_cfg))
                        .await
                        .is_err()
                    {
                        break; // TUI exited
                    }
                }
                Err(e) => eprintln!("[config watcher] failed to reload config: {e}"),
            }
        }
    });
}

// ---------------------------------------------------------------------------
// gRPC connection
// ---------------------------------------------------------------------------

/// Attempts one full connect → session → bridge cycle.
///
/// Returns `Ok(true)` if a session was live at some point (so the caller can
/// decide whether a subsequent failure warrants a visible error message).
/// Returns `Ok(false)` if the TUI event channel closed (TUI exited — stop retrying).
/// Returns `Err` if the initial connection or handshake failed.
///
/// On the first connection, lists existing sessions and sends a `SessionsLoaded`
/// event to the TUI, then awaits the user's session choice via a oneshot channel.
/// On reconnects the cached `SessionConfig` is reused directly.
async fn try_connect(
    server_addr: &str,
    event_tx: &mpsc::Sender<AppEvent>,
    session_config_cache: &std::sync::Arc<tokio::sync::Mutex<Option<SessionConfig>>>,
) -> anyhow::Result<bool> {
    let channel = Channel::from_shared(server_addr.to_string())?
        .connect()
        .await?;
    let mut grpc_client = AgentClient::new(channel);

    // Determine the SessionConfig to use for this connection.
    let init_config = {
        let cached = session_config_cache.lock().await.clone();
        match cached {
            Some(cfg) => cfg, // Reconnect: reuse the previously chosen config.
            None => {
                // First connection: list existing sessions and ask the user.
                let resp = grpc_client
                    .list_sessions(tonic::Request::new(ListSessionsRequest {}))
                    .await?;
                let sessions = resp.into_inner().sessions;

                let (tx, rx) = oneshot::channel::<SessionConfig>();
                if event_tx
                    .send(AppEvent::SessionsLoaded(sessions, tx))
                    .await
                    .is_err()
                {
                    return Ok(false); // TUI exited while we were fetching
                }

                // Block until the user makes a selection (or the TUI exits).
                match rx.await {
                    Ok(cfg) => {
                        *session_config_cache.lock().await = Some(cfg.clone());
                        cfg
                    }
                    Err(_) => return Ok(false), // oneshot dropped — TUI exited
                }
            }
        }
    };

    let (prompt_tx, prompt_rx) = mpsc::channel::<UserInput>(8);
    let prompt_stream = ReceiverStream::new(prompt_rx);

    let response = grpc_client
        .agent_session(tonic::Request::new(prompt_stream))
        .await?;
    let mut server_stream = response.into_inner();

    // Send SessionConfig as the mandatory first message before any prompts.
    prompt_tx
        .send(UserInput {
            input: Some(user_input::Input::Init(init_config)),
        })
        .await?;

    // Signal the TUI that the session is live.
    if event_tx.send(AppEvent::Connected(prompt_tx)).await.is_err() {
        return Ok(false); // TUI exited
    }

    // Bridge: forward server events until the stream ends.
    loop {
        match server_stream.message().await {
            Ok(Some(event)) => {
                if event_tx.send(AppEvent::Server(event)).await.is_err() {
                    return Ok(false); // TUI exited
                }
            }
            Ok(None) => {
                // Server closed the stream cleanly.
                let _ = event_tx.send(AppEvent::Disconnected(None)).await;
                return Ok(true);
            }
            Err(e) => {
                let _ = event_tx
                    .send(AppEvent::Disconnected(Some(e.to_string())))
                    .await;
                return Ok(true);
            }
        }
    }
}

/// Background task: connects to the server and retries every 3 s on failure.
///
/// Errors on the initial connection attempt are silent (status bar already
/// shows "Connecting…"). Errors after a live session was established are
/// forwarded as `Disconnected(Some(...))` so the conversation shows a message.
pub(crate) async fn connection_manager(
    server_addr: String,
    event_tx: mpsc::Sender<AppEvent>,
    session_config_cache: std::sync::Arc<tokio::sync::Mutex<Option<SessionConfig>>>,
) {
    loop {
        match try_connect(&server_addr, &event_tx, &session_config_cache).await {
            Ok(false) => return, // TUI exited — stop the task
            Ok(true) => {
                // Session was live; try_connect already sent the Disconnected event.
            }
            Err(_) => {
                // Initial connection failed; the connecting spinner is enough feedback.
                // Don't send another Disconnected — that would overwrite the existing
                // error message shown from the last real session drop.
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
}
