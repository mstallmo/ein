// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use ein_proto::ein::{AgentEvent, SessionConfig, SessionSummary, UserInput};
use tokio::sync::{mpsc, oneshot};

use crate::config::ClientConfig;

// ---------------------------------------------------------------------------
// Connection state
// ---------------------------------------------------------------------------

/// All events delivered to the main TUI select! loop.
pub(crate) enum AppEvent {
    /// A normal event streamed from the server.
    Server(AgentEvent),
    /// Connection was established; carries the sender for outbound prompts.
    Connected(mpsc::Sender<UserInput>),
    /// Connection was lost or a connection attempt failed.
    /// `Some(msg)` only when a session was already active (shown in conversation).
    Disconnected(Option<String>),
    /// `~/.ein/config.json` changed on disk; carries the freshly parsed config.
    ConfigChanged(ClientConfig),
    /// Server returned the session list. The TUI shows the session picker and
    /// sends the chosen `SessionConfig` back via the oneshot sender.
    SessionsLoaded(Vec<SessionSummary>, oneshot::Sender<SessionConfig>),
    /// A session was successfully deleted; remove it from the session picker.
    SessionDeleted(String),
}

/// Whether the TUI currently has a live server connection.
pub(crate) enum ConnectionStatus {
    Connecting,
    Connected,
}

// ---------------------------------------------------------------------------
// Display model
//
// Each variant represents one logical block in the conversation transcript.
// ---------------------------------------------------------------------------

pub(crate) enum DisplayMessage {
    /// Welcome banner shown once at the top of the conversation on startup.
    Header { cwd: String },
    /// Text sent by the local user.
    User(String),
    /// Streamed text from the agent (may be appended to incrementally).
    AgentText(String),
    /// A tool invocation. `arg` is the most meaningful single parameter for
    /// display (e.g. the shell command for Bash, the file path for Read/Write).
    /// `output_lines` accumulates stdout streamed in real time (Bash only).
    ToolCall {
        name: String,
        arg: Option<String>,
        output_lines: Vec<String>,
    },
    /// An Edit tool invocation with a syntax-highlighted diff for display.
    /// Populated at `ToolCallEnd` once the server has computed the start line.
    EditCall {
        file_path: String,
        start_line: u32,
        old_lines: Vec<String>,
        new_lines: Vec<String>,
    },
    /// An error returned by either the agent or the server.
    Error(String),
}

// ---------------------------------------------------------------------------
// Session picker / CWD prompt state
// ---------------------------------------------------------------------------

/// State for the session picker modal shown on first connection.
pub(crate) struct SessionPickerState {
    /// Existing sessions from the server (newest-first). Index 0 in the UI is
    /// always "New Session" and is not stored here.
    pub(crate) sessions: Vec<SessionSummary>,
    /// Currently highlighted row. 0 = "New Session", 1..=sessions.len() = existing.
    pub(crate) selected: usize,
    /// One-shot channel back to `try_connect`; sends the chosen `SessionConfig`.
    pub(crate) session_tx: oneshot::Sender<SessionConfig>,
}

/// State for the CWD access modal, shown only when "New Session" is chosen.
pub(crate) struct CwdState {
    pub(crate) cwd: String,
    /// Base `SessionConfig` built from `~/.ein/config.json`; CWD is optionally
    /// appended to `allowed_paths` before forwarding to `try_connect`.
    pub(crate) base_config: SessionConfig,
    /// Forwarded from `SessionPickerState.session_tx`.
    pub(crate) session_tx: oneshot::Sender<SessionConfig>,
}

/// Minimal deserialization target for the `session_config_json` field stored in
/// the database. Mirrors `SessionConfigRecord` in the server crate without
/// requiring a cross-crate dependency.
#[derive(serde::Deserialize, Default)]
pub(crate) struct StoredSessionConfig {
    #[serde(default)]
    pub(crate) allowed_paths: Vec<String>,
    #[serde(default)]
    pub(crate) allowed_hosts: Vec<String>,
    #[serde(default)]
    pub(crate) model_client_name: String,
    #[serde(default)]
    pub(crate) plugin_configs: std::collections::HashMap<String, StoredPluginConfig>,
}

