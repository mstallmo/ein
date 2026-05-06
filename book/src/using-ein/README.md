# Using Ein

## Interface overview

The Ein TUI has four vertical sections:

```
┌──────────────────────────────────────────────────────────────┐
│                                                              │
│   Conversation pane                                          │
│                                                              │
│   Scrollable history of your messages and agent responses.   │
│   Tool calls appear inline with their output.                │
│   A braille spinner shows when the agent is thinking.        │
│                                                              │
├──────────────────────────────────────────────────────────────┤
│ > your message here                                          │
├──────────────────────────────────────────────────────────────┤
│   /co  /compact  Summarize and compact conversation history  │
│        /config   Edit ~/.ein/config.json                     │
│                                                              │
└──────────────────────────────────────────────────────────────┘
  model: claude-sonnet-4-5 | tokens: 12,847        session: …
```

**Conversation pane** — grows to fill available space. Scrollable with `↑`/`↓`. Auto-scroll re-engages when you reach the bottom.

**Input area** — bordered text field with a `> ` prefix. Expands vertically as your text wraps. Blocked while the agent is working.

**Autocomplete section** — always 3 lines tall. Shows matching slash commands as you type `/`. The top match is highlighted.

**Status bar** — bottom line. Shows the active model name (vendor prefix stripped) and cumulative token usage on the left; session ID on the right.

## Visual indicators

| Element | Meaning |
|---------|---------|
| `⠋ thinking` (blue spinner) | Agent is processing; input is blocked |
| `● connecting…` (red) | Not connected to `eind`; retrying every 3 seconds |
| `▸ ToolName  arg` (steel blue) | A tool call in progress or completed |
| Red error text | Agent error or disconnection error |

## Sections

- [Sessions](sessions.md) — creating, resuming, switching, and managing sessions
- [Slash Commands](slash-commands.md) — full reference for all `/` commands
- [Built-in Tools](tools.md) — what Bash, Read, Write, and Edit do and how they appear
- [Keyboard Shortcuts](keyboard-shortcuts.md) — complete key binding reference
