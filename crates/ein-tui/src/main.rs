mod config;

use std::sync::OnceLock;

use crate::config::load_or_create_config;
use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ein_proto::ein::{
    AgentEvent, SessionConfig, UserInput, agent_client::AgentClient,
    agent_event::Event as ServerEvent, user_input,
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use syntect::{
    easy::HighlightLines,
    highlighting::{Style as SyntectStyle, ThemeSet},
    parsing::SyntaxSet,
};
use tokio::sync::mpsc;
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tonic::transport::Channel;

// ---------------------------------------------------------------------------
// Color palette
//
// Centralised here so tweaks only need one edit.
// ---------------------------------------------------------------------------

/// Border color for the input area — a muted dark-peach/terracotta.
const INPUT_BORDER_COLOR: Color = Color::Rgb(180, 115, 90);

/// Color used for the tool-call indicator (▸) and tool name — muted steel blue.
const TOOL_NAME_COLOR: Color = Color::Rgb(110, 150, 180);

/// Color used for the thinking spinner and label — soft sky blue.
const THINKING_COLOR: Color = Color::Rgb(140, 180, 200);

/// Muted grey used for secondary text: tool args, autocomplete labels, etc.
const MUTED_COLOR: Color = Color::DarkGray;

/// Muted white used for the top autocomplete suggestion.
const AUTOCOMPLETE_TOP_COLOR: Color = Color::Rgb(180, 180, 180);

/// Color used for the disconnected/connecting indicator — muted red.
const DISCONNECTED_COLOR: Color = Color::Rgb(200, 80, 80);

/// Color used for added lines in Edit diffs — muted green.
const DIFF_ADD_COLOR: Color = Color::Rgb(100, 170, 100);

/// Color used for removed lines in Edit diffs — muted red.
const DIFF_DEL_COLOR: Color = Color::Rgb(190, 90, 90);

/// Maximum number of removed or added lines shown in an Edit diff.
const DIFF_MAX_LINES: usize = 5;

// ---------------------------------------------------------------------------
// Syntax highlighting (lazily initialised)
// ---------------------------------------------------------------------------

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme_set() -> &'static ThemeSet {
    THEME_SET.get_or_init(ThemeSet::load_defaults)
}

/// Highlight one line of code and return it as a list of coloured `Span`s.
///
/// `h` is the stateful highlighter — callers must reuse the same instance
/// across all lines in a block so multi-line constructs are tracked correctly.
/// Each `line` should *not* include a trailing newline; one is appended
/// internally so syntect can detect end-of-line correctly.
fn highlight_line_spans(h: &mut HighlightLines, ps: &SyntaxSet, line: &str) -> Vec<Span<'static>> {
    let with_newline = format!("{line}\n");
    match h.highlight_line(&with_newline, ps) {
        Ok(ranges) => ranges
            .iter()
            .filter_map(|(style, text)| {
                let t = text.trim_end_matches('\n').to_string();
                if t.is_empty() {
                    None
                } else {
                    let SyntectStyle { foreground: c, .. } = style;
                    Some(Span::styled(t, Style::default().fg(Color::Rgb(c.r, c.g, c.b))))
                }
            })
            .collect(),
        Err(_) => vec![Span::raw(line.to_string())],
    }
}

// ---------------------------------------------------------------------------
// Command registry
//
// Every slash command available in the TUI is declared here. The autocomplete
// panel reads from this list at render time, filtering by the current input
// prefix, so adding a new command only requires appending an entry below.
// ---------------------------------------------------------------------------

struct CommandDef {
    name: &'static str,
    description: &'static str,
}

const COMMANDS: &[CommandDef] = &[
    CommandDef {
        name: "/exit",
        description: "Exit Ein",
    },
    CommandDef {
        name: "/config",
        description: "Edit ~/.ein/config.json",
    },
];

// ---------------------------------------------------------------------------
// Connection state
// ---------------------------------------------------------------------------

/// All events delivered to the main TUI select! loop.
enum AppEvent {
    /// A normal event streamed from the server.
    Server(AgentEvent),
    /// Connection was established; carries the sender for outbound prompts.
    Connected(mpsc::Sender<UserInput>),
    /// Connection was lost or a connection attempt failed.
    /// `Some(msg)` only when a session was already active (shown in conversation).
    Disconnected(Option<String>),
}

/// Whether the TUI currently has a live server connection.
enum ConnectionStatus {
    Connecting,
    Connected,
}

