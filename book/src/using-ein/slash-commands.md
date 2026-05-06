# Slash Commands

Type `/` in the input area to see all available commands with autocomplete. Commands work whether or not you're connected to the server (unless noted).

## Quick reference

| Command | Description |
|---------|-------------|
| `/exit` | Exit Ein |
| `/config` | Edit `~/.ein/config.json` |
| `/clear` | Clear conversation history (keeps SQLite) |
| `/new` | Start a new session |
| `/sessions` | Open the session picker |
| `/compact` | Summarize and compact conversation history |
| `/plugins` | Manage installed plugins |
| `/setup` | Run the first-time setup wizard |
| `/uninstall` | Stop and remove the `eind` service and binary |

---

## `/exit`

Exits Ein cleanly. Equivalent to `Ctrl+C`.

Works in any state, including while a modal is open or the agent is busy.

---

## `/config`

Opens `~/.ein/config.json` in `$EDITOR` (falls back to `nano` if `$EDITOR` is unset).

Ein suspends the terminal, opens the editor, then resumes when you close it. If you save changes, Ein picks them up via the config file watcher and sends the updated config to the server automatically.

---

## `/clear`

Clears the in-memory conversation history and wipes the display (keeping the header banner).

The SQLite record is preserved — you can still resume the full history later. This is useful for starting a fresh line of thought within the same session without losing the stored history.

Does not require a server connection.

---

## `/new`

Drops the current session and starts a fresh one. Shows the directory access prompt for the new session, then reconnects immediately (bypassing the 3-second retry delay).

The old session is preserved in SQLite and can be resumed from the session picker.

---

## `/sessions`

Opens the session picker so you can switch to a different session or start a new one. See [Sessions](sessions.md) for full details on the picker UI.

---

## `/compact`

Asks the LLM to summarize the current conversation, then replaces both the in-memory history and the SQLite record with the summary.

**Requires an active server connection.** If disconnected, an error is shown.

The summary streams back as agent output. Once complete, the conversation pane shows only the summary. This reduces context length and token costs for future turns but is irreversible — the detailed history is gone.

---

## `/plugins`

Opens the plugin manager modal. Lists all available plugin sources with their install status (installed ✓ or not installed ○). Navigate with `↑`/`↓` and press `Enter` to install or update the selected plugin. Press `Esc` to close.

---

## `/setup`

Opens the first-time setup wizard to configure or reconfigure a model provider. You can run this at any time to switch providers or update credentials.

Wizard steps:
1. Choose a provider
2. Enter API key (if applicable)
3. Enter base URL (if applicable)
4. Enter model name (optional)
5. Confirm — press `Enter` to save, `Esc` to go back

Saving triggers an immediate reconnect with the new config.

---

## `/uninstall`

Stops the `eind` server process and removes the `eind` binary from your system. Your data in `~/.ein/` (config, sessions database, plugins) is preserved.

**Phases:**
1. **Confirm** — press `Y` to proceed, `N` or `Esc` to cancel
2. **Running** — shows a progress spinner; input is blocked
3. **Done** — shows a step log (green on success, red on failure); press any key to dismiss
