// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

//! Integration test for the session-storage injection path.
//!
//! This file is compiled as an *external* crate against `eind`'s public API —
//! exactly the way a downstream embedder (e.g. Edward) sees it. If any type
//! needed to implement or inject a custom [`SessionStore`] were not public,
//! this test would fail to compile. It therefore locks in:
//!
//! - the public surface (`SessionStore`, `SessionSummaryData`, `Message`,
//!   `Role`, `AgentServer`, `EinConfig`) an embedder needs;
//! - that the trait is object-safe and usable as `Arc<dyn SessionStore>` — the
//!   exact type the server holds;
//! - that `AgentServer::with_session_store` accepts that trait object (a
//!   compile-time assertion, see `_assert_injection_wiring`);
//! - that a hand-rolled store honours the lifecycle contract the server relies
//!   on (create → save → load → list → delete), in the same call sequence
//!   `grpc.rs` performs during a session.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{Result, bail};
use async_trait::async_trait;
use eind::{Message, Role, SessionStore, SessionSummaryData};
use tokio::sync::Mutex;

/// A minimal in-memory [`SessionStore`], standing in for a real embedder's
/// database-backed implementation. Faithfully mirrors the observable
/// behaviour of the bundled SQLite store so the same lifecycle assertions hold.
#[derive(Default)]
struct MockSessionStore {
    sessions: Mutex<HashMap<String, SessionRow>>,
    /// Monotonic stand-in for a creation timestamp, so `list_sessions`
    /// ordering is deterministic regardless of wall-clock resolution.
    clock: AtomicI64,
}

struct SessionRow {
    created_at: i64,
    config_json: String,
    messages: Vec<Message>,
}

/// First user message content, truncated to 80 chars — matches the preview
/// semantics the TUI expects from any store.
fn preview_of(messages: &[Message]) -> String {
    messages
        .iter()
        .find(|m| matches!(m.role, Role::User))
        .and_then(|m| m.content.clone())
        .map(|c| {
            let mut s: String = c.chars().take(80).collect();
            if c.chars().count() > 80 {
                s.push('…');
            }
            s
        })
        .unwrap_or_default()
}

#[async_trait]
impl SessionStore for MockSessionStore {
    async fn create_session(&self, id: &str, config_json: &str) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        if sessions.contains_key(id) {
            bail!("session {id} already exists");
        }
        let created_at = self.clock.fetch_add(1, Ordering::Relaxed);
        sessions.insert(
            id.to_string(),
            SessionRow {
                created_at,
                config_json: config_json.to_string(),
                messages: Vec::new(),
            },
        );
        Ok(())
    }

    async fn session_exists(&self, id: &str) -> Result<bool> {
        Ok(self.sessions.lock().await.contains_key(id))
    }

    async fn load_messages(&self, id: &str) -> Result<Option<Vec<Message>>> {
        Ok(self
            .sessions
            .lock()
            .await
            .get(id)
            .map(|row| row.messages.clone()))
    }

    async fn list_sessions(&self) -> Result<Vec<SessionSummaryData>> {
        let sessions = self.sessions.lock().await;
        let mut rows: Vec<(&String, &SessionRow)> = sessions.iter().collect();
        // Newest-first, matching the SQLite store's `ORDER BY created_at DESC`.
        rows.sort_by_key(|(_, row)| std::cmp::Reverse(row.created_at));
        Ok(rows
            .into_iter()
            .map(|(id, row)| SessionSummaryData {
                id: id.clone(),
                created_at: row.created_at,
                preview: preview_of(&row.messages),
                session_config_json: row.config_json.clone(),
            })
            .collect())
    }

    async fn delete_session(&self, id: &str) -> Result<()> {
        self.sessions.lock().await.remove(id);
        Ok(())
    }

    async fn save_messages(&self, id: &str, messages: &[Message]) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        match sessions.get_mut(id) {
            Some(row) => {
                row.messages = messages.to_vec();
                Ok(())
            }
            None => bail!("session {id} not found"),
        }
    }
}

fn user_msg(text: &str) -> Message {
    Message {
        role: Role::User,
        content: Some(text.to_string()),
        tool_calls: None,
        tool_call_id: None,
    }
}