// ---------------------------------------------------------------------------
// Display model
//
// Each variant represents one logical block in the conversation transcript.
// ---------------------------------------------------------------------------

enum DisplayMessage {
    /// Text sent by the local user.
    User(String),
    /// Streamed text from the agent (may be appended to incrementally).
    AgentText(String),
    /// A tool invocation: (tool_name, primary_arg). The arg is the most
    /// meaningful single parameter for display (e.g. the shell command for
    /// Bash, the file path for Read/Write).
    ToolCall(String, Option<String>),
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
// App state
// ---------------------------------------------------------------------------

struct App {
    /// All messages rendered in the conversation pane.
    messages: Vec<DisplayMessage>,
    /// Current contents of the input field.
    input: String,
    /// Cursor position within `input`, counted in Unicode scalar values.
    cursor_pos: usize,
    /// True while the server is processing; disables text input.
    agent_busy: bool,
    /// How many lines the user has scrolled up from the bottom.
    scroll_offset: u16,
    /// When true the view follows new output automatically.
    auto_scroll: bool,
    /// True when the autocomplete panel should show filtered results.
    autocomplete_active: bool,
    /// Indices into `COMMANDS` that match the current input prefix.
    autocomplete_matches: Vec<usize>,
    /// Frame counter incremented by the animation ticker while busy.
    tick: u64,
    /// Short model name for the status bar (vendor prefix stripped).
    model_display: String,
    /// Cumulative tokens used this session (updated on each TokenUsage event).
    cumulative_tokens: i32,
    /// Current server connection state; drives status bar and input gating.
    connection_status: ConnectionStatus,
    /// Outbound prompt channel; `None` while disconnected.
    prompt_tx: Option<mpsc::Sender<UserInput>>,
    /// Last connection error message, shown above the connecting spinner.
    /// Replaced in-place on each disconnect; cleared when connected.
    connection_error: Option<String>,
    /// If `Some`, the startup modal is visible asking to allow access to this directory.
    pending_cwd_prompt: Option<String>,
}

impl App {
    fn new(model_display: String, pending_cwd_prompt: Option<String>) -> Self {
        Self {
            messages: vec![],
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
            pending_cwd_prompt,
        }
    }
}

// ---------------------------------------------------------------------------
// Autocomplete
// ---------------------------------------------------------------------------

/// Recomputes `autocomplete_matches` and `autocomplete_active` based on the
/// current input. Called after every keystroke that modifies `app.input`.
fn update_autocomplete(app: &mut App) {
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
// Rendering
// ---------------------------------------------------------------------------

/// Braille spinner frames for the thinking animation.
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Full render pass — called every time the terminal is redrawn.
///
/// Layout (top → bottom):
///   ┌─────────────────────────┐
///   │  Conversation pane      │  grows to fill available space
///   ├─────────────────────────┤  INPUT_BORDER_COLOR top border
///   │  Input area             │  expands vertically as text wraps
///   ├─────────────────────────┤  INPUT_BORDER_COLOR bottom border
///   │  Autocomplete section   │  always 3 lines tall, expands for results
///   └─────────────────────────┘
fn render(app: &App, frame: &mut Frame) {
    // Autocomplete section: minimum 3 lines so it always reserves space even
    // when empty; grows to show up to 5 results.
    let autocomplete_height = (app.autocomplete_matches.len().min(5) as u16).max(3);

    // Input area: pre-wrap by character so cursor math and render agree.
    // Uses the full terminal width (no left/right borders on the input block).
    let terminal_width = frame.area().width as usize;
    let input_chars: Vec<char> = format!("> {}", app.input).chars().collect();
    let input_content_lines = (input_chars.len().saturating_sub(1) / terminal_width + 1) as u16;
    let input_height = input_content_lines + 2; // +2 for top and bottom borders

    let layout = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(input_height),
        Constraint::Length(autocomplete_height),
        Constraint::Length(1),
    ])
    .split(frame.area());

