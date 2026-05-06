# Getting Started

## First launch

Run `ein`. If `eind` isn't already running, it will be started automatically (release builds). On first launch, the terminal fills with a greeting and the **setup wizard**.

## Setup wizard

The wizard walks you through choosing a model provider and entering credentials. You can also reach it later with `/setup`.

**Step 1 — Choose a provider**

Use `↑`/`↓` to highlight a provider, then press `Enter`:

- **OpenRouter** — access hundreds of models through one API key
- **Anthropic** — direct Claude API access
- **OpenAI** — OpenAI or any OpenAI-compatible endpoint
- **Ollama** — local inference, no API key needed

**Step 2 — Enter credentials**

Each step is a text field. Use `Backspace` to edit, `Enter` or `Tab` to advance, `Esc` to go back.

- **OpenRouter / Anthropic / OpenAI**: enter your API key (input is masked)
- **OpenRouter / OpenAI**: optionally enter a custom base URL (press `Enter` to accept the default)
- **All providers**: optionally enter a model name (press `Enter` to use the provider's default)
- **Ollama**: enter the base URL (default: `http://localhost:11434`), no API key required

**Step 3 — Confirm**

Press `Enter` to save. The wizard writes your credentials to `~/.ein/config.json` and reconnects.

## Session picker

After setup (or on subsequent launches), the **session picker** appears:

```
┌─ Sessions ──────────────────────────────────────────┐
│                                                     │
│  ○  New Session                                     │
│  ○  2026-05-06  Debugging the plugin loader         │
│  ○  2026-05-05  Refactoring the render module       │
│                                                     │
│  ↑/↓ navigate  Enter select  Shift+D delete  S setup│
└─────────────────────────────────────────────────────┘
```

Select **New Session** to start fresh, or pick an existing session to resume its full conversation history.

## Directory access prompt

When you start a new session, Ein asks whether the agent should have read/write access to your current working directory:

```
Allow access to /Users/you/myproject? [Y/n]
```

Press `Y` to add it to the session's `allowed_paths`. Press `N`, `Enter`, or `Esc` to skip. This is session-scoped and not saved to your config file.

## Your first prompt

Type a message and press `Enter`. Watch the agent respond in real time — text streams in as it's generated. When the agent runs a tool you'll see it inline:

```
▸ Bash  ls -la
  total 48
  drwxr-xr-x  12 you  staff   384 May  6 11:30 .
  ...
```

The agent loops, calling tools as needed, until it has a complete answer. While it's working a spinner appears — input is blocked until it finishes.

## Next steps

- See [Configuration](configuration/README.md) to tweak your provider settings, allowed paths, and network access.
- See [Using Ein](using-ein/README.md) for all slash commands, session management, and keyboard shortcuts.
