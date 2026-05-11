// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

use std::sync::OnceLock;

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use syntect::{
    easy::HighlightLines,
    highlighting::{Style as SyntectStyle, ThemeSet},
    parsing::SyntaxSet,
};
use tracing::debug;

use crate::app::{
    App, ConnectionStatus, DisplayMessage, Modal, PROVIDERS, PluginModalState, SessionPickerState,
    SetupWizardState, UninstallModalState, UninstallPhase, WizardStep,
};
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
// Markdown rendering
// ---------------------------------------------------------------------------

/// Convert a markdown string to a list of styled ratatui `Line`s.
///
/// Handles headings, bold, italic, inline code, fenced code blocks (syntax-
/// highlighted via syntect), ordered and unordered lists, blockquotes, and
/// horizontal rules. Plain text passes through unchanged.
fn markdown_to_lines(text: &str) -> Vec<Line<'static>> {
    use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();

    let mut bold = false;
    let mut italic = false;
    let mut strikethrough = false;
    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut code_buf = String::new();
    let mut blockquote_depth: u32 = 0;
    // Stack: None = unordered, Some(n) = ordered with next item number n
    let mut list_stack: Vec<Option<u64>> = Vec::new();
    let mut item_depth: u32 = 0;
    let mut item_needs_prefix = false;
    let mut task_list_item = false;
    let mut heading: Option<u8> = None;

    macro_rules! flush_line {
        () => {
            if !spans.is_empty() {
                lines.push(Line::from(std::mem::take(&mut spans)));
            }
        };
    }

    macro_rules! span_style {
        () => {{
            let mut s = Style::default();
            if bold {
                s = s.add_modifier(Modifier::BOLD);
            }
            if italic {
                s = s.add_modifier(Modifier::ITALIC);
            }
            if strikethrough {
                s = s.add_modifier(Modifier::CROSSED_OUT);
            }
            s
        }};
    }

    for event in Parser::new_ext(text, Options::all()) {
        match event {
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                flush_line!();
                // Only add spacing after paragraphs outside list items and blockquotes
                if item_depth == 0 && blockquote_depth == 0 {
                    lines.push(Line::raw(""));
                }
            }

            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some(match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                });
            }
            Event::End(TagEnd::Heading(_)) => {
                let lvl = heading.unwrap_or(1);
                let styled: Vec<Span<'static>> = std::mem::take(&mut spans)
                    .into_iter()
                    .map(|s| {
                        let mut style = s.style.add_modifier(Modifier::BOLD);

                        if lvl <= 2 {
                            style = style.fg(AUTOCOMPLETE_TOP_COLOR);
                        }

                        Span::styled(s.content, style)
                    })
                    .collect();

                lines.push(Line::from(styled));
                lines.push(Line::raw(""));
                heading = None;
            }
            Event::Start(Tag::Strong) => bold = true,
            Event::End(TagEnd::Strong) => bold = false,
            Event::Start(Tag::Emphasis) => italic = true,
            Event::End(TagEnd::Emphasis) => italic = false,
            Event::Start(Tag::Strikethrough) => strikethrough = true,
            Event::End(TagEnd::Strikethrough) => strikethrough = false,
            Event::Start(Tag::BlockQuote(_)) => {
                blockquote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                flush_line!();

                blockquote_depth = blockquote_depth.saturating_sub(1);
                if blockquote_depth == 0 {
                    lines.push(Line::raw(""));
                }
            }
            Event::Start(Tag::List(start)) => {
                flush_line!(); // flush enclosing item text before starting sub-list

                list_stack.push(start.map(|n| n as u64));
            }
            Event::End(TagEnd::List(_)) => {
                list_stack.pop();

                if list_stack.is_empty() {
                    lines.push(Line::raw(""));
                }
            }
            Event::Start(Tag::Item) => {
                item_depth += 1;
                item_needs_prefix = true;
            }
            Event::End(TagEnd::Item) => {
                flush_line!();

                item_depth = item_depth.saturating_sub(1);
                item_needs_prefix = false;
                task_list_item = false;
                // Advance the ordered list counter
                if let Some(Some(n)) = list_stack.last_mut() {
                    *n += 1;
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                in_code_block = true;
                code_lang = match &kind {
                    CodeBlockKind::Fenced(lang) => lang.trim().to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                code_buf.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code_block = false;
                let ps = syntax_set();
                let ts = theme_set();
                let syntax = if !code_lang.is_empty() {
                    ps.find_syntax_by_token(&code_lang)
                        .unwrap_or_else(|| ps.find_syntax_plain_text())
                } else {
                    ps.find_syntax_plain_text()
                };
                let mut h = HighlightLines::new(syntax, &ts.themes["base16-ocean.dark"]);
                for code_line in code_buf.trim_end_matches('\n').split('\n') {
                    let hspans = highlight_line_spans(&mut h, ps, code_line);
                    lines.push(Line::from(hspans));
                }
                lines.push(Line::raw(""));
                code_lang.clear();
                code_buf.clear();
            }
            Event::Text(t) => {
                if in_code_block {
                    code_buf.push_str(&t);
                } else {
                    // Inject bullet/number prefix at the start of a list item
                    if item_needs_prefix {
                        item_needs_prefix = false;
                        let depth = list_stack.len().saturating_sub(1);
                        let indent = "  ".repeat(depth);
                        let prefix_span = match list_stack.last() {
                            Some(Some(n)) => Span::styled(
                                format!("{indent}{n}. "),
                                Style::default().fg(MUTED_COLOR),
                            ),
                            _ => {
                                if task_list_item {
                                    Span::styled(format!("{indent} "), Style::default())
                                } else {
                                    Span::styled(
                                        format!("{indent}• "),
                                        Style::default().fg(MUTED_COLOR),
                                    )
                                }
                            }
                        };

                        spans.push(prefix_span);
                    } else if blockquote_depth > 0 && spans.is_empty() {
                        spans.push(Span::styled(
                            "│ ".repeat(blockquote_depth as usize),
                            Style::default().fg(MUTED_COLOR),
                        ));
                    }

                    // Text events may contain embedded newlines
                    let t_str = t.as_ref();
                    let mut parts = t_str.split('\n');
                    if let Some(first) = parts.next() {
                        if !first.is_empty() {
                            spans.push(Span::styled(first.to_string(), span_style!()));
                        }
                    }
                    for rest in parts {
                        flush_line!();
                        if !rest.is_empty() {
                            if blockquote_depth > 0 {
                                spans.push(Span::styled(
                                    "│ ".repeat(blockquote_depth as usize),
                                    Style::default().fg(MUTED_COLOR),
                                ));
                            }
                            spans.push(Span::styled(rest.to_string(), span_style!()));
                        }
                    }
                }
            }
            Event::Code(t) => {
                // Inject list item prefix if needed
                if item_needs_prefix {
                    item_needs_prefix = false;
                    let depth = list_stack.len().saturating_sub(1);
                    let indent = "  ".repeat(depth);
                    let prefix = match list_stack.last() {
                        Some(Some(n)) => format!("{indent}{n}. "),
                        _ => format!("{indent}• "),
                    };
                    spans.push(Span::styled(prefix, Style::default().fg(MUTED_COLOR)));
                }

                spans.push(Span::styled(
                    format!("{}", t.as_ref()),
                    Style::default().fg(THINKING_COLOR),
                ));
            }
            Event::TaskListMarker(checked) => {
                task_list_item = true;
                let span = if checked {
                    Span::default().content("\u{2714}").fg(Color::Green)
                } else {
                    Span::default().content("[ ]")
                };

                spans.push(span);
            }
            // Treat soft breaks as line breaks so single-newline content stays on separate lines
            Event::SoftBreak | Event::HardBreak => {
                flush_line!();
            }
            Event::Rule => {
                lines.push(Line::from(Span::styled(
                    "─".repeat(40),
                    Style::default().fg(MUTED_COLOR),
                )));
                lines.push(Line::raw(""));
            }
            _ => {}
        }
    }

    // Flush any remaining spans
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }

    // Ensure exactly one trailing blank line for spacing consistency with other message variants
    let last_is_blank = lines
        .last()
        .map(|l| l.spans.iter().all(|s| s.content.is_empty()))
        .unwrap_or(false);
    if !last_is_blank {
        lines.push(Line::raw(""));
    }

    lines
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

    // --- Plugin modal ---

    if let Some(modal) = &app.modal {
        match modal {
            Modal::SetupWizard(setup_wizard_state) => {
                render_setup_wizard(setup_wizard_state, frame);
            }
            Modal::PluginManager(plugin_manager_state) => {
                render_plugin_modal(plugin_manager_state, app.tick, frame);
            }
            Modal::SessionPicker(session_picker_state) => {
                render_session_picker(session_picker_state, frame);
            }
            Modal::CwdPrompt(cwd_state) => {
                render_cwd_modal(&cwd_state.cwd, frame);
            }
            Modal::UninstallConfirm(state) => {
                render_uninstall_modal(state, app.tick, frame);
            }
        }
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
            DisplayMessage::SetupPrompt => {
                lines.push(Line::from(Span::styled(
                    " No provider configured.",
                    Style::default()
                        .fg(DISCONNECTED_COLOR)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(Span::styled(
                    " Run /setup to get started, or /config to edit ~/.ein/config.json directly.",
                    Style::default().fg(MUTED_COLOR),
                )));
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
                lines.extend(markdown_to_lines(text));
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

/// Renders the plugin manager modal over the entire terminal.
fn render_plugin_modal(modal: &PluginModalState, tick: u64, frame: &mut Frame) {
    let frame_idx = (tick as usize) % SPINNER.len();

    // Calculate height: borders(2) + top blank(1) + content rows + bottom blank(1) + hints(1)
    // plus an optional status row and its trailing blank (2 more).
    let content_rows: u16 = if modal.loading || modal.installing {
        1
    } else {
        modal.sources.len().max(1) as u16
    };
    let status_rows: u16 = if modal.status_message.is_some() { 2 } else { 0 };
    let modal_height = 2 + 1 + content_rows + 1 + 1 + status_rows;
    let modal_width = (frame.area().width * 7 / 10)
        .max(50)
        .min(frame.area().width);
    let area = centered_rect(modal_width, modal_height, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Plugins ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(INPUT_BORDER_COLOR));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = vec![Line::raw("")];

    if modal.loading {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {} ", SPINNER[frame_idx]),
                Style::default().fg(THINKING_COLOR),
            ),
            Span::styled(
                "Loading...",
                Style::default()
                    .fg(MUTED_COLOR)
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
    } else if modal.installing {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {} ", SPINNER[frame_idx]),
                Style::default().fg(THINKING_COLOR),
            ),
            Span::styled(
                "Installing...",
                Style::default()
                    .fg(THINKING_COLOR)
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
    } else if modal.sources.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No plugin sources available",
            Style::default().fg(MUTED_COLOR),
        )));
    } else {
        for (i, source) in modal.sources.iter().enumerate() {
            let is_sel = modal.selected == i;
            let cursor = if is_sel { "> " } else { "  " };
            let (checkmark, check_color) = if source.installed {
                ("✓", Color::Green)
            } else {
                ("○", MUTED_COLOR)
            };
            let name_style = if is_sel {
                Style::default().fg(AUTOCOMPLETE_TOP_COLOR)
            } else {
                Style::default().fg(MUTED_COLOR)
            };
            lines.push(Line::from(vec![
                Span::styled(cursor, name_style),
                Span::styled(checkmark, Style::default().fg(check_color)),
                Span::styled(format!("  {}", source.display_name), name_style),
            ]));
        }
    }

    lines.push(Line::raw(""));

    if let Some(msg) = &modal.status_message {
        let msg_color =
            if msg.to_lowercase().contains("fail") || msg.to_lowercase().contains("error") {
                DISCONNECTED_COLOR
            } else {
                Color::Green
            };
        lines.push(Line::from(Span::styled(
            format!("  {msg}"),
            Style::default().fg(msg_color),
        )));
        lines.push(Line::raw(""));
    }

    if !modal.loading {
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
            Span::styled(" Install/Update  ", Style::default().fg(MUTED_COLOR)),
            Span::styled(
                "[Esc]",
                Style::default()
                    .fg(AUTOCOMPLETE_TOP_COLOR)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Close", Style::default().fg(MUTED_COLOR)),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), inner);
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

fn render_uninstall_modal(state: &UninstallModalState, tick: u64, frame: &mut Frame) {
    // Height: 2 borders + blank line + content lines + blank + hint
    let content_lines = match &state.phase {
        UninstallPhase::Confirm => 4u16,
        UninstallPhase::Running => 1u16,
        UninstallPhase::Done { .. } => state.log.len().max(1) as u16 + 2,
    };
    let modal_height = 2 + 1 + content_lines;
    let modal_width = (frame.area().width * 7 / 10)
        .max(60)
        .min(frame.area().width);
    let area = centered_rect(modal_width, modal_height, frame.area());

    frame.render_widget(Clear, area);

    let (title, border_color) = match &state.phase {
        UninstallPhase::Confirm => (" Uninstall eind? ", DISCONNECTED_COLOR),
        UninstallPhase::Running => (" Uninstalling… ", MUTED_COLOR),
        UninstallPhase::Done { success: true } => (" Uninstalled ", Color::Green),
        UninstallPhase::Done { success: false } => (" Uninstall failed ", DISCONNECTED_COLOR),
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

    let mut lines = vec![Line::raw("")];

    match &state.phase {
        UninstallPhase::Confirm => {
            lines.push(Line::from(Span::styled(
                " Stop the service and remove the server binary.",
                Style::default().fg(AUTOCOMPLETE_TOP_COLOR),
            )));
            lines.push(Line::from(Span::styled(
                " Config and sessions in ~/.ein/ will be preserved.",
                Style::default().fg(MUTED_COLOR),
            )));
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled(
                    " [Y]",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" Confirm   ", Style::default().fg(MUTED_COLOR)),
                Span::styled(
                    "[N]",
                    Style::default()
                        .fg(DISCONNECTED_COLOR)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" Cancel", Style::default().fg(MUTED_COLOR)),
            ]));
        }
        UninstallPhase::Running => {
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", SPINNER[tick as usize % SPINNER.len()]),
                    Style::default().fg(THINKING_COLOR),
                ),
                Span::styled(
                    "Uninstalling…",
                    Style::default()
                        .fg(MUTED_COLOR)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
        UninstallPhase::Done { success } => {
            for step in &state.log {
                lines.push(Line::from(Span::styled(
                    format!(" {step}"),
                    Style::default().fg(if *success {
                        Color::Green
                    } else {
                        DISCONNECTED_COLOR
                    }),
                )));
            }
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                " Press any key to dismiss",
                Style::default()
                    .fg(MUTED_COLOR)
                    .add_modifier(Modifier::ITALIC),
            )));
        }
    }

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
        Span::styled(" Delete  ", Style::default().fg(MUTED_COLOR)),
        Span::styled(
            "[S]",
            Style::default()
                .fg(AUTOCOMPLETE_TOP_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" Setup", Style::default().fg(MUTED_COLOR)),
    ]));

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Renders an input field with a cursor indicator within a wizard page.
///
/// `masked` replaces visible characters with `*` (used for API key fields).
fn render_wizard_field(label: &str, buf: &str, cursor: usize, masked: bool) -> Line<'static> {
    let displayed: String = if masked {
        "*".repeat(buf.chars().count())
    } else {
        buf.to_string()
    };

    let before: String = displayed.chars().take(cursor).collect();
    let at: String = displayed
        .chars()
        .nth(cursor)
        .map(|c| c.to_string())
        .unwrap_or_else(|| " ".to_string());
    let after: String = displayed.chars().skip(cursor + 1).collect();

    Line::from(vec![
        Span::styled(format!("  {label}: "), Style::default().fg(MUTED_COLOR)),
        Span::raw(before),
        Span::styled(at, Style::default().add_modifier(Modifier::REVERSED)),
        Span::raw(after),
    ])
}