    // --- Conversation pane ---
    // Build static message lines then, if the agent is busy, append an
    // animated spinner so the user can see activity without the input area
    // being taken over.
    let mut lines = build_lines(&app.messages);
    let frame_idx = (app.tick as usize) % SPINNER.len();
    if app.agent_busy {
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", SPINNER[frame_idx]),
                Style::default().fg(THINKING_COLOR),
            ),
            Span::styled(
                "thinking",
                Style::default()
                    .fg(THINKING_COLOR)
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
    } else if matches!(app.connection_status, ConnectionStatus::Connecting) {
        if let Some(err) = &app.connection_error {
            lines.push(Line::from(Span::styled(
                format!(" {err}"),
                Style::default().fg(DISCONNECTED_COLOR),
            )));
            lines.push(Line::raw(""));
        }
        lines.push(Line::from(vec![
            Span::styled(" ● ", Style::default().fg(DISCONNECTED_COLOR)),
            Span::styled(SPINNER[frame_idx], Style::default().fg(MUTED_COLOR)),
            Span::styled(
                "  connecting to server",
                Style::default()
                    .fg(MUTED_COLOR)
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
    }

    let total_lines = lines.len() as u16;
    let viewport_height = layout[0].height;

    // scroll_offset counts lines scrolled *up* from the bottom, so the
    // ratatui scroll value (lines from the top) is the inverse.
    let scroll = if app.auto_scroll {
        total_lines.saturating_sub(viewport_height)
    } else {
        total_lines
            .saturating_sub(viewport_height)
            .saturating_sub(app.scroll_offset)
    };

    let conv = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    frame.render_widget(conv, layout[0]);

    // --- Input area ---
    // Text is pre-wrapped into fixed-width chunks so cursor positioning and
    // the rendered text always agree on line boundaries.
    let input_lines: Vec<Line> = input_chars
        .chunks(terminal_width)
        .map(|chunk| Line::raw(chunk.iter().collect::<String>()))
        .collect();
    let input = Paragraph::new(input_lines).block(
        Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(INPUT_BORDER_COLOR)),
    );
    frame.render_widget(input, layout[1]);

    // Place the terminal cursor at the correct position within the (possibly
    // wrapped) input area. cursor_abs accounts for the "> " prefix.
    if !app.agent_busy {
        let cursor_abs = 2 + app.cursor_pos;
        let cursor_row = (cursor_abs / terminal_width) as u16;
        let cursor_col = (cursor_abs % terminal_width) as u16;
        frame.set_cursor_position((layout[1].x + cursor_col, layout[1].y + 1 + cursor_row));
    }

    // --- Autocomplete section ---
    // Rendered below the input with no borders. Shows filtered command names
    // and descriptions in muted grey so it acts as a reference, not a focus
    // target.
    if app.autocomplete_active {
        let items: Vec<ListItem> = app
            .autocomplete_matches
            .iter()
            .enumerate()
            .map(|(i, &idx)| {
                let cmd = &COMMANDS[idx];
                let color = if i == 0 {
                    AUTOCOMPLETE_TOP_COLOR
                } else {
                    MUTED_COLOR
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {}", cmd.name),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  {}", cmd.description), Style::default().fg(color)),
                ]))
            })
            .collect();

        frame.render_widget(List::new(items), layout[2]);
    }

    // --- CWD access modal (overlays everything when present) ---
    if let Some(cwd) = &app.pending_cwd_prompt {
        render_cwd_modal(cwd, frame);
    }

    // --- Status bar ---
    let status_text = match app.connection_status {
        ConnectionStatus::Connecting => format!(" model: {}", app.model_display),
        ConnectionStatus::Connected => format!(
            " model: {} | tokens: {}",
            app.model_display, app.cumulative_tokens
        ),
    };
    let status = Paragraph::new(status_text).style(Style::default().fg(MUTED_COLOR));
    frame.render_widget(status, layout[3]);
}

