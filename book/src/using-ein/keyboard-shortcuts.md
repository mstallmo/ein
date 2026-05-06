# Keyboard Shortcuts

## Global

| Key | Action |
|-----|--------|
| `Ctrl+C` | Force quit Ein immediately (works in any state, even while the agent is busy) |

## Normal mode (no modal open)

### Text input

| Key | Action |
|-----|--------|
| Any character | Insert at cursor position |
| `Backspace` | Delete character before cursor |
| `←` | Move cursor left |
| `→` | Move cursor right |
| `Enter` | Submit message or execute slash command |

> Input is blocked while the agent is working (`⠋ thinking` spinner is visible).

### Scrolling

| Key | Action |
|-----|--------|
| `↑` | Scroll conversation up one line; disables auto-scroll |
| `↓` | Scroll conversation down one line; re-enables auto-scroll at the bottom |

Auto-scroll re-engages automatically when you scroll back to the bottom of the conversation.

## Session picker

| Key | Action |
|-----|--------|
| `↑` / `↓` | Navigate sessions |
| `Enter` | Select highlighted session (or "New Session") |
| `Shift+D` | Delete highlighted session (not available on "New Session") |
| `S` | Open setup wizard to configure a provider |

## Setup wizard

| Key | Action |
|-----|--------|
| `↑` / `↓` | Navigate provider list (step 1 only) |
| `Enter` or `Tab` | Advance to next step |
| `Esc` | Go back to previous step (or close wizard from step 1) |
| Any character | Edit text field (steps 2–4) |
| `Backspace` | Delete character in text field |
| `←` / `→` | Move cursor within text field |

## Plugin manager

| Key | Action |
|-----|--------|
| `↑` / `↓` | Navigate plugin sources |
| `Enter` | Install or update selected plugin |
| `Esc` | Close modal (also works while loading) |

## Directory access prompt (CWD modal)

| Key | Action |
|-----|--------|
| `Y` | Allow access — add current directory to `allowed_paths` |
| `N` | Deny access — skip |
| `Enter` | Deny access — skip |
| `Esc` | Deny access — skip |

## Uninstall confirmation

| Key | Action |
|-----|--------|
| `Y` | Confirm uninstall and begin removal |
| `N` or `Esc` | Cancel (confirm phase only) |
| Any key | Dismiss (done phase only) |
