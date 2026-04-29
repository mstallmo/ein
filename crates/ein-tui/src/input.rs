// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ein_proto::ein::{UserInput, agent_event::Event as ServerEvent, user_input};
use tracing::{debug, info, warn};

use crate::app::{
    App, CwdState, DisplayMessage, Modal, SessionPickerState, SetupWizardState, WizardStep,
};
use crate::connection::to_proto_session_config;

// ---------------------------------------------------------------------------
// Command registry
//
// Every slash command available in the TUI is declared here. The autocomplete
// panel reads from this list at render time, filtering by the current input
// prefix, so adding a new command only requires appending an entry below.
// ---------------------------------------------------------------------------

pub(crate) struct CommandDef {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
}

pub(crate) const COMMANDS: &[CommandDef] = &[
    CommandDef {
        name: "/exit",
        description: "Exit Ein",
    },
    CommandDef {
        name: "/config",
        description: "Edit ~/.ein/config.json",
    },
    CommandDef {
        name: "/clear",
        description: "Clear conversation history",
    },
    CommandDef {
        name: "/new",
        description: "Start a new session",
    },
    CommandDef {
        name: "/sessions",
        description: "Switch to a different session",
    },
    CommandDef {
        name: "/compact",
        description: "Summarize and compact conversation history",
    },
    CommandDef {
        name: "/plugins",
        description: "Manage installed plugins",
    },
    CommandDef {
        name: "/setup",
        description: "Run the first-time setup wizard",
    },
];

/// Recomputes `autocomplete_matches` and `autocomplete_active` based on the
/// current input. Called after every keystroke that modifies `app.input`.
pub(crate) fn update_autocomplete(app: &mut App) {
    if app.input.starts_with('/') {
        app.autocomplete_matches = COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, cmd)| cmd.name.starts_with(app.input.as_str()))
            .map(|(i, _)| i)
            .collect();
        app.autocomplete_active = !app.autocomplete_matches.is_empty();
    } else {
        app.autocomplete_active = false;
        app.autocomplete_matches.clear();
    }
}

// ---------------------------------------------------------------------------
// Key action
// ---------------------------------------------------------------------------

/// The outcome of processing a single key press, returned to the caller in
/// `main` so terminal lifecycle side-effects stay in one place.
pub(crate) enum KeyAction {
    /// The user quit (Ctrl-C or `/exit`).
    Quit,
    /// The user ran `/config`; open this path in `$EDITOR` then resume.
    OpenConfig(std::path::PathBuf),
    /// No further action required; continue the event loop.
    Continue,
    /// The user ran `/new`; drop the current session and start a fresh one.
    NewSession,
    /// The user ran `/sessions`; show the session picker to switch sessions.
    OpenSessionPicker,
    /// The user pressed Shift+D on an existing session in the picker; delete it.
    DeleteSession(String),
    /// Open the plugin manager modal and fetch status from the server.
    OpenPluginModal,
    /// User selected a plugin source to install/update; `source_id` identifies it.
    InstallPlugin { source_id: String },
    /// Open (or reopen) the first-time setup wizard.
    OpenSetupWizard,
    /// Setup wizard saved config; trigger an immediate reconnect.
    SetupComplete,
}

// ---------------------------------------------------------------------------
// Key event handler
// ---------------------------------------------------------------------------

/// Processes a single key press and mutates `app` accordingly.
///
/// Returns `KeyAction::Quit` to signal a clean exit, `KeyAction::OpenConfig`
/// to hand off the editor spawn to the caller (which owns the terminal), or
/// `KeyAction::Continue` for all other cases.
pub(crate) async fn handle_key_event(app: &mut App, key: KeyEvent) -> KeyAction {
    // Ctrl-C always exits immediately, even while the agent is busy.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return KeyAction::Quit;
    }

    match &app.modal {
        Some(Modal::SetupWizard(_)) => handle_setup_wizard_key(app, key),
        Some(Modal::PluginManager(_)) => handle_plugin_modal_key(app, key),
        Some(Modal::SessionPicker(_)) => handle_session_picker_key(app, key).await,
        Some(Modal::CwdPrompt(_)) => handle_cwd_modal_key(app, key),
        None => handle_normal_key(app, key).await,
    }
}