/// Converts the message log into a flat list of styled ratatui `Line`s.
fn build_lines(messages: &[DisplayMessage]) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for msg in messages {
        match msg {
            DisplayMessage::User(text) => {
                lines.push(Line::from(vec![
                    Span::styled("You: ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(text.clone()),
                ]));
                lines.push(Line::raw(""));
            }
            DisplayMessage::AgentText(text) => {
                for line in text.lines() {
                    lines.push(Line::raw(line.to_string()));
                }
                lines.push(Line::raw(""));
            }
            DisplayMessage::ToolCall(name, arg) => {
                // "▸ ToolName  arg" — indicator and name in steel blue, arg muted.
                let mut spans = vec![
                    Span::styled(" ▸ ", Style::default().fg(TOOL_NAME_COLOR)),
                    Span::styled(
                        name.clone(),
                        Style::default()
                            .fg(TOOL_NAME_COLOR)
                            .add_modifier(Modifier::BOLD),
                    ),
                ];
                if let Some(a) = arg {
                    spans.push(Span::styled(
                        format!("  {}", a),
                        Style::default().fg(MUTED_COLOR),
                    ));
                }
                lines.push(Line::from(spans));
                lines.push(Line::raw(""));
            }
            DisplayMessage::EditCall {
                file_path,
                start_line,
                old_lines,
                new_lines,
            } => {
                // Header: "▸ Edit  file_path"
                lines.push(Line::from(vec![
                    Span::styled(" ▸ ", Style::default().fg(TOOL_NAME_COLOR)),
                    Span::styled(
                        "Edit",
                        Style::default()
                            .fg(TOOL_NAME_COLOR)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {file_path}"),
                        Style::default().fg(MUTED_COLOR),
                    ),
                ]));

                let ps = syntax_set();
                let ts = theme_set();
                let ext = std::path::Path::new(file_path.as_str())
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("");
                let syntax = ps
                    .find_syntax_by_extension(ext)
                    .unwrap_or_else(|| ps.find_syntax_plain_text());

                // Removed lines — muted red gutter, syntax-highlighted code.
                let mut h_old = HighlightLines::new(syntax, &ts.themes["base16-ocean.dark"]);
                for (i, code) in old_lines.iter().take(DIFF_MAX_LINES).enumerate() {
                    let line_num = start_line + i as u32;
                    let mut spans = vec![
                        Span::styled(
                            format!("  {:>4} ", line_num),
                            Style::default().fg(DIFF_DEL_COLOR),
                        ),
                        Span::styled("- ", Style::default().fg(DIFF_DEL_COLOR)),
                    ];
                    spans.extend(highlight_line_spans(&mut h_old, ps, code));
                    lines.push(Line::from(spans));
                }
                if old_lines.len() > DIFF_MAX_LINES {
                    lines.push(Line::from(Span::styled(
                        "       - …",
                        Style::default().fg(DIFF_DEL_COLOR),
                    )));
                }

                // Added lines — muted green gutter, syntax-highlighted code.
                let mut h_new = HighlightLines::new(syntax, &ts.themes["base16-ocean.dark"]);
                for (i, code) in new_lines.iter().take(DIFF_MAX_LINES).enumerate() {
                    let line_num = start_line + i as u32;
                    let mut spans = vec![
                        Span::styled(
                            format!("  {:>4} ", line_num),
                            Style::default().fg(DIFF_ADD_COLOR),
                        ),
                        Span::styled("+ ", Style::default().fg(DIFF_ADD_COLOR)),
                    ];
                    spans.extend(highlight_line_spans(&mut h_new, ps, code));
                    lines.push(Line::from(spans));
                }
                if new_lines.len() > DIFF_MAX_LINES {
                    lines.push(Line::from(Span::styled(
                        "       + …",
                        Style::default().fg(DIFF_ADD_COLOR),
                    )));
                }

                lines.push(Line::raw(""));
            }
            DisplayMessage::Error(msg) => {
                lines.push(Line::from(Span::styled(
                    format!("Error: {}", msg),
                    Style::default().fg(Color::Red),
                )));
                lines.push(Line::raw(""));
            }
        }
    }
    lines
}

// ---------------------------------------------------------------------------
// CWD modal
// ---------------------------------------------------------------------------

/// Returns a centered `Rect` of the requested size within `area`.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

/// Renders the startup modal asking the user whether to allow access to the
/// current working directory. Overlays the entire terminal using `Clear`.
fn render_cwd_modal(cwd: &str, frame: &mut Frame) {
    let modal_width = (frame.area().width * 7 / 10)
        .max(50)
        .min(frame.area().width);
    let modal_height = 7u16;
    let area = centered_rect(modal_width, modal_height, frame.area());

    // Clear the area behind the modal so the chat pane doesn't show through.
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Allow directory access? ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(INPUT_BORDER_COLOR));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Truncate the path with an ellipsis if it's wider than the inner area.
    let max_path_len = inner.width.saturating_sub(2) as usize;
    let display_path = if cwd.len() > max_path_len && max_path_len > 3 {
        format!("…{}", &cwd[cwd.len() - max_path_len + 1..])
    } else {
        cwd.to_string()
    };

    let lines = vec![
        Line::raw(""),
        Line::from(Span::styled(
            format!(" {display_path}"),
            Style::default().fg(AUTOCOMPLETE_TOP_COLOR),
        )),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                " [Y]",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Allow   ", Style::default().fg(MUTED_COLOR)),
            Span::styled(
                "[N]",
                Style::default()
                    .fg(DISCONNECTED_COLOR)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Deny", Style::default().fg(MUTED_COLOR)),
        ]),
    ];

    frame.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extracts the most useful display argument for a known tool from its raw
