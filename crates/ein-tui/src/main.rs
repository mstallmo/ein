use crossterm::{
    event::{Event, EventStream, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ein_proto::ein::{UserInput, agent_client::AgentClient, agent_event::Event as ServerEvent};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
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
        description: "Exit the TUI",
    },
];

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
}

impl App {
    fn new() -> Self {
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
    ])
    .split(frame.area());

    // --- Conversation pane ---
    // Build static message lines then, if the agent is busy, append an
    // animated spinner so the user can see activity without the input area
    // being taken over.
    let mut lines = build_lines(&app.messages);
    if app.agent_busy {
        let frame_idx = (app.tick as usize) % SPINNER.len();
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
            .map(|&idx| {
                let cmd = &COMMANDS[idx];
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {}", cmd.name),
                        Style::default()
                            .fg(MUTED_COLOR)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {}", cmd.description),
                        Style::default().fg(MUTED_COLOR),
                    ),
                ]))
            })
            .collect();

        frame.render_widget(List::new(items), layout[2]);
    }
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
// Helpers
// ---------------------------------------------------------------------------

/// Extracts the most useful display argument for a known tool from its raw
/// JSON arguments string. Returns `(tool_name, Option<primary_arg>)`.
///
/// - `Bash`  → `command` field
/// - `Read` / `Write` → `file_path` field
/// - unknown → no arg shown
fn parse_tool_call(name: &str, arguments: &str) -> (String, Option<String>) {
    let args: serde_json::Value =
        serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);
    let arg = match name {
        "Bash" => args.get("command").and_then(|v| v.as_str()).map(String::from),
        "Read" | "Write" => args
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
        Some(ServerEvent::ToolCallEnd(_)) => {
            // Tool results are surfaced implicitly through the agent's next ContentDelta.
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
        None => {}
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

    // Open the gRPC connection and establish a bidirectional streaming session.
    let channel = Channel::from_shared(server_addr)?.connect().await?;
    let mut grpc_client = AgentClient::new(channel);

    let (prompt_tx, prompt_rx) = mpsc::channel::<UserInput>(8);
    let prompt_stream = ReceiverStream::new(prompt_rx);

    let response = grpc_client
        .agent_session(tonic::Request::new(prompt_stream))
        .await?;
    let mut server_stream = response.into_inner();

    // Bridge the gRPC stream into a local mpsc channel so the main select!
    // loop can receive server events alongside terminal keyboard events.
    let (event_tx, mut event_rx) = mpsc::channel(64);
    tokio::spawn(async move {
        while let Ok(Some(event)) = server_stream.message().await {
            if event_tx.send(event).await.is_err() {
                break;
            }
        }
    });

    // Configure the terminal for raw / alternate-screen rendering.
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    let mut term_events = EventStream::new();
    // Ticker drives the thinking spinner; only app.tick is incremented when
    // the agent is busy, so the timer is cheap when idle.
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(80));

    loop {
        terminal.draw(|f| render(&app, f))?;

        tokio::select! {
            _ = ticker.tick() => {
                if app.agent_busy {
                    app.tick = app.tick.wrapping_add(1);
                }
            }

            Some(Ok(event)) = term_events.next() => {
                let Event::Key(key) = event else { continue };
                if key.kind != KeyEventKind::Press { continue; }

                match key.code {
                    KeyCode::Enter => {
                        if app.agent_busy || app.input.is_empty() {
                            continue;
                        }
                        let text = std::mem::take(&mut app.input);
                        app.cursor_pos = 0;
                        app.autocomplete_active = false;
                        app.autocomplete_matches.clear();

                        // Handle built-in slash commands before sending to server.
                        if text == "/exit" {
                            break;
                        }

                        app.messages.push(DisplayMessage::User(text.clone()));
                        app.auto_scroll = true;
                        app.agent_busy = true;
                        let _ = prompt_tx.send(UserInput { prompt: text }).await;
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

            Some(server_event) = event_rx.recv() => {
                handle_server_event(&mut app, server_event);
            }
        }
    }

    // Restore the terminal to its original state before exiting.
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