fn handle_plugin_modal_key(app: &mut App, key: KeyEvent) -> KeyAction {
    let busy = matches!(&app.modal, Some(Modal::PluginManager(m)) if m.loading || m.installing);
    if busy {
        if key.code == KeyCode::Esc {
            app.modal = None;
        }
        return KeyAction::Continue;
    }

    match key.code {
        KeyCode::Esc => {
            app.modal = None;
        }
        KeyCode::Up => {
            if let Some(Modal::PluginManager(m)) = &mut app.modal {
                if m.selected > 0 {
                    m.selected -= 1;
                }
            }
        }
        KeyCode::Down => {
            if let Some(Modal::PluginManager(m)) = &mut app.modal {
                if m.selected + 1 < m.sources.len() {
                    m.selected += 1;
                }
            }
        }
        KeyCode::Enter => {
            let source_id = match &app.modal {
                Some(Modal::PluginManager(m)) => m
                    .sources
                    .get(m.selected)
                    .map(|s| s.id.clone())
                    .unwrap_or_else(|| "default".to_string()),
                _ => return KeyAction::Continue,
            };
            if let Some(Modal::PluginManager(m)) = &mut app.modal {
                m.installing = true;
                m.status_message = None;
            }
            return KeyAction::InstallPlugin { source_id };
        }
        _ => {}
    }
    KeyAction::Continue
}

async fn handle_session_picker_key(app: &mut App, key: KeyEvent) -> KeyAction {
    match key.code {
        KeyCode::Up => {
            if let Some(Modal::SessionPicker(p)) = &mut app.modal {
                if p.selected > 0 {
                    p.selected -= 1;
                }
            }
        }
        KeyCode::Down => {
            if let Some(Modal::SessionPicker(p)) = &mut app.modal {
                if p.selected < p.sessions.len() {
                    p.selected += 1;
                }
            }
        }
        // Shift+D: delete the highlighted existing session (not "New Session").
        KeyCode::Char('D') => {
            if let Some(Modal::SessionPicker(p)) = &app.modal {
                if p.selected > 0 {
                    let session_id = p.sessions[p.selected - 1].id.clone();
                    return KeyAction::DeleteSession(session_id);
                }
            }
        }
        // S: open the setup wizard to configure a provider.
        KeyCode::Char('s') | KeyCode::Char('S') => {
            return KeyAction::OpenSetupWizard;
        }
        KeyCode::Enter => {
            let state = match app.modal.take() {
                Some(Modal::SessionPicker(s)) => s,
                other => {
                    app.modal = other;
                    return KeyAction::Continue;
                }
            };
            if state.selected == 0 {
                let base = to_proto_session_config(&app.current_cfg, String::new());
                if let Some(cwd) = app.cwd.clone() {
                    app.modal = Some(Modal::CwdPrompt(CwdState {
                        cwd,
                        base_config: base,
                        session_tx: state.session_tx,
                    }));
                } else {
                    let _ = state.session_tx.send(base);
                }
            } else {
                resolve_session_resume(state).await;
            }
        }
        _ => {}
    }
    KeyAction::Continue
}

async fn resolve_session_resume(state: SessionPickerState) {
    use crate::app::StoredSessionConfig;

    let session = &state.sessions[state.selected - 1];
    let stored: StoredSessionConfig =
        serde_json::from_str(&session.session_config_json).unwrap_or_default();
    let resume_cfg = ein_proto::ein::SessionConfig {
        allowed_paths: stored.allowed_paths,
        allowed_hosts: stored.allowed_hosts,
        plugin_configs: stored
            .plugin_configs
            .into_iter()
            .map(|(k, v)| {
                (
                    k,
                    ein_proto::ein::PluginConfig {
                        allowed_paths: v.allowed_paths,
                        allowed_hosts: v.allowed_hosts,
                        params_json: v.params_json,
                    },
                )
            })
            .collect(),
        model_client_name: stored.model_client_name,
        session_id: session.id.clone(),
    };
    let _ = state.session_tx.send(resume_cfg);
}