/// JSON arguments string. Returns `(tool_name, Option<primary_arg>)`.
///
/// - `Bash`        → `command` field
/// - `Read` / `Write` / `Edit` → `file_path` field
/// - unknown       → no arg shown
///
/// For `Edit` this is a temporary placeholder shown while the tool runs; it is
/// replaced by a full `EditCall` message when `ToolCallEnd` arrives.
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
// Server event handler
// ---------------------------------------------------------------------------

/// Applies a single server-pushed `AgentEvent` to the local app state.
///
/// `ContentDelta` events are coalesced into the last `AgentText` message
/// so that streaming output appears as one continuously growing block rather
/// than many small fragments.
fn handle_server_event(app: &mut App, event: ein_proto::ein::AgentEvent) {
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
            let (name, arg) = parse_tool_call(&t.tool_name, &t.arguments);
            app.messages.push(DisplayMessage::ToolCall(name, arg));
            app.auto_scroll = true;
        }
        Some(ServerEvent::ToolCallEnd(t)) => {
            // For Edit calls the tool returns diff metadata; replace the
            // ToolCall placeholder that was pushed at ToolCallStart.
            if t.tool_name == "Edit" && !t.metadata.is_empty() {
                if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&t.metadata) {
                    let start_line = meta
                        .get("start_line")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(1) as u32;
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
                        if let DisplayMessage::ToolCall(name, file_path_opt) = msg {
                            if name == "Edit" {
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
            }
        }
        Some(ServerEvent::AgentFinished(f)) => {
            if !f.final_content.is_empty() {
                app.messages
                    .push(DisplayMessage::AgentText(f.final_content));
            }
            app.agent_busy = false;
            app.auto_scroll = true;
        }
        Some(ServerEvent::AgentError(e)) => {
            app.messages.push(DisplayMessage::Error(e.message));
            app.agent_busy = false;
            app.auto_scroll = true;
        }
        Some(ServerEvent::TokenUsage(u)) => {
            app.cumulative_tokens = u.total_tokens;
        }
        None => {}
    }
}

// ---------------------------------------------------------------------------
// Connection management
// ---------------------------------------------------------------------------