#[derive(serde::Deserialize, Default)]
pub(crate) struct StoredPluginConfig {
    #[serde(default)]
    pub(crate) allowed_paths: Vec<String>,
    #[serde(default)]
    pub(crate) allowed_hosts: Vec<String>,
    #[serde(default)]
    pub(crate) params_json: String,
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

pub(crate) struct App {
    /// All messages rendered in the conversation pane.
    pub(crate) messages: Vec<DisplayMessage>,
    /// Current contents of the input field.
    pub(crate) input: String,
    /// Cursor position within `input`, counted in Unicode scalar values.
    pub(crate) cursor_pos: usize,
    /// True while the server is processing; disables text input.
    pub(crate) agent_busy: bool,
    /// How many lines the user has scrolled up from the bottom.
    pub(crate) scroll_offset: u16,
    /// When true the view follows new output automatically.
    pub(crate) auto_scroll: bool,
    /// True when the autocomplete panel should show filtered results.
    pub(crate) autocomplete_active: bool,
    /// Indices into `COMMANDS` that match the current input prefix.
    pub(crate) autocomplete_matches: Vec<usize>,
    /// Frame counter incremented by the animation ticker while busy.
    pub(crate) tick: u64,
    /// Short model name for the status bar (vendor prefix stripped).
    pub(crate) model_display: String,
    /// Cumulative tokens used this session (updated on each TokenUsage event).
    pub(crate) cumulative_tokens: i32,
    /// Current server connection state; drives status bar and input gating.
    pub(crate) connection_status: ConnectionStatus,
    /// Outbound prompt channel; `None` while disconnected.
    pub(crate) prompt_tx: Option<mpsc::Sender<UserInput>>,
    /// Last connection error message, shown above the connecting spinner.
    /// Replaced in-place on each disconnect; cleared when connected.
    pub(crate) connection_error: Option<String>,
    /// When `Some`, the session picker overlay is visible (shown first on startup).
    pub(crate) pending_session_picker: Option<SessionPickerState>,
    /// When `Some`, the CWD access modal is visible (only for new sessions).
    pub(crate) pending_cwd_prompt: Option<CwdState>,
    /// Current working directory captured at startup; offered when creating new sessions.
    pub(crate) cwd: Option<String>,
    /// Current client config, kept in sync with `ConfigChanged` events.
    pub(crate) current_cfg: ClientConfig,
    /// Session UUID assigned by the server, shown in the status bar.
    pub(crate) session_id: Option<String>,
}

impl App {
    pub(crate) fn new(
        model_display: String,
        cwd: Option<String>,
        cwd_display: String,
        current_cfg: ClientConfig,
    ) -> Self {
        Self {
            messages: vec![DisplayMessage::Header { cwd: cwd_display }],
            input: String::new(),
            cursor_pos: 0,
            agent_busy: false,
            scroll_offset: 0,
            auto_scroll: true,
            autocomplete_active: false,
            autocomplete_matches: vec![],
            tick: 0,
            model_display,
            cumulative_tokens: 0,
            connection_status: ConnectionStatus::Connecting,
            prompt_tx: None,
            connection_error: None,
            pending_session_picker: None,
            pending_cwd_prompt: None,
            cwd,
            current_cfg,
            session_id: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Test helpers (shared across all test modules in this crate)
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_helpers {
    use super::*;
    use ein_proto::ein::{AgentEvent, agent_event::Event as ProtoEvent};

    /// Build an `App` suitable for tests: no CWD modal, no prompt sender.
    pub(crate) fn make_app(model: &str) -> App {
        App::new(
            model.to_string(),
            None,
            "/test".to_string(),
            ClientConfig::default(),
        )
    }

    /// Wrap a proto event variant into an `AgentEvent`.
    pub(crate) fn agent_event(ev: ProtoEvent) -> AgentEvent {
        AgentEvent { event: Some(ev) }
    }
}
