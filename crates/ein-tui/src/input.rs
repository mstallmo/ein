// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ein_proto::ein::{UserInput, agent_event::Event as ServerEvent, user_input};
use tracing::{debug, info, warn};

use crate::app::{App, CwdState, DisplayMessage, SessionPickerState};
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

    // While the session picker is visible, route all key events to it.
    if app.pending_session_picker.is_some() {
        return handle_session_picker_key(app, key).await;
    }

    // While the CWD modal is visible (only for new sessions), intercept Y/N.
    if app.pending_cwd_prompt.is_some() {
        return handle_cwd_modal_key(app, key);
    }

    handle_normal_key(app, key).await
}

async fn handle_session_picker_key(app: &mut App, key: KeyEvent) -> KeyAction {
    let picker = app.pending_session_picker.as_mut().unwrap();
    match key.code {
        KeyCode::Up => {
            if picker.selected > 0 {
                picker.selected -= 1;
            }
        }
        KeyCode::Down => {
            if picker.selected < picker.sessions.len() {
                picker.selected += 1;
            }
        }
        KeyCode::Enter => {
            let state = app.pending_session_picker.take().unwrap();
            if state.selected == 0 {
                // "New Session" — build config from current settings.
                let base = to_proto_session_config(&app.current_cfg, String::new());
                if let Some(cwd) = app.cwd.clone() {
                    // Show the CWD modal before sending the config.
                    app.pending_cwd_prompt = Some(CwdState {
                        cwd,
                        base_config: base,
                        session_tx: state.session_tx,
                    });
                } else {
                    let _ = state.session_tx.send(base);
                }
            } else {
                // Resume existing session using its stored config.
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

fn handle_cwd_modal_key(app: &mut App, key: KeyEvent) -> KeyAction {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let state = app.pending_cwd_prompt.take().unwrap();
            let mut config = state.base_config;
            config.allowed_paths.push(state.cwd);
            let _ = state.session_tx.send(config);
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Enter | KeyCode::Esc => {
            let state = app.pending_cwd_prompt.take().unwrap();
            let _ = state.session_tx.send(state.base_config);
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
            if text == "/exit" {
                return KeyAction::Quit;
            }

            if text == "/config" {
                if let Some(path) = dirs::home_dir().map(|h| h.join(".ein").join("config.json")) {
                    return KeyAction::OpenConfig(path);
                }
                return KeyAction::Continue;
            }

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
            let (name, arg) = parse_tool_call(&t.tool_name, &t.arguments);

            app.messages.push(DisplayMessage::ToolCall {
                name,
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
                                let (name, arg) = parse_tool_call(&tc.tool_name, &tc.arguments);
                                app.messages.push(DisplayMessage::ToolCall {
                                    name,
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

// TODO: Extract this logic into the gRPC protocl and allow tools to define their
// own display format for frontends. It will ultimately be up to the frontend
// to decide how to display the information but the content should be defined by
// the tool.
/// Extracts the most useful display argument for a known tool from its raw
/// JSON arguments string. Returns `(tool_name, Option<primary_arg>)`.
///
/// - `Bash`               → `command` field
/// - `Read` / `Write` / `Edit` → `file_path` field
/// - unknown              → no arg shown
fn parse_tool_call(name: &str, arguments: &str) -> (String, Option<String>) {
    let args: serde_json::Value =
        serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);

    let arg = match name {
        "Bash" => args
            .get("command")
            .and_then(|v| v.as_str())
            .map(String::from),
        "Read" | "Write" | "Edit" => args
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(String::from),
        _ => None,
    };

    (name.to_string(), arg)
}

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