fn assistant_msg(text: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: Some(text.to_string()),
        tool_calls: None,
        tool_call_id: None,
    }
}

/// Drives a custom store through the exact sequence `grpc.rs` performs during a
/// session — via `Arc<dyn SessionStore>`, the type the server actually holds.
#[tokio::test]
async fn injected_store_honours_session_lifecycle() {
    let store: Arc<dyn SessionStore> = Arc::new(MockSessionStore::default());
    let id = "0192f000-0000-7000-8000-000000000001";

    // A brand-new session id does not exist yet.
    assert!(!store.session_exists(id).await.unwrap());
    assert!(store.load_messages(id).await.unwrap().is_none());

    // --- new session: server calls create_session, then starts empty ---
    store.create_session(id, "{}").await.unwrap();
    assert!(store.session_exists(id).await.unwrap());
    assert_eq!(
        store.load_messages(id).await.unwrap().unwrap().len(),
        0,
        "a freshly created session resumes with an empty history"
    );

    // --- after an agent turn: server persists the full history ---
    let turn_one = vec![user_msg("Hello"), assistant_msg("Hi there!")];
    store.save_messages(id, &turn_one).await.unwrap();

    // --- resume: server reloads the persisted history ---
    let resumed = store.load_messages(id).await.unwrap().unwrap();
    assert_eq!(resumed.len(), 2);
    assert!(matches!(resumed[0].role, Role::User));
    assert_eq!(resumed[1].content.as_deref(), Some("Hi there!"));

    // A later turn overwrites, it does not append.
    store
        .save_messages(id, &[user_msg("only message")])
        .await
        .unwrap();
    assert_eq!(store.load_messages(id).await.unwrap().unwrap().len(), 1);
}

/// The session-list surface (`ListSessions` RPC) sees injected sessions
/// newest-first with a preview drawn from the first user message.
#[tokio::test]
async fn injected_store_lists_and_deletes_sessions() {
    let store: Arc<dyn SessionStore> = Arc::new(MockSessionStore::default());
    let older = "0192f000-0000-7000-8000-00000000000a";
    let newer = "0192f000-0000-7000-8000-00000000000b";

    store.create_session(older, "{}").await.unwrap();
    store.create_session(newer, "{}").await.unwrap();
    store
        .save_messages(older, &[user_msg("first session question")])
        .await
        .unwrap();

    let summaries = store.list_sessions().await.unwrap();
    assert_eq!(summaries.len(), 2);
    // Newest-first ordering.
    assert_eq!(summaries[0].id, newer);
    assert_eq!(summaries[1].id, older);
    assert_eq!(summaries[1].preview, "first session question");

    // Delete removes it from the listing and is idempotent.
    store.delete_session(newer).await.unwrap();
    store.delete_session(newer).await.unwrap();
    let after = store.list_sessions().await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, older);
}

/// Contract edge cases the server depends on: duplicate creation and saving to
/// a missing session both fail.
#[tokio::test]
async fn injected_store_rejects_invalid_operations() {
    let store: Arc<dyn SessionStore> = Arc::new(MockSessionStore::default());
    let id = "0192f000-0000-7000-8000-0000000000ff";

    store.create_session(id, "{}").await.unwrap();
    assert!(
        store.create_session(id, "{}").await.is_err(),
        "creating a duplicate session id must fail"
    );
    assert!(
        store
            .save_messages("nonexistent", &[user_msg("hi")])
            .await
            .is_err(),
        "saving to a session that was never created must fail"
    );
}

/// Compile-time assertion (never executed): the server's store-injection
/// constructor must keep accepting the public `Arc<dyn SessionStore>` trait
/// object. If the wiring Edward relies on ever changes shape, this stops
/// compiling.
#[allow(dead_code)]
async fn _assert_injection_wiring(store: Arc<dyn SessionStore>) {
    let _ = eind::AgentServer::with_session_store(eind::EinConfig::default(), store).await;
    let _ = eind::run_with_store(50051, Arc::new(MockSessionStore::default())).await;
}
