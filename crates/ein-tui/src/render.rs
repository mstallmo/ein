// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use std::sync::OnceLock;

use ratatui::{
    Frame,
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
use tracing::debug;

use crate::app::{App, ConnectionStatus, DisplayMessage, SessionPickerState};
use crate::input::COMMANDS;

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

/// Application version, read from Cargo.toml at compile time.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Braille spinner frames for the thinking animation.
const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

// ---------------------------------------------------------------------------
// Corgi pixel art (startup header)
//
// 16×16 pixel grid rendered as 8 terminal rows using the half-block technique:
// each cell is a `▄` (lower-half block) where fg = lower pixel, bg = upper
// pixel. This doubles vertical resolution so pixels appear roughly square.
//
// Color index key:
//   0 = transparent   1 = tan/orange body    2 = dark-brown ears/outline
//   3 = cream muzzle  4 = near-black eyes/nose  5 = pink tongue
// ---------------------------------------------------------------------------

#[rustfmt::skip]
const CORGI_PIXELS: [[u8; 16]; 16] = [
    [0, 0, 0, 2, 2, 0, 0, 0, 0, 0, 0, 2, 2, 0, 0, 0], // row  0: ear tips
    [0, 0, 2, 1, 1, 2, 0, 0, 0, 0, 2, 1, 1, 2, 0, 0], // row  1: ears
    [0, 0, 2, 1, 1, 2, 0, 0, 0, 0, 2, 1, 1, 2, 0, 0], // row  2: ears
    [0, 2, 1, 1, 1, 1, 2, 2, 2, 2, 1, 1, 1, 1, 2, 0], // row  3: top of head
    [0, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 0], // row  4: forehead
    [0, 2, 1, 1, 4, 1, 1, 1, 1, 1, 1, 4, 1, 1, 2, 0], // row  5: eyes
    [0, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 0], // row  6: under eyes
    [0, 2, 1, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 1, 2, 0], // row  7: upper muzzle
    [0, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2, 0], // row  8: muzzle
    [0, 2, 3, 3, 3, 3, 4, 4, 4, 4, 3, 3, 3, 3, 2, 0], // row  9: nose
    [0, 2, 3, 3, 3, 3, 3, 5, 5, 3, 3, 3, 3, 3, 2, 0], // row 10: tongue
    [0, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 2, 0], // row 11: lower muzzle
    [0, 0, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 0, 0], // row 12: chin
    [0, 0, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 0, 0], // row 13: neck
    [0, 0, 0, 2, 1, 1, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0], // row 14: neck narrowing
    [0, 0, 0, 0, 2, 2, 1, 1, 1, 1, 2, 2, 0, 0, 0, 0], // row 15: neck base
];

/// Maps a pixel color index to its `Color`, or `None` for transparent (index 0).
fn pixel_color(idx: u8) -> Option<Color> {
    match idx {
        1 => Some(Color::Rgb(220, 160, 70)),  // tan/orange body
        2 => Some(Color::Rgb(120, 70, 20)),   // dark-brown ears/outline
        3 => Some(Color::Rgb(240, 225, 195)), // cream muzzle
        4 => Some(Color::Rgb(30, 20, 10)),    // near-black eyes/nose
        5 => Some(Color::Rgb(220, 140, 150)), // pink tongue
        _ => None,                            // transparent
    }
}

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

                    Some(Span::styled(
                        t,
                        Style::default().fg(Color::Rgb(c.r, c.g, c.b)),
                    ))
                }
            })
            .collect(),
        Err(_) => vec![Span::raw(line.to_string())],
    }
}

// ---------------------------------------------------------------------------
// Main render pass
// ---------------------------------------------------------------------------

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
pub(crate) fn render(app: &App, frame: &mut Frame) {
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

    // Ratatui's Paragraph::scroll((y, 0)) counts *rendered* rows (after
    // word-wrapping), not logical Line objects.  Use Paragraph::line_count so
    // that the row total matches exactly what ratatui will render — this
    // handles word-wrap, wide words, and unicode correctly without a fragile
    // manual approximation.
    let conv_width = layout[0].width;
    let viewport_height = layout[0].height;
    let conv = Paragraph::new(lines).wrap(Wrap { trim: false });
    let total_rows = conv.line_count(conv_width) as u16;

    // scroll_offset counts rows scrolled *up* from the bottom, so the
    // ratatui scroll value (rows from the top) is the inverse.
    let scroll = if app.auto_scroll {
        total_rows.saturating_sub(viewport_height)
    } else {
        total_rows
            .saturating_sub(viewport_height)
            .saturating_sub(app.scroll_offset)
    };

    debug!(
        total_rows,
        viewport_height,
        scroll,
        auto_scroll = app.auto_scroll,
        "scroll"
    );
    frame.render_widget(conv.scroll((scroll, 0)), layout[0]);

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

    // --- Session picker (overlays everything, shown before CWD modal) ---
    if let Some(picker) = &app.pending_session_picker {
        render_session_picker(picker, frame);
    }

    // --- CWD access modal (overlays everything when present, after session picker) ---
    if let Some(cwd_state) = &app.pending_cwd_prompt {
        render_cwd_modal(&cwd_state.cwd, frame);
    }

    // --- Status bar ---
    let status_text = match app.connection_status {
        ConnectionStatus::Connecting => format!(" model: {}", app.model_display),
        ConnectionStatus::Connected => format!(
            " model: {} | tokens: {}",
            app.model_display, app.cumulative_tokens
        ),
    };
    let session_text = app
        .session_id
        .as_deref()
        .map(|id| format!("session: {} ", id))
        .unwrap_or_default();
    let bar_width = layout[3].width as usize;
    let left_len = status_text.len();
    let right_len = session_text.len();
    let padding = bar_width.saturating_sub(left_len + right_len);
    let full_status = format!("{}{}{}", status_text, " ".repeat(padding), session_text);
    let status = Paragraph::new(full_status).style(Style::default().fg(MUTED_COLOR));
    frame.render_widget(status, layout[3]);
}