fn handle_setup_wizard_key(app: &mut App, key: KeyEvent) -> KeyAction {
    // Clone the current step so we release the borrow before mutating app.modal.
    let step = match &app.modal {
        Some(Modal::SetupWizard(w)) => w.step.clone(),
        _ => return KeyAction::Continue,
    };

    match step {
        WizardStep::ChooseProvider => match key.code {
            KeyCode::Esc => {
                app.modal = None;
            }
            KeyCode::Up => {
                if let Some(Modal::SetupWizard(w)) = &mut app.modal {
                    if w.provider_idx > 0 {
                        w.provider_idx -= 1;
                    }
                }
            }
            KeyCode::Down => {
                if let Some(Modal::SetupWizard(w)) = &mut app.modal {
                    if w.provider_idx + 1 < crate::app::PROVIDERS.len() {
                        w.provider_idx += 1;
                    }
                }
            }
            KeyCode::Enter | KeyCode::Tab => {
                if let Some(Modal::SetupWizard(w)) = &mut app.modal {
                    w.advance_step();
                }
            }
            _ => {}
        },

        WizardStep::EnterApiKey => {
            if let Some(Modal::SetupWizard(w)) = &mut app.modal {
                handle_wizard_text_input(key, w, |w| (&mut w.api_key, &mut w.api_key_cursor));
            }
        }
        WizardStep::EnterBaseUrl => {
            if let Some(Modal::SetupWizard(w)) = &mut app.modal {
                handle_wizard_text_input(key, w, |w| (&mut w.base_url, &mut w.base_url_cursor));
            }
        }
        WizardStep::EnterModel => {
            if let Some(Modal::SetupWizard(w)) = &mut app.modal {
                handle_wizard_text_input(key, w, |w| (&mut w.model, &mut w.model_cursor));
            }
        }

        WizardStep::Confirm => match key.code {
            KeyCode::Esc => {
                if let Some(Modal::SetupWizard(w)) = &mut app.modal {
                    w.error = None;
                    w.retreat_step();
                }
            }
            KeyCode::Enter => {
                let (provider_key, api_key, base_url, model) = match &app.modal {
                    Some(Modal::SetupWizard(w)) => (
                        w.provider_key(),
                        w.api_key.clone(),
                        w.base_url.clone(),
                        w.model.clone(),
                    ),
                    _ => return KeyAction::Continue,
                };
                let cfg = crate::config::build_config_for_provider(
                    provider_key,
                    &api_key,
                    &base_url,
                    &model,
                );
                match crate::config::save_config(&cfg) {
                    Ok(()) => {
                        app.modal = None;
                        return KeyAction::SetupComplete;
                    }
                    Err(e) => {
                        if let Some(Modal::SetupWizard(w)) = &mut app.modal {
                            w.error = Some(e.to_string());
                        }
                    }
                }
            }
            _ => {}
        },
    }

    KeyAction::Continue
}

/// Handles a key press for a wizard text-input step.
///
/// `field_fn` extracts mutable references to the buffer and cursor for the active field.
fn handle_wizard_text_input<F>(key: KeyEvent, wizard: &mut SetupWizardState, field_fn: F)
where
    F: Fn(&mut SetupWizardState) -> (&mut String, &mut usize),
{
    match key.code {
        KeyCode::Esc => wizard.retreat_step(),
        KeyCode::Enter | KeyCode::Tab => wizard.advance_step(),
        KeyCode::Char(c) => {
            let (buf, cursor) = field_fn(wizard);
            let byte_idx = char_to_byte_idx(buf, *cursor);
            buf.insert(byte_idx, c);
            *cursor += 1;
        }
        KeyCode::Backspace => {
            let (buf, cursor) = field_fn(wizard);
            if *cursor > 0 {
                let byte_end = char_to_byte_idx(buf, *cursor);
                let byte_start = char_to_byte_idx(buf, *cursor - 1);
                buf.drain(byte_start..byte_end);
                *cursor -= 1;
            }
        }
        KeyCode::Left => {
            let (_, cursor) = field_fn(wizard);
            if *cursor > 0 {
                *cursor -= 1;
            }
        }
        KeyCode::Right => {
            let (buf, cursor) = field_fn(wizard);
            let len = buf.chars().count();
            if *cursor < len {
                *cursor += 1;
            }
        }
        _ => {}
    }
}

