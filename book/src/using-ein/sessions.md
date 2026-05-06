# Sessions

A session is a persistent conversation. It has a UUID identifier and its full message history is stored in SQLite at `~/.ein/sessions.db`. You can resume any session across restarts, and multiple clients can reconnect to the same `eind` server and pick up exactly where they left off.

## Creating a new session

Type `/new`. The current session is dropped (without deleting it), the session picker closes, and the **directory access prompt** appears for the new session. After that, you're in a fresh conversation.

The `/new` command also bypasses the 3-second reconnect delay — it triggers an immediate reconnect.

## The session picker

The picker appears automatically when Ein first connects. You can re-open it at any time with `/sessions`.

```
┌─ Sessions ──────────────────────────────────────────┐
│                                                     │
│  ○  New Session                                     │
│  ○  2026-05-06  Debugging the plugin loader         │
│  ○  2026-05-05  Refactoring the render module       │
│  ○  2026-05-04  Writing tests for grpc.rs           │
│                                                     │
│  ↑/↓ navigate  Enter select  Shift+D delete  S setup│
└─────────────────────────────────────────────────────┘
```

Row 0 is always **New Session**. Existing sessions are listed newest-first. Navigate with `↑`/`↓` and press `Enter` to load the selected session.

## Resuming a session

Select an existing session in the picker and press `Enter`. The server restores the full conversation history, which is replayed in the conversation pane. The next prompt you send continues from where you left off.

## Deleting a session

In the session picker, highlight an existing session (not "New Session") and press `Shift+D`. The session is deleted from the server immediately and removed from the picker list.

Deletion is permanent — the SQLite record and all its history are removed.

## Clearing context

`/clear` wipes the in-memory conversation history for the current session and clears the display. The SQLite record is **not** affected — you can still see the full history if you reconnect and resume the session later.

Use `/clear` when the context window is getting long but you want to keep the session alive for future reference.

## Compacting context

`/compact` asks the LLM to summarize the entire conversation, then replaces both the in-memory history and the SQLite record with the summary. Requires an active server connection.

The summary streams back as agent output, then the conversation pane clears to show only the summary going forward. Subsequent prompts continue with the compact history.

Use `/compact` when context is getting long and you don't need verbatim earlier history — it reduces token costs for future turns.

> **Note**: `/compact` is irreversible. The detailed history is replaced. If you want to preserve the original history, start a new session with `/new` instead.

## Session config

When you start a session, the configuration at that moment (allowed paths, allowed hosts, model client) is locked in for the life of the session. Mid-session config file changes update the model client but not the path/host allowlists. To pick up new allowlists, start a new session.