// ---------------------------------------------------------------------------
// Message line builder
// ---------------------------------------------------------------------------

/// Converts the message log into a flat list of styled ratatui `Line`s.
pub(crate) fn build_lines(messages: &[DisplayMessage]) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    for msg in messages {
        match msg {
            DisplayMessage::Header { cwd } => {
                lines.push(Line::raw(""));

                for (term_row, pair) in CORGI_PIXELS.chunks(2).enumerate() {
                    let upper = &pair[0];
                    let lower = &pair[1];

                    let mut spans: Vec<Span<'static>> = upper
                        .iter()
                        .zip(lower.iter())
                        .map(|(&u, &l)| {
                            if u == 0 && l == 0 {
                                Span::raw(" ")
                            } else {
                                let fg = pixel_color(l).unwrap_or(Color::Reset);
                                let bg = pixel_color(u).unwrap_or(Color::Reset);
                                Span::styled("▄", Style::default().fg(fg).bg(bg))
                            }
                        })
                        .collect();

                    match term_row {
                        1 => {
                            spans.push(Span::styled(
                                "  Ein",
                                Style::default()
                                    .fg(Color::Rgb(230, 200, 120))
                                    .add_modifier(Modifier::BOLD),
                            ));
                            spans.push(Span::styled(
                                format!("  v{VERSION}"),
                                Style::default().fg(MUTED_COLOR),
                            ));
                        }
                        3 => {
                            let display_cwd = dirs::home_dir()
                                .and_then(|h| {
                                    std::path::Path::new(cwd.as_str())
                                        .strip_prefix(&h)
                                        .ok()
                                        .map(|rel| format!("~/{}", rel.display()))
                                })
                                .unwrap_or_else(|| cwd.clone());
                            spans.push(Span::styled(
                                format!("  {display_cwd}"),
                                Style::default().fg(THINKING_COLOR),
                            ));
                        }
                        _ => {}
                    }

                    lines.push(Line::from(spans));
                }

                lines.push(Line::raw(""));
            }
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
            DisplayMessage::ToolCall {
                name,
                arg,
                output_lines,
            } => {
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

                // Show the last ≤8 lines of streamed output, indented.
                let skip = output_lines.len().saturating_sub(8);
                for output_line in output_lines.iter().skip(skip) {
                    lines.push(Line::from(Span::styled(
                        format!("    {output_line}"),
                        Style::default().fg(MUTED_COLOR),
                    )));
                }

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
                    Span::styled(format!("  {file_path}"), Style::default().fg(MUTED_COLOR)),
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
// Modal renderers
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

/// Renders the session picker modal, overlaying the entire terminal.
fn render_session_picker(picker: &SessionPickerState, frame: &mut Frame) {
    // Row 0 = "New Session" (always); subsequent rows = existing sessions (cap at 8).
    let visible_rows = (picker.sessions.len() + 1).min(9);
    let modal_height = (visible_rows as u16) + 5; // blank + rows + blank + hint + 2 borders
    let modal_width = (frame.area().width * 7 / 10)
        .max(60)
        .min(frame.area().width);
    let area = centered_rect(modal_width, modal_height, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Select Session ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(INPUT_BORDER_COLOR));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = vec![Line::raw("")];

    // Row 0: "New Session"
    let sel0 = picker.selected == 0;
    lines.push(Line::from(Span::styled(
        format!("{}New Session", if sel0 { "> " } else { "  " }),
        if sel0 {
            Style::default()
                .fg(AUTOCOMPLETE_TOP_COLOR)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(MUTED_COLOR)
        },
    )));

    // Rows 1..N: existing sessions
    for (i, session) in picker.sessions.iter().enumerate().take(8) {
        let row_idx = i + 1;
        let is_sel = picker.selected == row_idx;
        let cursor = if is_sel { "> " } else { "  " };
        let date = format_session_date(session.created_at);
        let style = if is_sel {
            Style::default().fg(AUTOCOMPLETE_TOP_COLOR)
        } else {
            Style::default().fg(MUTED_COLOR)
        };
        let preview = if session.preview.is_empty() {
            "(no messages yet)".to_string()
        } else {
            session.preview.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(cursor, style),
            Span::styled(date, Style::default().fg(THINKING_COLOR)),
            Span::styled("  ", style),
            Span::styled(preview, style),
        ]));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(
            "[↑↓]",
            Style::default()
                .fg(AUTOCOMPLETE_TOP_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" Navigate  ", Style::default().fg(MUTED_COLOR)),
        Span::styled(
            "[Enter]",
            Style::default()
                .fg(AUTOCOMPLETE_TOP_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" Select  ", Style::default().fg(MUTED_COLOR)),
        Span::styled(
            "[Shift+D]",
            Style::default()
                .fg(AUTOCOMPLETE_TOP_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" Delete", Style::default().fg(MUTED_COLOR)),
    ]));

    frame.render_widget(Paragraph::new(lines), inner);
}

fn format_session_date(unix_secs: i64) -> String {
    chrono::DateTime::from_timestamp(unix_secs, 0)
        .unwrap_or_default()
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod unit {
    use super::build_lines;
    use crate::app::DisplayMessage;

    #[test]
    fn build_lines_tool_call_caps_at_8_output_lines() {
        let output_lines: Vec<String> = (0..20).map(|i| format!("line{i}")).collect();
        let msgs = vec![DisplayMessage::ToolCall {
            name: "Bash".to_string(),
            arg: Some("echo hi".to_string()),
            output_lines,
        }];
        let lines = build_lines(&msgs);
        // 1 header + 8 output rows + 1 trailing blank = 10
        assert_eq!(lines.len(), 10);
    }

    #[test]
    fn build_lines_tool_call_shows_last_lines() {
        let output_lines: Vec<String> = (0..20).map(|i| format!("sentinel_{i}")).collect();
        let msgs = vec![DisplayMessage::ToolCall {
            name: "Bash".to_string(),
            arg: None,
            output_lines,
        }];
        let lines = build_lines(&msgs);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(
            text.contains("sentinel_19"),
            "last output line should be rendered"
        );
        assert!(
            !text.contains("sentinel_0"),
            "first output line should be scrolled off"
        );
    }

    #[test]
    fn build_lines_empty_output_lines() {
        let msgs = vec![DisplayMessage::ToolCall {
            name: "Read".to_string(),
            arg: Some("/etc/hosts".to_string()),
            output_lines: vec![],
        }];
        let lines = build_lines(&msgs);
        // 1 header + 0 output rows + 1 trailing blank = 2
        assert_eq!(lines.len(), 2);
    }
}

#[cfg(test)]
mod render_tests {
    use crate::app::test_helpers::make_app;
    use crate::app::{App, ConnectionStatus, DisplayMessage};
    use crate::render::render;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn draw(app: &App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(app, f)).unwrap();
        terminal
            .backend_mut()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    /// Build an app with a completed Bash tool call (100 lines of output).
    fn app_with_long_tool_call() -> App {
        let mut app = make_app("test-model");
        app.messages.push(DisplayMessage::ToolCall {
            name: "Bash".to_string(),
            arg: Some("some-command".to_string()),
            output_lines: (0..100).map(|i| format!("output line {i}")).collect(),
        });
        app
    }

    #[test]
    fn thinking_spinner_visible_during_tool_output() {
        // Regression: spinner was scrolled off-screen when bash produced many lines.
        let mut app = app_with_long_tool_call();
        app.agent_busy = true;
        app.auto_scroll = true;

        let text = draw(&app, 100, 30);
        assert!(
            text.contains("thinking"),
            "thinking spinner should be visible in the viewport"
        );
    }

    #[test]
    fn agent_response_visible_after_long_tool_call() {
        // Regression: agent response after a long bash output was cut off.
        let mut app = app_with_long_tool_call();
        app.messages.push(DisplayMessage::AgentText(
            "SENTINEL_RESPONSE_TEXT".to_string(),
        ));
        app.agent_busy = false;
        app.auto_scroll = true;

        let text = draw(&app, 100, 30);
        assert!(
            text.contains("SENTINEL_RESPONSE_TEXT"),
            "agent response should be visible after tool call"
        );
    }

    #[test]
    fn status_bar_shows_model_name_and_tokens() {
        let mut app = make_app("my-test-model");
        app.connection_status = ConnectionStatus::Connected;
        app.cumulative_tokens = 99;

        let text = draw(&app, 80, 10);
        assert!(
            text.contains("my-test-model"),
            "status bar should show model name"
        );
        assert!(text.contains("99"), "status bar should show token count");
    }

    #[test]
    fn connecting_animation_shown_when_disconnected() {
        let mut app = make_app("m");
        app.connection_status = ConnectionStatus::Connecting;
        app.agent_busy = false;

        let text = draw(&app, 80, 10);
        assert!(
            text.contains("connecting to server"),
            "connecting animation should be shown"
        );
    }
}