fn handle_cwd_modal_key(app: &mut App, key: KeyEvent) -> KeyAction {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            if let Some(Modal::CwdPrompt(state)) = app.modal.take() {
                let mut config = state.base_config;
                config.allowed_paths.push(state.cwd);
                let _ = state.session_tx.send(config);
            }
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Enter | KeyCode::Esc => {
            if let Some(Modal::CwdPrompt(state)) = app.modal.take() {
                let _ = state.session_tx.send(state.base_config);
            }
        }
        _ => {}
    }
    KeyAction::Continue
}

async fn handle_normal_key(app: &mut App, key: KeyEvent) -> KeyAction {
    match key.code {
        KeyCode::Enter => {
            if app.agent_busy || app.input.is_empty() {
                return KeyAction::Continue;
            }
            let text = std::mem::take(&mut app.input);
            app.cursor_pos = 0;
            app.autocomplete_active = false;
            app.autocomplete_matches.clear();

            // Slash commands work regardless of connection state.
            match text.as_str() {
                "/clear" => {
                    // Tell the server to wipe its in-memory context (SQLite history is kept).
                    if let Some(tx) = &app.prompt_tx {
                        let _ = tx
                            .send(UserInput {
                                input: Some(user_input::Input::ClearContext(true)),
                            })
                            .await;
                    }
                    // Clear the local display, keeping the header banner.
                    app.messages
                        .retain(|m| matches!(m, DisplayMessage::Header { .. }));
                    app.scroll_offset = 0;
                    app.auto_scroll = true;

                    return KeyAction::Continue;
                }
                "/compact" => {
                    // Require an active connection — compact triggers a server LLM call.
                    if app.prompt_tx.is_none() {
                        app.messages
                            .push(DisplayMessage::Error("Not connected to server".to_string()));
                        app.auto_scroll = true;
                        return KeyAction::Continue;
                    }
                    if let Some(tx) = &app.prompt_tx {
                        let _ = tx
                            .send(UserInput {
                                input: Some(user_input::Input::CompactContext(true)),
                            })
                            .await;
                    }

                    // Clear display so only the incoming summary is shown.
                    app.messages
                        .retain(|m| matches!(m, DisplayMessage::Header { .. }));
                    app.scroll_offset = 0;
                    app.auto_scroll = true;
                    app.agent_busy = true;

                    return KeyAction::Continue;
                }
                "/config" => {
                    if let Some(path) = dirs::home_dir().map(|h| h.join(".ein").join("config.json"))
                    {
                        return KeyAction::OpenConfig(path);
                    }
                    return KeyAction::Continue;
                }
                "/exit" => return KeyAction::Quit,
                "/new" => return KeyAction::NewSession,
                "/plugins" => return KeyAction::OpenPluginModal,
                "/sessions" => return KeyAction::OpenSessionPicker,
                "/setup" => return KeyAction::OpenSetupWizard,
                _ => {
                    // Reject unrecognized slash commands — display a local error, do not send to server.
                    if text.starts_with('/') {
                        let cmd = text.split_whitespace().next().unwrap_or(&text);
                        app.messages
                            .push(DisplayMessage::Error(format!("Unknown command: {}", cmd)));
                        app.auto_scroll = true;
                        return KeyAction::Continue;
                    }

                    // Prompts require an active connection.
                    if app.prompt_tx.is_none() {
                        return KeyAction::Continue;
                    }

                    app.messages.push(DisplayMessage::User(text.clone()));
                    app.auto_scroll = true;
                    app.agent_busy = true;
                    if let Some(tx) = &app.prompt_tx {
                        let _ = tx
                            .send(UserInput {
                                input: Some(user_input::Input::Prompt(text)),
                            })
                            .await;
                    }
                }
            }
        }
        KeyCode::Char(c) => {
            if !app.agent_busy {
                let byte_idx = char_to_byte_idx(&app.input, app.cursor_pos);
                app.input.insert(byte_idx, c);
                app.cursor_pos += 1;
                update_autocomplete(app);
            }
        }
        KeyCode::Backspace => {
            if !app.agent_busy && app.cursor_pos > 0 {
                let byte_end = char_to_byte_idx(&app.input, app.cursor_pos);
                let byte_start = char_to_byte_idx(&app.input, app.cursor_pos - 1);
                app.input.drain(byte_start..byte_end);
                app.cursor_pos -= 1;
                update_autocomplete(app);
            }
        }
        KeyCode::Left => {
            if !app.agent_busy && app.cursor_pos > 0 {
                app.cursor_pos -= 1;
            }
        }
        KeyCode::Right => {
            if !app.agent_busy {
                let char_count = app.input.chars().count();
                if app.cursor_pos < char_count {
                    app.cursor_pos += 1;
                }
            }
        }
        // Scroll up: disable auto-scroll and move one line toward the top.
        KeyCode::Up => {
            app.auto_scroll = false;
            app.scroll_offset = app.scroll_offset.saturating_add(1);
        }
        // Scroll down: move toward the bottom; re-enable auto-scroll at the end.
        KeyCode::Down => {
            if app.scroll_offset > 0 {
                app.scroll_offset -= 1;
            } else {
                app.auto_scroll = true;
            }
        }
        _ => {}
    }
    KeyAction::Continue
}