/// Attempts one full connect → session → bridge cycle.
///
/// Returns `Ok(true)` if a session was live at some point (so the caller can
/// decide whether a subsequent failure warrants a visible error message),
/// `Ok(false)` if the TUI event channel closed (TUI exited — stop retrying),
/// or `Err` if the initial connection or handshake failed.
async fn try_connect(
    server_addr: &str,
    cfg: &config::ClientConfig,
    event_tx: &mpsc::Sender<AppEvent>,
) -> anyhow::Result<bool> {
    let channel = Channel::from_shared(server_addr.to_string())?
        .connect()
        .await?;
    let mut grpc_client = AgentClient::new(channel);

    let (prompt_tx, prompt_rx) = mpsc::channel::<UserInput>(8);
    let prompt_stream = ReceiverStream::new(prompt_rx);

    let response = grpc_client
        .agent_session(tonic::Request::new(prompt_stream))
        .await?;
    let mut server_stream = response.into_inner();

    // Send SessionConfig as the mandatory first message before any prompts.
    prompt_tx
        .send(UserInput {
            input: Some(user_input::Input::Init(SessionConfig {
                allowed_paths: cfg.allowed_paths.clone(),
                allowed_hosts: cfg.allowed_hosts.clone(),
                model: cfg.model.clone(),
                max_tokens: cfg.max_tokens,
            })),
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
async fn connection_manager(
    server_addr: String,
    cfg: config::ClientConfig,
    event_tx: mpsc::Sender<AppEvent>,
) {
    loop {
        match try_connect(&server_addr, &cfg, &event_tx).await {
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

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Optional server address as first CLI argument; defaults to localhost.
    let server_addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://localhost:50051".to_string());

    // Load (or create) the client config before opening the gRPC session.
    let mut cfg = load_or_create_config()?;

    // Derive a short model name for the status bar by stripping the vendor
    // prefix (e.g. "anthropic/claude-haiku-4.5" → "claude-haiku-4.5").
    let model_display = cfg
        .model
        .split_once('/')
        .map(|(_, m)| m.to_string())
        .unwrap_or_else(|| cfg.model.clone());

    // Collect the cwd for the startup modal (shown before connecting).
    let cwd_str = std::env::current_dir()
        .ok()
        .map(|p| p.display().to_string());

    let (event_tx, mut event_rx) = mpsc::channel::<AppEvent>(64);

    // If there is no cwd to prompt about, spawn the connection manager immediately.
    // Otherwise it is spawned when the modal is dismissed.
    if cwd_str.is_none() {
        tokio::spawn(connection_manager(
            server_addr.clone(),
            cfg.clone(),
            event_tx.clone(),
        ));
    }

    // Configure the terminal for raw / alternate-screen rendering.
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(model_display, cwd_str);
    let mut term_events = EventStream::new();
    // Ticker drives the thinking spinner; only app.tick is incremented when
    // the agent is busy, so the timer is cheap when idle.
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(80));

    loop {
        terminal.draw(|f| render(&app, f))?;

        tokio::select! {
            _ = ticker.tick() => {
                if app.agent_busy || matches!(app.connection_status, ConnectionStatus::Connecting) {
                    app.tick = app.tick.wrapping_add(1);
                }
            }

            Some(Ok(event)) = term_events.next() => {
                let Event::Key(key) = event else { continue };
                if key.kind != KeyEventKind::Press { continue; }

                // Ctrl-C always exits immediately, even while the agent is busy.
                if key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    break;
                }

                // While the startup modal is showing, intercept Y/N and dismiss it.
                if app.pending_cwd_prompt.is_some() {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => {
                            let cwd = app.pending_cwd_prompt.take().unwrap();
                            cfg.allowed_paths.push(cwd);
                            tokio::spawn(connection_manager(
                                server_addr.clone(),
                                cfg.clone(),
                                event_tx.clone(),
                            ));
                        }
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Enter | KeyCode::Esc => {
                            app.pending_cwd_prompt = None;
                            tokio::spawn(connection_manager(
                                server_addr.clone(),
                                cfg.clone(),
                                event_tx.clone(),
                            ));
                        }
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Enter => {
                        if app.agent_busy || app.input.is_empty() {
                            continue;
                        }
                        let text = std::mem::take(&mut app.input);
                        app.cursor_pos = 0;
                        app.autocomplete_active = false;
                        app.autocomplete_matches.clear();

                        // Slash commands work regardless of connection state.
                        if text == "/exit" {
                            break;
                        }

                        if text == "/config" {
                            if let Some(path) =
                                dirs::home_dir().map(|h| h.join(".ein").join("config.json"))
                            {
                                let editor =
                                    std::env::var("EDITOR").unwrap_or_else(|_| "nano".to_string());
                                disable_raw_mode()?;
                                execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
                                let _ = std::process::Command::new(&editor).arg(&path).status();
                                enable_raw_mode()?;
                                execute!(terminal.backend_mut(), EnterAlternateScreen)?;
                                terminal.clear()?;
                            }
                            continue;
                        }

                        // Prompts require an active connection.
                        if app.prompt_tx.is_none() {
                            continue;
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
                            update_autocomplete(&mut app);
                        }
                    }
                    KeyCode::Backspace => {
                        if !app.agent_busy && app.cursor_pos > 0 {
                            let byte_end = char_to_byte_idx(&app.input, app.cursor_pos);
                            let byte_start = char_to_byte_idx(&app.input, app.cursor_pos - 1);
                            app.input.drain(byte_start..byte_end);
                            app.cursor_pos -= 1;
                            update_autocomplete(&mut app);
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
            }

            Some(app_event) = event_rx.recv() => {
                match app_event {
                    AppEvent::Server(event) => handle_server_event(&mut app, event),
                    AppEvent::Connected(sender) => {
                        app.prompt_tx = Some(sender);
                        app.connection_status = ConnectionStatus::Connected;
                        app.cumulative_tokens = 0;
                        app.connection_error = None;
                    }
                    AppEvent::Disconnected(msg) => {
                        app.connection_error = msg;
                        app.prompt_tx = None;
                        app.connection_status = ConnectionStatus::Connecting;
                        app.agent_busy = false;
                        app.auto_scroll = true;
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
