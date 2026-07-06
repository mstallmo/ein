// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

//! Session persistence.
//!
//! [`SessionStore`] is the storage abstraction the server uses to persist
//! session configs and message histories so they survive restarts. The server
//! only ever holds an `Arc<dyn SessionStore>`, so downstream embedders (e.g. a
//! larger system built on top of `eind`) can supply their own database-backed
//! implementation instead of the bundled SQLite one.
//!
//! [`SqliteSessionStore`] is the default implementation. Sessions are
//! identified by UUID v7, which is unique across independent databases and
//! sortable by creation time.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use async_trait::async_trait;
use ein_plugin::model_client::Message;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

/// Serialisable snapshot of a `SessionConfig` proto message.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionConfigRecord {
    pub allowed_paths: Vec<String>,
    pub allowed_hosts: Vec<String>,
    pub model_client_name: String,
    pub plugin_configs: HashMap<String, PluginConfigRecord>,
}

impl From<&ein_proto::ein::SessionConfig> for SessionConfigRecord {
    fn from(cfg: &ein_proto::ein::SessionConfig) -> Self {
        Self {
            allowed_paths: cfg.allowed_paths.clone(),
            allowed_hosts: cfg.allowed_hosts.clone(),
            model_client_name: cfg.model_client_name.clone(),
            plugin_configs: cfg
                .plugin_configs
                .iter()
                .map(|(k, v)| (k.clone(), v.into()))
                .collect(),
        }
    }
}

/// Serialisable snapshot of a `PluginConfig` proto message.
#[derive(Debug, Serialize, Deserialize)]
pub struct PluginConfigRecord {
    pub allowed_paths: Vec<String>,
    pub allowed_hosts: Vec<String>,
    pub params_json: String,
}

impl From<&ein_proto::ein::PluginConfig> for PluginConfigRecord {
    fn from(cfg: &ein_proto::ein::PluginConfig) -> Self {
        Self {
            allowed_paths: cfg.allowed_paths.clone(),
            allowed_hosts: cfg.allowed_hosts.clone(),
            params_json: cfg.params_json.clone(),
        }
    }
}

/// A lightweight summary of a persisted session, returned by [`SessionStore::list_sessions`].
pub struct SessionSummaryData {
    pub id: String,
    pub created_at: i64,
    /// First user message in the session, truncated to 80 chars (empty if none yet).
    pub preview: String,
    /// The raw JSON stored at session creation; used by the TUI to reconstruct
    /// the original `SessionConfig` when resuming.
    pub session_config_json: String,
}

/// Deserialises the messages JSON and returns the content of the first user
/// message, truncated to 80 chars. Returns an empty string when there are no
/// user messages yet.
fn extract_preview(messages_json: &str) -> String {
    #[derive(serde::Deserialize)]
    struct Partial {
        role: String,
        content: Option<String>,
    }

    let messages: Vec<Partial> = serde_json::from_str(messages_json).unwrap_or_default();
    messages
        .into_iter()
        .find(|m| m.role == "user")
        .and_then(|m| m.content)
        .map(|c| {
            let mut s: String = c.chars().take(80).collect();
            if c.chars().count() > 80 {
                s.push('…');
            }
            s
        })
        .unwrap_or_default()
}

/// Storage backend for session configs and message histories.
///
/// The server interacts with persistence solely through this trait, holding an
/// `Arc<dyn SessionStore>`. [`SqliteSessionStore`] is the bundled default;
/// embedders can implement this trait against their own database and inject it
/// via [`AgentServer::with_session_store`](crate::AgentServer::with_session_store)
/// or [`run_with`](crate::run_with).
///
/// Sessions are keyed by a string `id` — a UUID v7 assigned by the server.
/// Implementations must be safe to share across concurrent session tasks
/// (`Send + Sync`).
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Insert a new session record. Returns an error if `id` already exists.
    ///
    /// `config_json` is stored for auditing and session-list UX (e.g. showing
    /// which model or paths were active). It is not read back during resume —
    /// the client always re-sends a fresh `SessionConfig` on reconnect.
    async fn create_session(&self, id: &str, config_json: &str) -> Result<()>;

    /// Return `true` if `id` names an existing session.
    async fn session_exists(&self, id: &str) -> Result<bool>;

    /// Load the message history for `id`. Returns `None` if the session does
    /// not exist, or `Some(vec![])` if it exists but has no messages yet.
    async fn load_messages(&self, id: &str) -> Result<Option<Vec<Message>>>;

    /// Return a summary of all sessions, ordered newest-first.
    async fn list_sessions(&self) -> Result<Vec<SessionSummaryData>>;

    /// Permanently delete a session and its message history.
    ///
    /// Succeeds whether or not the session existed.
    async fn delete_session(&self, id: &str) -> Result<()>;

    /// Overwrite the stored message history for an existing session.
    async fn save_messages(&self, id: &str, messages: &[Message]) -> Result<()>;
}

/// Default [`SessionStore`] backed by an async SQLite connection pool.
pub struct SqliteSessionStore {
    pool: SqlitePool,
}