// ---------------------------------------------------------------------------
// Server event handler
// ---------------------------------------------------------------------------

/// Applies a single server-pushed `AgentEvent` to the local app state.
///
/// `ContentDelta` events are coalesced into the last `AgentText` message
/// so that streaming output appears as one continuously growing block rather
/// than many small fragments.
pub(crate) fn handle_server_event(app: &mut App, event: ein_proto::ein::AgentEvent) {
    match event.event {
        Some(ServerEvent::ContentDelta(d)) => {
            // Append to the last AgentText block if possible; otherwise start a new one.
            if let Some(DisplayMessage::AgentText(text)) = app.messages.last_mut() {
                text.push_str(&d.text);
            } else {
                app.messages.push(DisplayMessage::AgentText(d.text));
            }
            app.auto_scroll = true;
        }
        Some(ServerEvent::ToolCallStart(t)) => {
            debug!(tool = %t.tool_name, "tool call start");
            let arg = if t.display_arg.is_empty() {
                None
            } else {
                Some(t.display_arg.clone())
            };

            app.messages.push(DisplayMessage::ToolCall {
                name: t.tool_name.clone(),
                arg,
                output_lines: vec![],
            });
            app.auto_scroll = true;
        }
        Some(ServerEvent::ToolCallEnd(t)) => {
            debug!(tool = %t.tool_name, "tool call end");
            // For Edit calls the tool returns diff metadata; replace the
            // ToolCall placeholder that was pushed at ToolCallStart.
            if t.tool_name == "Edit"
                && !t.metadata.is_empty()
                && let Ok(meta) = serde_json::from_str::<serde_json::Value>(&t.metadata)
            {
                let start_line =
                    meta.get("start_line").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
                let parse_lines = |key: &str| -> Vec<String> {
                    meta.get(key)
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let old_lines = parse_lines("old_lines");
                let new_lines = parse_lines("new_lines");

                for msg in app.messages.iter_mut().rev() {
                    if let DisplayMessage::ToolCall {
                        name,
                        arg: file_path_opt,
                        ..
                    } = msg
                        && name == "Edit"
                    {
                        let file_path = file_path_opt.clone().unwrap_or_default();
                        *msg = DisplayMessage::EditCall {
                            file_path,
                            start_line,
                            old_lines,
                            new_lines,
                        };
                        break;
                    }
                }
            }
        }
        Some(ServerEvent::AgentFinished(f)) => {
            debug!("agent finished");
            if !f.final_content.is_empty() {
                app.messages
                    .push(DisplayMessage::AgentText(f.final_content));
            }
            app.agent_busy = false;
            app.auto_scroll = true;
        }
        Some(ServerEvent::AgentError(e)) => {
            warn!(message = %e.message, "agent error");
            app.messages.push(DisplayMessage::Error(e.message));
            app.agent_busy = false;
            app.auto_scroll = true;
        }
        Some(ServerEvent::TokenUsage(u)) => {
            debug!(total = u.total_tokens, "token usage");
            app.cumulative_tokens = u.total_tokens;
        }
        Some(ServerEvent::ToolOutputChunk(c)) => {
            debug!(
                chunk_len = c.output.len(),
                lines = c.output.split('\n').count(),
                "tool output chunk",
            );
            if let Some(DisplayMessage::ToolCall { output_lines, .. }) = app.messages.last_mut() {
                // Split on newlines so each entry is a single display line.
                // This keeps the row-count calculation correct (it doesn't
                // account for embedded '\n' within a ratatui Line).
                output_lines.extend(c.output.split('\n').map(str::to_owned));
                app.auto_scroll = true;
            }
        }
        Some(ServerEvent::SessionStarted(s)) => {
            app.session_id = Some(s.session_id.clone());
            if s.resumed && !s.history.is_empty() {
                info!(session_id = %s.session_id, messages = s.history.len(), "restoring session history");
                for h_msg in &s.history {
                    match h_msg.role.as_str() {
                        "user" if !h_msg.content.is_empty() => {
                            app.messages
                                .push(DisplayMessage::User(h_msg.content.clone()));
                        }
                        "assistant" => {
                            if !h_msg.content.is_empty() {
                                app.messages
                                    .push(DisplayMessage::AgentText(h_msg.content.clone()));
                            }
                            for tc in &h_msg.tool_calls {
                                let arg = if tc.display_arg.is_empty() {
                                    None
                                } else {
                                    Some(tc.display_arg.clone())
                                };
                                app.messages.push(DisplayMessage::ToolCall {
                                    name: tc.tool_name.clone(),
                                    arg,
                                    output_lines: vec![],
                                });
                            }
                        }
                        _ => {}
                    }
                }
                app.auto_scroll = true;
            }
        }
        None => {}
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Converts a Unicode scalar-value index into the corresponding byte index
/// within `s`. Returns `s.len()` when `char_idx` is past the end.
fn char_to_byte_idx(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod state {
    use crate::app::DisplayMessage;
    use crate::app::test_helpers::{agent_event, make_app};
    use ein_proto::ein::{
        AgentFinished, ContentDelta, TokenUsage, ToolOutputChunk, agent_event::Event as ProtoEvent,
    };

    use super::*;

    /// Push a ToolCall placeholder so ToolOutputChunk has somewhere to land.
    fn push_tool_call(app: &mut App) {
        app.messages.push(DisplayMessage::ToolCall {
            name: "Bash".to_string(),
            arg: None,
            output_lines: vec![],
        });
    }

    fn output_lines(app: &App) -> &Vec<String> {
        match app.messages.last().unwrap() {
            DisplayMessage::ToolCall { output_lines, .. } => output_lines,
            _ => panic!("expected ToolCall"),
        }
    }

    #[test]
    fn tool_output_chunk_splits_on_newlines() {
        let mut app = make_app("m");
        push_tool_call(&mut app);
        handle_server_event(
            &mut app,
            agent_event(ProtoEvent::ToolOutputChunk(ToolOutputChunk {
                tool_call_id: String::new(),
                output: "a\nb\nc\n".to_string(),
            })),
        );
        let lines = output_lines(&app);
        assert_eq!(lines.len(), 4, "split on \\n produces 4 entries");
        assert!(
            lines.iter().all(|l| !l.contains('\n')),
            "no entry should contain \\n"
        );
    }

    #[test]
    fn tool_output_chunk_single_line() {
        let mut app = make_app("m");
        push_tool_call(&mut app);
        handle_server_event(
            &mut app,
            agent_event(ProtoEvent::ToolOutputChunk(ToolOutputChunk {
                tool_call_id: String::new(),
                output: "hello".to_string(),
            })),
        );
        assert_eq!(output_lines(&app).len(), 1);
        assert_eq!(output_lines(&app)[0], "hello");
    }

    #[test]
    fn agent_finished_clears_busy() {
        let mut app = make_app("m");
        app.agent_busy = true;
        app.auto_scroll = false;
        handle_server_event(
            &mut app,
            agent_event(ProtoEvent::AgentFinished(AgentFinished {
                final_content: String::new(),
            })),
        );
        assert!(!app.agent_busy);
        assert!(app.auto_scroll);
    }

    #[test]
    fn content_delta_coalesces() {
        let mut app = make_app("m");
        handle_server_event(
            &mut app,
            agent_event(ProtoEvent::ContentDelta(ContentDelta {
                text: "hello ".to_string(),
            })),
        );
        handle_server_event(
            &mut app,
            agent_event(ProtoEvent::ContentDelta(ContentDelta {
                text: "world".to_string(),
            })),
        );
        let agent_texts: Vec<_> = app
            .messages
            .iter()
            .filter_map(|m| {
                if let DisplayMessage::AgentText(t) = m {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            agent_texts.len(),
            1,
            "two ContentDeltas should coalesce into one AgentText"
        );
        assert_eq!(agent_texts[0], "hello world");
    }

    #[test]
    fn token_usage_updates_cumulative() {
        let mut app = make_app("m");
        handle_server_event(
            &mut app,
            agent_event(ProtoEvent::TokenUsage(TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 32,
                total_tokens: 42,
            })),
        );
        assert_eq!(app.cumulative_tokens, 42);
    }
}

#[cfg(test)]
mod key_events {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::app::{DisplayMessage, test_helpers::make_app};

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    // ---------------------------------------------------------------------------
    // Ctrl-C and slash commands
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn ctrl_c_always_quits() {
        let mut app = make_app("m");
        let action = handle_key_event(&mut app, ctrl(KeyCode::Char('c'))).await;
        assert!(matches!(action, KeyAction::Quit));
    }

    #[tokio::test]
    async fn exit_command_quits() {
        let mut app = make_app("m");
        app.input = "/exit".to_string();
        let action = handle_key_event(&mut app, key(KeyCode::Enter)).await;
        assert!(matches!(action, KeyAction::Quit));
    }

    #[tokio::test]
    async fn new_command_returns_new_session() {
        let mut app = make_app("m");
        app.input = "/new".to_string();
        let action = handle_key_event(&mut app, key(KeyCode::Enter)).await;
        assert!(matches!(action, KeyAction::NewSession));
    }

    #[tokio::test]
    async fn sessions_command_opens_picker() {
        let mut app = make_app("m");
        app.input = "/sessions".to_string();
        let action = handle_key_event(&mut app, key(KeyCode::Enter)).await;
        assert!(matches!(action, KeyAction::OpenSessionPicker));
    }

    #[tokio::test]
    async fn plugins_command_opens_plugin_modal() {
        let mut app = make_app("m");
        app.input = "/plugins".to_string();
        let action = handle_key_event(&mut app, key(KeyCode::Enter)).await;
        assert!(matches!(action, KeyAction::OpenPluginModal));
    }

    #[tokio::test]
    async fn unknown_slash_command_shows_error_message() {
        let mut app = make_app("m");
        app.input = "/doesnotexist".to_string();
        let action = handle_key_event(&mut app, key(KeyCode::Enter)).await;
        assert!(matches!(action, KeyAction::Continue));
        assert!(
            app.messages
                .iter()
                .any(|m| matches!(m, DisplayMessage::Error(_))),
            "unknown slash command must add an Error message"
        );
    }

    #[tokio::test]
    async fn enter_with_empty_input_is_a_no_op() {
        let mut app = make_app("m");
        let initial_msg_count = app.messages.len();
        let action = handle_key_event(&mut app, key(KeyCode::Enter)).await;
        assert!(matches!(action, KeyAction::Continue));
        assert_eq!(app.messages.len(), initial_msg_count);
    }

    // ---------------------------------------------------------------------------
    // Text input editing
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn char_key_appends_to_input_and_advances_cursor() {
        let mut app = make_app("m");
        let _ = handle_key_event(&mut app, key(KeyCode::Char('h'))).await;
        let _ = handle_key_event(&mut app, key(KeyCode::Char('i'))).await;
        assert_eq!(app.input, "hi");
        assert_eq!(app.cursor_pos, 2);
    }

    #[tokio::test]
    async fn backspace_removes_last_char() {
        let mut app = make_app("m");
        app.input = "hello".to_string();
        app.cursor_pos = 5;
        let _ = handle_key_event(&mut app, key(KeyCode::Backspace)).await;
        assert_eq!(app.input, "hell");
        assert_eq!(app.cursor_pos, 4);
    }

    #[tokio::test]
    async fn backspace_at_start_of_input_is_a_no_op() {
        let mut app = make_app("m");
        app.input = "hi".to_string();
        app.cursor_pos = 0;
        let _ = handle_key_event(&mut app, key(KeyCode::Backspace)).await;
        assert_eq!(app.input, "hi");
        assert_eq!(app.cursor_pos, 0);
    }

    #[tokio::test]
    async fn enter_clears_input_after_command() {
        let mut app = make_app("m");
        app.input = "/exit".to_string();
        let _ = handle_key_event(&mut app, key(KeyCode::Enter)).await;
        // input was consumed (taken) by the Enter handler
        assert!(app.input.is_empty());
    }

    // ---------------------------------------------------------------------------
    // Scrolling
    // ---------------------------------------------------------------------------

    #[tokio::test]
    async fn up_arrow_increments_scroll_offset_and_disables_autoscroll() {
        let mut app = make_app("m");
        app.auto_scroll = true;
        app.scroll_offset = 0;
        let _ = handle_key_event(&mut app, key(KeyCode::Up)).await;
        assert_eq!(app.scroll_offset, 1);
        assert!(!app.auto_scroll);
    }

    #[tokio::test]
    async fn down_arrow_at_bottom_re_enables_autoscroll() {
        let mut app = make_app("m");
        app.scroll_offset = 0;
        app.auto_scroll = false;
        let _ = handle_key_event(&mut app, key(KeyCode::Down)).await;
        assert!(app.auto_scroll);
    }

    #[tokio::test]
    async fn down_arrow_above_bottom_decrements_scroll_offset() {
        let mut app = make_app("m");
        app.scroll_offset = 5;
        let _ = handle_key_event(&mut app, key(KeyCode::Down)).await;
        assert_eq!(app.scroll_offset, 4);
    }

    // ---------------------------------------------------------------------------
    // update_autocomplete
    // ---------------------------------------------------------------------------

    #[test]
    fn autocomplete_activates_for_slash_prefix() {
        let mut app = make_app("m");
        app.input = "/ex".to_string();
        update_autocomplete(&mut app);
        assert!(app.autocomplete_active);
        assert!(!app.autocomplete_matches.is_empty());
    }

    #[test]
    fn autocomplete_slash_alone_matches_all_commands() {
        let mut app = make_app("m");
        app.input = "/".to_string();
        update_autocomplete(&mut app);
        assert!(app.autocomplete_active);
        assert_eq!(app.autocomplete_matches.len(), COMMANDS.len());
    }

    #[test]
    fn autocomplete_deactivates_for_non_slash_input() {
        let mut app = make_app("m");
        app.input = "hello".to_string();
        app.autocomplete_active = true;
        app.autocomplete_matches = vec![0];
        update_autocomplete(&mut app);
        assert!(!app.autocomplete_active);
        assert!(app.autocomplete_matches.is_empty());
    }

    #[test]
    fn autocomplete_no_match_leaves_inactive() {
        let mut app = make_app("m");
        app.input = "/zzz".to_string();
        update_autocomplete(&mut app);
        assert!(!app.autocomplete_active);
    }

    // ---------------------------------------------------------------------------
    // char_to_byte_idx (private helper, accessible from same-file test module)
    // ---------------------------------------------------------------------------

    #[test]
    fn char_to_byte_idx_ascii() {
        assert_eq!(char_to_byte_idx("hello", 0), 0);
        assert_eq!(char_to_byte_idx("hello", 3), 3);
        assert_eq!(char_to_byte_idx("hello", 5), 5);
    }

    #[test]
    fn char_to_byte_idx_multibyte() {
        let s = "héllo"; // é is 2 bytes (U+00E9)
        assert_eq!(char_to_byte_idx(s, 0), 0);
        assert_eq!(char_to_byte_idx(s, 1), 1); // 'h'
        assert_eq!(char_to_byte_idx(s, 2), 3); // after 'é' (2 bytes)
    }

    #[test]
    fn char_to_byte_idx_past_end_returns_len() {
        assert_eq!(char_to_byte_idx("hi", 99), 2);
    }
}