/// Key hint line shown at the bottom of each wizard page.
fn wizard_hint(
    primary: &str,
    primary_label: &str,
    secondary: &str,
    secondary_label: &str,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("  [{primary}]"),
            Style::default()
                .fg(AUTOCOMPLETE_TOP_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {primary_label}  "),
            Style::default().fg(MUTED_COLOR),
        ),
        Span::styled(
            format!("[{secondary}]"),
            Style::default()
                .fg(AUTOCOMPLETE_TOP_COLOR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {secondary_label}"),
            Style::default().fg(MUTED_COLOR),
        ),
    ])
}

/// Renders the first-time setup wizard modal over the entire terminal.
fn render_setup_wizard(wizard: &SetupWizardState, frame: &mut Frame) {
    let modal_width = (frame.area().width * 7 / 10)
        .max(60)
        .min(frame.area().width);

    // Build the page content first so we know the height.
    let mut content: Vec<Line> = vec![Line::raw("")];

    let title = match wizard.step {
        WizardStep::ChooseProvider => {
            for (i, (_, display_name)) in PROVIDERS.iter().enumerate() {
                let is_sel = wizard.provider_idx == i;
                let cursor = if is_sel { "> " } else { "  " };
                let style = if is_sel {
                    Style::default()
                        .fg(AUTOCOMPLETE_TOP_COLOR)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(MUTED_COLOR)
                };
                content.push(Line::from(Span::styled(
                    format!("{cursor}{display_name}"),
                    style,
                )));
            }
            content.push(Line::raw(""));
            content.push(wizard_hint("↑↓", "Navigate", "Enter", "Select"));
            " Setup: Choose Provider "
        }
        WizardStep::EnterApiKey => {
            content.push(render_wizard_field(
                "API Key",
                &wizard.api_key,
                wizard.api_key_cursor,
                true,
            ));
            content.push(Line::raw(""));
            content.push(wizard_hint("Enter", "Next", "Esc", "Back"));
            " Setup: API Key "
        }
        WizardStep::EnterBaseUrl => {
            let default_hint = match wizard.provider_key() {
                "ein_openrouter" => "  (default: https://openrouter.ai/api/v1)",
                "ein_ollama" => "  (default: http://localhost:11434)",
                _ => "  (leave blank for api.openai.com)",
            };
            content.push(render_wizard_field(
                "Base URL",
                &wizard.base_url,
                wizard.base_url_cursor,
                false,
            ));
            content.push(Line::from(Span::styled(
                default_hint.to_string(),
                Style::default().fg(MUTED_COLOR),
            )));
            content.push(Line::raw(""));
            content.push(wizard_hint("Enter", "Next", "Esc", "Back"));
            " Setup: Base URL "
        }
        WizardStep::EnterModel => {
            content.push(render_wizard_field(
                "Model",
                &wizard.model,
                wizard.model_cursor,
                false,
            ));
            content.push(Line::raw(""));
            content.push(wizard_hint("Enter", "Next", "Esc", "Back"));
            " Setup: Model "
        }
        WizardStep::Confirm => {
            let provider_name = PROVIDERS[wizard.provider_idx].1;
            let key_chars: Vec<char> = wizard.api_key.chars().collect();
            let masked_key = if key_chars.len() > 4 {
                format!(
                    "*****{}",
                    key_chars[key_chars.len() - 4..].iter().collect::<String>()
                )
            } else if key_chars.is_empty() {
                "(none)".to_string()
            } else {
                "*****".to_string()
            };

            content.push(Line::from(vec![
                Span::styled("  Provider : ", Style::default().fg(MUTED_COLOR)),
                Span::styled(
                    provider_name.to_string(),
                    Style::default().fg(AUTOCOMPLETE_TOP_COLOR),
                ),
            ]));

            if !wizard.api_key.is_empty() {
                content.push(Line::from(vec![
                    Span::styled("  API Key  : ", Style::default().fg(MUTED_COLOR)),
                    Span::styled(masked_key, Style::default().fg(AUTOCOMPLETE_TOP_COLOR)),
                ]));
            }

            let effective_url: &str = match wizard.provider_key() {
                "ein_openrouter" if wizard.base_url.is_empty() => "https://openrouter.ai/api/v1",
                "ein_ollama" if wizard.base_url.is_empty() => "http://localhost:11434",
                _ => &wizard.base_url,
            };
            if !effective_url.is_empty() {
                content.push(Line::from(vec![
                    Span::styled("  Base URL : ", Style::default().fg(MUTED_COLOR)),
                    Span::styled(
                        effective_url.to_string(),
                        Style::default().fg(AUTOCOMPLETE_TOP_COLOR),
                    ),
                ]));
            }

            if !wizard.model.is_empty() {
                content.push(Line::from(vec![
                    Span::styled("  Model    : ", Style::default().fg(MUTED_COLOR)),
                    Span::styled(
                        wizard.model.clone(),
                        Style::default().fg(AUTOCOMPLETE_TOP_COLOR),
                    ),
                ]));
            }

            if let Some(err) = &wizard.error {
                content.push(Line::raw(""));
                content.push(Line::from(Span::styled(
                    format!("  Error: {err}"),
                    Style::default().fg(DISCONNECTED_COLOR),
                )));
            }

            content.push(Line::raw(""));
            content.push(wizard_hint("Enter", "Save", "Esc", "Back"));
            " Setup: Confirm "
        }
    };

    let modal_height = (content.len() as u16) + 2; // +2 for borders
    let area = centered_rect(modal_width, modal_height, frame.area());

    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(INPUT_BORDER_COLOR));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    frame.render_widget(Paragraph::new(content), inner);
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
    use super::{build_lines, markdown_to_lines};
    use crate::{app::DisplayMessage, render::AUTOCOMPLETE_TOP_COLOR};
    use ratatui::style::{Color, Style, Stylize};

    fn read_fixture(filename: &str) -> Vec<u8> {
        use std::{env, fs};

        let fixture_path = format!(
            "{}/tests/fixtures/{filename}",
            env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set")
        );

        fs::read(&fixture_path).expect(&format!("FAILED TO READ fixture path {fixture_path}"))
    }

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

    #[test]
    fn markdown_heading_1() {
        let lines = markdown_to_lines("# Heading 1");
        assert_eq!(lines.len(), 2);

        let header = &lines[0];
        let spans = &header.spans;
        assert_eq!(spans.len(), 1);

        let span = &spans[0];
        assert_eq!(span.to_string(), "Heading 1");
        assert_eq!(
            span.style,
            Style::default().fg(AUTOCOMPLETE_TOP_COLOR).bold()
        );
    }

    #[test]
    fn markdown_heading_2() {
        let lines = markdown_to_lines("## Heading 2");
        assert_eq!(lines.len(), 2);

        let header = &lines[0];
        let spans = &header.spans;
        assert_eq!(spans.len(), 1);

        let span = &spans[0];
        assert_eq!(span.to_string(), "Heading 2");
        assert_eq!(
            span.style,
            Style::default().fg(AUTOCOMPLETE_TOP_COLOR).bold()
        );
    }

    #[test]
    fn markdown_heading_3() {
        let lines = markdown_to_lines("### Heading 3");
        assert_eq!(lines.len(), 2);

        let header = &lines[0];
        let spans = &header.spans;
        assert_eq!(spans.len(), 1);

        let span = &spans[0];
        assert_eq!(span.to_string(), "Heading 3");
        assert_eq!(span.style, Style::default().bold());
    }

    #[test]
    fn markdown_heading_4() {
        let lines = markdown_to_lines("#### Heading 4");
        assert_eq!(lines.len(), 2);

        let header = &lines[0];
        let spans = &header.spans;
        assert_eq!(spans.len(), 1);

        let span = &spans[0];
        assert_eq!(span.to_string(), "Heading 4");
        assert_eq!(span.style, Style::default().bold());
    }

    #[test]
    fn markdown_heading_5() {
        let lines = markdown_to_lines("##### Heading 5");
        assert_eq!(lines.len(), 2);

        let header = &lines[0];
        let spans = &header.spans;
        assert_eq!(spans.len(), 1);

        let span = &spans[0];
        assert_eq!(span.to_string(), "Heading 5");
        assert_eq!(span.style, Style::default().bold());
    }

    #[test]
    fn markdown_heading_6() {
        let lines = markdown_to_lines("###### Heading 6");
        assert_eq!(lines.len(), 2);

        let header = &lines[0];
        let spans = &header.spans;
        assert_eq!(spans.len(), 1);

        let span = &spans[0];
        assert_eq!(span.to_string(), "Heading 6");
        assert_eq!(span.style, Style::default().bold());
    }

    #[test]
    fn markdown_text() {
        let fixture_bytes = read_fixture("text.md");
        let markdown_text =
            String::from_utf8(fixture_bytes).expect(&format!("Failed to parse bytes"));

        let lines = markdown_to_lines(&markdown_text);
        assert_eq!(lines.len(), 3);

        let line = &lines[0];
        assert_eq!(line.spans.len(), 9);
        assert_eq!(line.spans[0].style, Style::default());
        assert_eq!(line.spans[1].style, Style::default().bold());
        assert_eq!(line.spans[2].style, Style::default());
        assert_eq!(line.spans[3].style, Style::default().italic());
        assert_eq!(line.spans[4].style, Style::default());
        assert_eq!(line.spans[5].style, Style::default().bold().italic());
        assert_eq!(line.spans[6].style, Style::default());
        assert_eq!(line.spans[7].style, Style::default().crossed_out());
        assert_eq!(line.spans[8].style, Style::default());
    }

    #[test]
    fn markdown_unordered_list() {
        let fixture_bytes = read_fixture("unordered_list.md");
        let markdown_lists =
            String::from_utf8(fixture_bytes).expect(&format!("Failed to parse bytes"));

        let lines = markdown_to_lines(&markdown_lists);
        assert_eq!(lines.len(), 6);

        // First entry
        assert_eq!(lines[0].spans.len(), 2);
        let marker_span = &lines[0].spans[0];
        let content_span = &lines[0].spans[1];
        assert_eq!(marker_span.style, Style::default().dark_gray());
        assert_eq!(marker_span.to_string(), "• ");
        assert_eq!(content_span.style, Style::default());
        assert_eq!(content_span.to_string(), "Item 1");

        // Second entry
        assert_eq!(lines[0].spans.len(), 2);
        let marker_span = &lines[1].spans[0];
        let content_span = &lines[1].spans[1];
        assert_eq!(marker_span.style, Style::default().dark_gray());
        assert_eq!(marker_span.to_string(), "• ");
        assert_eq!(content_span.style, Style::default());
        assert_eq!(content_span.to_string(), "Item 2");

        // First nested entry
        assert_eq!(lines[0].spans.len(), 2);
        let marker_span = &lines[2].spans[0];
        let content_span = &lines[2].spans[1];
        assert_eq!(marker_span.style, Style::default().dark_gray());
        assert_eq!(marker_span.to_string(), "  • ");
        assert_eq!(content_span.style, Style::default());
        assert_eq!(content_span.to_string(), "Nested Item 2.1");

        // Second nested entry
        assert_eq!(lines[0].spans.len(), 2);
        let marker_span = &lines[3].spans[0];
        let content_span = &lines[3].spans[1];
        assert_eq!(marker_span.style, Style::default().dark_gray());
        assert_eq!(marker_span.to_string(), "  • ");
        assert_eq!(content_span.style, Style::default());
        assert_eq!(content_span.to_string(), "Nested Item 2.2");

        // Third entry
        assert_eq!(lines[0].spans.len(), 2);
        let marker_span = &lines[4].spans[0];
        let content_span = &lines[4].spans[1];
        assert_eq!(marker_span.style, Style::default().dark_gray());
        assert_eq!(marker_span.to_string(), "• ");
        assert_eq!(content_span.style, Style::default());
        assert_eq!(content_span.to_string(), "Item 3");
    }

    #[test]
    fn markdown_ordered_list() {
        let fixture_bytes = read_fixture("ordered_list.md");
        let markdown_lists =
            String::from_utf8(fixture_bytes).expect(&format!("Failed to parse bytes"));

        let lines = markdown_to_lines(&markdown_lists);
        assert_eq!(lines.len(), 4);

        // First entry
        assert_eq!(lines[0].spans.len(), 2);
        let marker_span = &lines[0].spans[0];
        let content_span = &lines[0].spans[1];
        assert_eq!(marker_span.style, Style::default().dark_gray());
        assert_eq!(marker_span.to_string(), "1. ");
        assert_eq!(content_span.style, Style::default());
        assert_eq!(content_span.to_string(), "First step");

        // Second entry
        assert_eq!(lines[0].spans.len(), 2);
        let marker_span = &lines[1].spans[0];
        let content_span = &lines[1].spans[1];
        assert_eq!(marker_span.style, Style::default().dark_gray());
        assert_eq!(marker_span.to_string(), "2. ");
        assert_eq!(content_span.style, Style::default());
        assert_eq!(content_span.to_string(), "Second step");

        // Third entry
        assert_eq!(lines[0].spans.len(), 2);
        let marker_span = &lines[2].spans[0];
        let content_span = &lines[2].spans[1];
        assert_eq!(marker_span.style, Style::default().dark_gray());
        assert_eq!(marker_span.to_string(), "3. ");
        assert_eq!(content_span.style, Style::default());
        assert_eq!(content_span.to_string(), "Third step");
    }

    #[test]
    fn markdown_task_list() {
        let fixture_bytes = read_fixture("task_list.md");
        let markdown_lists =
            String::from_utf8(fixture_bytes).expect(&format!("Failed to parse bytes"));

        let lines = markdown_to_lines(&markdown_lists);
        assert_eq!(lines.len(), 5);

        // First entry
        assert_eq!(lines[0].spans.len(), 3);
        let marker_span = &lines[0].spans[0];
        let spacer_span = &lines[0].spans[1];
        let content_span = &lines[0].spans[2];
        assert_eq!(marker_span.style, Style::default().green());
        assert_eq!(marker_span.to_string(), "\u{2714}");
        assert_eq!(spacer_span.to_string(), " ");
        assert_eq!(content_span.style, Style::default());
        assert_eq!(content_span.to_string(), "Implement core parser");

        // Second entry
        assert_eq!(lines[1].spans.len(), 3);
        let marker_span = &lines[1].spans[0];
        let spacer_span = &lines[1].spans[1];
        let content_span = &lines[1].spans[2];
        assert_eq!(marker_span.style, Style::default());
        assert_eq!(marker_span.to_string(), "[ ]");
        assert_eq!(spacer_span.to_string(), " ");
        assert_eq!(content_span.style, Style::default());
        assert_eq!(content_span.to_string(), "Add theme support");

        // Third entry
        assert_eq!(lines[2].spans.len(), 3);
        let marker_span = &lines[2].spans[0];
        let spacer_span = &lines[2].spans[1];
        let content_span = &lines[2].spans[2];
        assert_eq!(marker_span.style, Style::default());
        assert_eq!(marker_span.to_string(), "[ ]");
        assert_eq!(spacer_span.to_string(), " ");
        assert_eq!(content_span.style, Style::default());
        assert_eq!(content_span.to_string(), "Fix edge cases with backticks");

        // Fourth entry
        assert_eq!(lines[3].spans.len(), 3);
        let marker_span = &lines[3].spans[0];
        let spacer_span = &lines[3].spans[1];
        let content_span = &lines[3].spans[2];
        assert_eq!(marker_span.style, Style::default());
        assert_eq!(marker_span.to_string(), "[ ]");
        assert_eq!(spacer_span.to_string(), " ");
        assert_eq!(content_span.style, Style::default());
        assert_eq!(content_span.to_string(), "Release v1.0");
    }

    #[test]
    fn markdown_code() {
        let fixture_bytes = read_fixture("code.md");
        let markdown_code =
            String::from_utf8(fixture_bytes).expect(&format!("Failed to parse bytes"));

        let lines = markdown_to_lines(&markdown_code);
        assert_eq!(lines.len(), 4);

        let line = &lines[0];
        assert_eq!(line.spans.len(), 2);
        assert_eq!(
            line.spans[0].style,
            Style::default().fg(Color::Rgb(143, 161, 179))
        );
        assert_eq!(line.spans[0].to_string(), "$");
        assert_eq!(
            line.spans[1].style,
            Style::default().fg(Color::Rgb(192, 197, 206))
        );
        assert_eq!(line.spans[1].to_string(), " git init");
    }

    #[test]
    fn markdown_blockquotes() {
        let fixture_bytes = read_fixture("blockquotes.md");
        let markdown_blockquotes =
            String::from_utf8(fixture_bytes).expect(&format!("Failed to parse bytes"));

        let lines = markdown_to_lines(&markdown_blockquotes);
        assert_eq!(lines.len(), 2);

        let line = &lines[0];
        assert_eq!(line.spans.len(), 1);
        assert_eq!(
            line.spans[0].to_string(),
            "│ “The best way to predict the future is to invent it.” — Alan Kay"
        );
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