impl SqliteSessionStore {
    /// Open (or create) the database at `path` and run pending migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        let pool = SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal),
        )
        .await
        .with_context(|| format!("opening session database at {}", path.display()))?;

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("running database migrations")?;

        Ok(Self { pool })
    }

    /// Open an in-memory database and run migrations — intended for unit tests only.
    #[cfg(test)]
    pub(crate) async fn open_in_memory() -> Result<Self> {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .context("opening in-memory database")?;

        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("running database migrations")?;

        Ok(Self { pool })
    }
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn create_session(&self, id: &str, config_json: &str) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        sqlx::query(
            "INSERT INTO sessions (id, created_at, session_config_json, messages_json)
             VALUES (?, ?, ?, ?)",
        )
        .bind(id)
        .bind(now)
        .bind(config_json)
        .bind("[]")
        .execute(&self.pool)
        .await
        .with_context(|| format!("creating session {id}"))?;

        Ok(())
    }

    async fn session_exists(&self, id: &str) -> Result<bool> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .context("checking session existence")?;

        Ok(row.0 > 0)
    }

    async fn load_messages(&self, id: &str) -> Result<Option<Vec<Message>>> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT messages_json FROM sessions WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .context("loading messages")?;

        match row {
            None => Ok(None),
            Some((json,)) => {
                let messages: Vec<Message> =
                    serde_json::from_str(&json).context("deserialising messages")?;

                Ok(Some(messages))
            }
        }
    }

    async fn list_sessions(&self) -> Result<Vec<SessionSummaryData>> {
        let rows: Vec<(String, i64, String, String)> = sqlx::query_as(
            "SELECT id, created_at, messages_json, session_config_json
             FROM sessions ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .context("listing sessions")?;

        Ok(rows
            .into_iter()
            .map(
                |(id, created_at, messages_json, session_config_json)| SessionSummaryData {
                    id,
                    created_at,
                    preview: extract_preview(&messages_json),
                    session_config_json,
                },
            )
            .collect())
    }

    async fn delete_session(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("deleting session {id}"))?;

        Ok(())
    }

    async fn save_messages(&self, id: &str, messages: &[Message]) -> Result<()> {
        let json = serde_json::to_string(messages).context("serialising messages")?;
        let result = sqlx::query("UPDATE sessions SET messages_json = ? WHERE id = ?")
            .bind(&json)
            .bind(id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("saving messages for session {id}"))?;

        anyhow::ensure!(result.rows_affected() == 1, "session {id} not found");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ein_plugin::model_client::Role;

    async fn make_store() -> SqliteSessionStore {
        SqliteSessionStore::open_in_memory()
            .await
            .expect("in-memory store")
    }

    fn simple_message(role: Role, text: &str) -> Message {
        Message {
            role,
            content: Some(text.to_string()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[tokio::test]
    async fn new_session_does_not_exist() {
        let store = make_store().await;
        assert!(!store.session_exists("nonexistent-id").await.unwrap());
    }

    #[tokio::test]
    async fn create_and_exists() {
        let store = make_store().await;
        store.create_session("abc-123", "{}").await.unwrap();
        assert!(store.session_exists("abc-123").await.unwrap());
    }

    #[tokio::test]
    async fn load_messages_returns_none_for_missing_session() {
        let store = make_store().await;
        assert!(store.load_messages("ghost").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn load_messages_returns_empty_vec_after_create() {
        let store = make_store().await;
        store.create_session("s1", "{}").await.unwrap();
        let msgs = store.load_messages("s1").await.unwrap().unwrap();
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn save_and_reload_messages() {
        let store = make_store().await;
        store.create_session("s2", "{}").await.unwrap();

        let messages = vec![
            simple_message(Role::System, "You are helpful."),
            simple_message(Role::User, "Hello"),
            simple_message(Role::Assistant, "Hi there!"),
        ];
        store.save_messages("s2", &messages).await.unwrap();

        let loaded = store.load_messages("s2").await.unwrap().unwrap();
        assert_eq!(loaded.len(), 3);
        assert!(matches!(loaded[0].role, Role::System));
        assert_eq!(loaded[1].content.as_deref(), Some("Hello"));
        assert_eq!(loaded[2].content.as_deref(), Some("Hi there!"));
    }

    #[tokio::test]
    async fn save_overwrites_previous_messages() {
        let store = make_store().await;
        store.create_session("s3", "{}").await.unwrap();

        store
            .save_messages("s3", &[simple_message(Role::User, "first")])
            .await
            .unwrap();
        store
            .save_messages("s3", &[simple_message(Role::User, "second")])
            .await
            .unwrap();

        let loaded = store.load_messages("s3").await.unwrap().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content.as_deref(), Some("second"));
    }

    #[tokio::test]
    async fn duplicate_session_id_returns_error() {
        let store = make_store().await;
        store.create_session("dup", "{}").await.unwrap();
        assert!(store.create_session("dup", "{}").await.is_err());
    }

    #[tokio::test]
    async fn uuid_v7_ids_are_time_sortable() {
        // UUID v7 encodes a millisecond timestamp in the high bits, so
        // lexicographic order matches creation order.
        let id1 = uuid::Uuid::now_v7().to_string();
        let id2 = uuid::Uuid::now_v7().to_string();
        assert!(id1 <= id2, "UUID v7 ids should be non-decreasing");
    }

    #[tokio::test]
    async fn save_messages_on_missing_session_returns_error() {
        let store = make_store().await;
        let result = store
            .save_messages("nonexistent", &[simple_message(Role::User, "hi")])
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delete_session_removes_it() {
        let store = make_store().await;
        store.create_session("del-1", "{}").await.unwrap();
        assert!(store.session_exists("del-1").await.unwrap());

        store.delete_session("del-1").await.unwrap();
        assert!(!store.session_exists("del-1").await.unwrap());
    }

    #[tokio::test]
    async fn delete_session_is_idempotent() {
        let store = make_store().await;
        // Deleting a non-existent session should not error.
        store.delete_session("ghost").await.unwrap();
    }

    #[tokio::test]
    async fn deleted_session_absent_from_list() {
        let store = make_store().await;
        store.create_session("keep", "{}").await.unwrap();
        store.create_session("remove", "{}").await.unwrap();

        store.delete_session("remove").await.unwrap();

        let sessions = store.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "keep");
    }
}
