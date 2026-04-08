// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Mason Stallmo

//! SQLite-backed session persistence.
//!
//! [`SessionStore`] stores session configs and message histories so they
//! survive server restarts. Sessions are identified by UUID v7, which is
//! unique across independent databases and sortable by creation time.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
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

/// Serialisable snapshot of a `PluginConfig` proto message.
#[derive(Debug, Serialize, Deserialize)]
pub struct PluginConfigRecord {
    pub allowed_paths: Vec<String>,
    pub allowed_hosts: Vec<String>,
    pub params_json: String,
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
                .map(|(k, v)| {
                    (
                        k.clone(),
                        PluginConfigRecord {
                            allowed_paths: v.allowed_paths.clone(),
                            allowed_hosts: v.allowed_hosts.clone(),
                            params_json: v.params_json.clone(),
                        },
                    )
                })
                .collect(),
        }
    }
}

/// Async SQLite session store backed by a connection pool.
pub struct SessionStore {
    pool: SqlitePool,
}

impl SessionStore {
    /// Open (or create) the database at `path` and run pending migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        let pool = SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true),
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
    pub async fn open_in_memory() -> Result<Self> {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .context("opening in-memory database")?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("running database migrations")?;
        Ok(Self { pool })
    }

    /// Insert a new session record. Returns an error if `id` already exists.
    pub async fn create_session(&self, id: &str, config_json: &str) -> Result<()> {
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

    /// Return `true` if `id` names an existing session.
    pub async fn session_exists(&self, id: &str) -> Result<bool> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .context("checking session existence")?;
        Ok(row.0 > 0)
    }

    /// Load the message history for `id`. Returns `None` if the session does
    /// not exist.
    pub async fn load_messages(&self, id: &str) -> Result<Option<Vec<Message>>> {
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

    /// Overwrite the stored message history for an existing session.
    pub async fn save_messages(&self, id: &str, messages: &[Message]) -> Result<()> {
        let json = serde_json::to_string(messages).context("serialising messages")?;
        sqlx::query("UPDATE sessions SET messages_json = ? WHERE id = ?")
            .bind(&json)
            .bind(id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("saving messages for session {id}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ein_plugin::model_client::Role;

    async fn make_store() -> SessionStore {
        SessionStore::open_in_memory().await.expect("in-memory store")
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
}
