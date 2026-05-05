use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use std::str::FromStr;

use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use rusqlite::{Connection, params};
use serde_json;

use devo_protocol::{
    PendingInputItem, PendingInputKind, SessionId, SessionMetadata, SessionRuntimeStatus,
    SessionTitleState,
};

/// Queue type for pending messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueType {
    /// Pending turn inputs (from turn/start while a turn is active).
    Turn,
    /// /btw steer inputs (from turn/steer, scoped to current turn only).
    Btw,
}

impl QueueType {
    fn as_str(&self) -> &'static str {
        match self {
            QueueType::Turn => "turn",
            QueueType::Btw => "btw",
        }
    }
}

/// Session-level token statistics.
#[derive(Debug, Clone)]
pub struct SessionStats {
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
    pub total_cache_creation_tokens: usize,
    pub total_cache_read_tokens: usize,
    pub last_input_tokens: usize,
    pub turn_count: usize,
    pub prompt_token_estimate: usize,
}

/// SQLite database for session metadata, token stats, and pending queues.
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Opens or creates the SQLite database at the given path.
    pub fn open(db_path: PathBuf) -> Result<Self> {
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open database at {}", db_path.display()))?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    /// Runs schema migrations.
    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT,
                title_state TEXT NOT NULL DEFAULT 'unset',
                model TEXT,
                thinking TEXT,
                cwd TEXT NOT NULL,
                ephemeral INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                schema_version INTEGER NOT NULL DEFAULT 2
            );

            CREATE TABLE IF NOT EXISTS session_stats (
                session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                total_input_tokens INTEGER NOT NULL DEFAULT 0,
                total_output_tokens INTEGER NOT NULL DEFAULT 0,
                total_cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
                total_cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                last_input_tokens INTEGER NOT NULL DEFAULT 0,
                turn_count INTEGER NOT NULL DEFAULT 0,
                prompt_token_estimate INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS pending_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                queue_type TEXT NOT NULL CHECK(queue_type IN ('turn', 'btw')),
                kind TEXT NOT NULL,
                content TEXT NOT NULL,
                metadata TEXT,
                created_at INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_pending_session
                ON pending_messages(session_id, queue_type);
            ",
        )
        .context("failed to run database migrations")?;
        Ok(())
    }

    // === Session CRUD ===

    /// Inserts or updates a session's metadata.
    pub fn upsert_session(&self, meta: &SessionMetadata) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let title_state_str = match &meta.title_state {
            SessionTitleState::Unset => "unset",
            SessionTitleState::Provisional => "provisional",
            SessionTitleState::Final(_) => "final",
        };
        conn.execute(
            "INSERT INTO sessions (id, title, title_state, model, thinking, cwd, ephemeral, created_at, updated_at, schema_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 2)
             ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                title_state = excluded.title_state,
                model = excluded.model,
                thinking = excluded.thinking,
                cwd = excluded.cwd,
                updated_at = excluded.updated_at",
            params![
                meta.session_id.to_string(),
                meta.title,
                title_state_str,
                meta.model,
                meta.thinking,
                meta.cwd.to_string_lossy().to_string(),
                meta.ephemeral as i32,
                meta.created_at.timestamp(),
                meta.updated_at.timestamp(),
            ],
        )
        .context("failed to upsert session")?;
        Ok(())
    }

    /// Retrieves a session's metadata by ID.
    pub fn get_session(&self, id: &SessionId) -> Result<Option<SessionMetadata>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, title, title_state, model, thinking, cwd, ephemeral, created_at, updated_at
                 FROM sessions WHERE id = ?1",
            )
            .context("failed to prepare get_session statement")?;
        let result = stmt.query_row(params![id.to_string()], |row| {
            let id_str: String = row.get(0)?;
            let title: Option<String> = row.get(1)?;
            let title_state_str: String = row.get(2)?;
            let model: Option<String> = row.get(3)?;
            let thinking: Option<String> = row.get(4)?;
            let cwd_str: String = row.get(5)?;
            let ephemeral: i32 = row.get(6)?;
            let created_at: i64 = row.get(7)?;
            let updated_at: i64 = row.get(8)?;

            let title_state = match title_state_str.as_str() {
                "provisional" => SessionTitleState::Provisional,
                "final" => {
                    SessionTitleState::Final(devo_protocol::SessionTitleFinalSource::ModelGenerated)
                }
                _ => SessionTitleState::Unset,
            };

            Ok(SessionMetadata {
                session_id: SessionId::from_str(&id_str).unwrap_or_default(),
                cwd: PathBuf::from(&cwd_str),
                created_at: Utc
                    .timestamp_opt(created_at, 0)
                    .single()
                    .unwrap_or_else(Utc::now),
                updated_at: Utc
                    .timestamp_opt(updated_at, 0)
                    .single()
                    .unwrap_or_else(Utc::now),
                title,
                title_state,
                ephemeral: ephemeral != 0,
                model,
                thinking,
                reasoning_effort: None,
                total_input_tokens: 0,
                total_output_tokens: 0,
                prompt_token_estimate: 0,
                status: SessionRuntimeStatus::Idle,
            })
        });

        match result {
            Ok(meta) => Ok(Some(meta)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Lists all sessions ordered by most recently updated.
    pub fn list_sessions(&self) -> Result<Vec<SessionMetadata>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT id, title, title_state, model, thinking, cwd, ephemeral, created_at, updated_at
                 FROM sessions ORDER BY updated_at DESC",
            )
            .context("failed to prepare list_sessions statement")?;
        let rows = stmt
            .query_map([], |row| {
                let id_str: String = row.get(0)?;
                let title: Option<String> = row.get(1)?;
                let title_state_str: String = row.get(2)?;
                let model: Option<String> = row.get(3)?;
                let thinking: Option<String> = row.get(4)?;
                let cwd_str: String = row.get(5)?;
                let ephemeral: i32 = row.get(6)?;
                let created_at: i64 = row.get(7)?;
                let updated_at: i64 = row.get(8)?;

                let title_state = match title_state_str.as_str() {
                    "provisional" => SessionTitleState::Provisional,
                    "final" => SessionTitleState::Final(
                        devo_protocol::SessionTitleFinalSource::ModelGenerated,
                    ),
                    _ => SessionTitleState::Unset,
                };

                Ok(SessionMetadata {
                    session_id: SessionId::from_str(&id_str).unwrap_or_default(),
                    cwd: PathBuf::from(&cwd_str),
                    created_at: Utc
                        .timestamp_opt(created_at, 0)
                        .single()
                        .unwrap_or_else(Utc::now),
                    updated_at: Utc
                        .timestamp_opt(updated_at, 0)
                        .single()
                        .unwrap_or_else(Utc::now),
                    title,
                    title_state,
                    ephemeral: ephemeral != 0,
                    model,
                    thinking,
                    reasoning_effort: None,
                    total_input_tokens: 0,
                    total_output_tokens: 0,
                    prompt_token_estimate: 0,
                    status: SessionRuntimeStatus::Idle,
                })
            })
            .context("failed to query sessions")?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    /// Deletes a session and its related data.
    pub fn delete_session(&self, id: &SessionId) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "DELETE FROM sessions WHERE id = ?1",
            params![id.to_string()],
        )
        .context("failed to delete session")?;
        Ok(())
    }

    // === Session Stats ===

    /// Inserts or updates session token statistics.
    pub fn update_stats(&self, id: &SessionId, stats: &SessionStats) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "INSERT INTO session_stats (session_id, total_input_tokens, total_output_tokens,
                total_cache_creation_tokens, total_cache_read_tokens, last_input_tokens,
                turn_count, prompt_token_estimate)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(session_id) DO UPDATE SET
                total_input_tokens = excluded.total_input_tokens,
                total_output_tokens = excluded.total_output_tokens,
                total_cache_creation_tokens = excluded.total_cache_creation_tokens,
                total_cache_read_tokens = excluded.total_cache_read_tokens,
                last_input_tokens = excluded.last_input_tokens,
                turn_count = excluded.turn_count,
                prompt_token_estimate = excluded.prompt_token_estimate",
            params![
                id.to_string(),
                stats.total_input_tokens as i64,
                stats.total_output_tokens as i64,
                stats.total_cache_creation_tokens as i64,
                stats.total_cache_read_tokens as i64,
                stats.last_input_tokens as i64,
                stats.turn_count as i64,
                stats.prompt_token_estimate as i64,
            ],
        )
        .context("failed to update session stats")?;
        Ok(())
    }

    /// Retrieves session token statistics.
    pub fn get_stats(&self, id: &SessionId) -> Result<Option<SessionStats>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let result = conn.query_row(
            "SELECT total_input_tokens, total_output_tokens, total_cache_creation_tokens,
                    total_cache_read_tokens, last_input_tokens, turn_count, prompt_token_estimate
             FROM session_stats WHERE session_id = ?1",
            params![id.to_string()],
            |row| {
                Ok(SessionStats {
                    total_input_tokens: row.get::<_, i64>(0)? as usize,
                    total_output_tokens: row.get::<_, i64>(1)? as usize,
                    total_cache_creation_tokens: row.get::<_, i64>(2)? as usize,
                    total_cache_read_tokens: row.get::<_, i64>(3)? as usize,
                    last_input_tokens: row.get::<_, i64>(4)? as usize,
                    turn_count: row.get::<_, i64>(5)? as usize,
                    prompt_token_estimate: row.get::<_, i64>(6)? as usize,
                })
            },
        );

        match result {
            Ok(stats) => Ok(Some(stats)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // === Pending Messages ===

    /// Pushes a pending message to the specified queue.
    pub fn push_pending(
        &self,
        session_id: &SessionId,
        queue: QueueType,
        item: &PendingInputItem,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let (kind_str, content) = match &item.kind {
            PendingInputKind::UserText { text } => ("user_text", text.clone()),
            PendingInputKind::ToolCallBlockedByHook {
                tool_use_id,
                reason,
            } => {
                let content = serde_json::json!({
                    "tool_use_id": tool_use_id,
                    "reason": reason,
                });
                ("tool_call_blocked", content.to_string())
            }
            PendingInputKind::BudgetLimitSteering => ("budget_limit", String::new()),
        };
        let metadata_str = item.metadata.as_ref().map(|v| v.to_string());
        conn.execute(
            "INSERT INTO pending_messages (session_id, queue_type, kind, content, metadata, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                session_id.to_string(),
                queue.as_str(),
                kind_str,
                content,
                metadata_str,
                item.created_at.timestamp(),
            ],
        )
        .context("failed to push pending message")?;
        Ok(())
    }

    /// Drains all pending messages from the specified queue, deleting them in the process.
    pub fn drain_pending(
        &self,
        session_id: &SessionId,
        queue: QueueType,
    ) -> Result<Vec<PendingInputItem>> {
        let mut conn = self.conn.lock().expect("database mutex poisoned");

        let tx = conn
            .transaction()
            .context("failed to begin drain transaction")?;

        let items = {
            let mut stmt = tx
                .prepare(
                    "SELECT kind, content, metadata, created_at
                     FROM pending_messages
                     WHERE session_id = ?1 AND queue_type = ?2
                     ORDER BY id ASC",
                )
                .context("failed to prepare drain_pending statement")?;
            let rows = stmt
                .query_map(params![session_id.to_string(), queue.as_str()], |row| {
                    let kind_str: String = row.get(0)?;
                    let content: String = row.get(1)?;
                    let metadata_str: Option<String> = row.get(2)?;
                    let created_at: i64 = row.get(3)?;

                    let kind = match kind_str.as_str() {
                        "user_text" => PendingInputKind::UserText { text: content },
                        "tool_call_blocked" => {
                            let parsed: serde_json::Value =
                                serde_json::from_str(&content).unwrap_or_default();
                            PendingInputKind::ToolCallBlockedByHook {
                                tool_use_id: parsed["tool_use_id"]
                                    .as_str()
                                    .unwrap_or_default()
                                    .to_string(),
                                reason: parsed["reason"].as_str().unwrap_or_default().to_string(),
                            }
                        }
                        "budget_limit" => PendingInputKind::BudgetLimitSteering,
                        _ => PendingInputKind::UserText { text: content },
                    };

                    let metadata = metadata_str.and_then(|s| serde_json::from_str(&s).ok());

                    Ok(PendingInputItem {
                        kind,
                        metadata,
                        created_at: Utc
                            .timestamp_opt(created_at, 0)
                            .single()
                            .unwrap_or_else(Utc::now),
                    })
                })
                .context("failed to query pending messages")?;

            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            items
        };

        tx.execute(
            "DELETE FROM pending_messages WHERE session_id = ?1 AND queue_type = ?2",
            params![session_id.to_string(), queue.as_str()],
        )
        .context("failed to delete drained messages")?;

        tx.commit().context("failed to commit drain transaction")?;

        Ok(items)
    }

    /// Clears all pending messages from the specified queue.
    pub fn clear_pending(&self, session_id: &SessionId, queue: QueueType) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "DELETE FROM pending_messages WHERE session_id = ?1 AND queue_type = ?2",
            params![session_id.to_string(), queue.as_str()],
        )
        .context("failed to clear pending messages")?;
        Ok(())
    }

    /// Counts pending messages in the specified queue.
    #[allow(dead_code)]
    pub fn count_pending(&self, session_id: &SessionId, queue: QueueType) -> Result<usize> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pending_messages WHERE session_id = ?1 AND queue_type = ?2",
            params![session_id.to_string(), queue.as_str()],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_db() -> (Database, TempDir) {
        let dir = TempDir::new().expect("create temp dir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(db_path).expect("open database");
        (db, dir)
    }

    fn sample_session(id: &str) -> SessionMetadata {
        SessionMetadata {
            session_id: SessionId::from_str(id).unwrap_or_default(),
            cwd: PathBuf::from("/tmp"),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            title: Some("Test Session".into()),
            title_state: SessionTitleState::Provisional,
            ephemeral: false,
            model: Some("claude-sonnet-4-20250514".into()),
            thinking: None,
            reasoning_effort: None,
            total_input_tokens: 0,
            total_output_tokens: 0,
            prompt_token_estimate: 0,
            status: SessionRuntimeStatus::Idle,
        }
    }

    #[test]
    fn upsert_and_get_session() {
        let (db, _dir) = test_db();
        let meta = sample_session("session-1");
        db.upsert_session(&meta).expect("upsert");

        let retrieved = db.get_session(&meta.session_id).expect("get");
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.session_id, meta.session_id);
        assert_eq!(retrieved.title, Some("Test Session".into()));
    }

    #[test]
    fn list_sessions_ordered() {
        let (db, _dir) = test_db();
        let meta1 = sample_session("session-1");
        let mut meta2 = sample_session("session-2");
        meta2.updated_at = Utc::now() + chrono::Duration::seconds(10);
        db.upsert_session(&meta1).expect("upsert");
        db.upsert_session(&meta2).expect("upsert");

        let sessions = db.list_sessions().expect("list");
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id, meta2.session_id);
    }

    #[test]
    fn delete_session_cascades() {
        let (db, _dir) = test_db();
        let meta = sample_session("session-1");
        db.upsert_session(&meta).expect("upsert");

        db.delete_session(&meta.session_id).expect("delete");
        let retrieved = db.get_session(&meta.session_id).expect("get");
        assert!(retrieved.is_none());
    }

    #[test]
    fn update_and_get_stats() {
        let (db, _dir) = test_db();
        let meta = sample_session("session-1");
        db.upsert_session(&meta).expect("upsert");

        let stats = SessionStats {
            total_input_tokens: 1000,
            total_output_tokens: 500,
            total_cache_creation_tokens: 100,
            total_cache_read_tokens: 50,
            last_input_tokens: 200,
            turn_count: 5,
            prompt_token_estimate: 800,
        };
        db.update_stats(&meta.session_id, &stats).expect("update");

        let retrieved = db.get_stats(&meta.session_id).expect("get");
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.total_input_tokens, 1000);
        assert_eq!(retrieved.total_output_tokens, 500);
        assert_eq!(retrieved.turn_count, 5);
    }

    #[test]
    fn push_and_drain_pending() {
        let (db, _dir) = test_db();
        let meta = sample_session("session-1");
        db.upsert_session(&meta).expect("upsert");

        let item1 = PendingInputItem {
            kind: PendingInputKind::UserText {
                text: "hello".into(),
            },
            metadata: None,
            created_at: Utc::now(),
        };
        let item2 = PendingInputItem {
            kind: PendingInputKind::UserText {
                text: "world".into(),
            },
            metadata: None,
            created_at: Utc::now(),
        };

        db.push_pending(&meta.session_id, QueueType::Turn, &item1)
            .expect("push");
        db.push_pending(&meta.session_id, QueueType::Turn, &item2)
            .expect("push");

        let count = db
            .count_pending(&meta.session_id, QueueType::Turn)
            .expect("count");
        assert_eq!(count, 2);

        let drained = db
            .drain_pending(&meta.session_id, QueueType::Turn)
            .expect("drain");
        assert_eq!(drained.len(), 2);
        assert!(matches!(&drained[0].kind, PendingInputKind::UserText { text } if text == "hello"));
        assert!(matches!(&drained[1].kind, PendingInputKind::UserText { text } if text == "world"));

        let count = db
            .count_pending(&meta.session_id, QueueType::Turn)
            .expect("count");
        assert_eq!(count, 0);
    }

    #[test]
    fn queue_types_are_isolated() {
        let (db, _dir) = test_db();
        let meta = sample_session("session-1");
        db.upsert_session(&meta).expect("upsert");

        let turn_item = PendingInputItem {
            kind: PendingInputKind::UserText {
                text: "turn msg".into(),
            },
            metadata: None,
            created_at: Utc::now(),
        };
        let btw_item = PendingInputItem {
            kind: PendingInputKind::UserText {
                text: "btw msg".into(),
            },
            metadata: None,
            created_at: Utc::now(),
        };

        db.push_pending(&meta.session_id, QueueType::Turn, &turn_item)
            .expect("push");
        db.push_pending(&meta.session_id, QueueType::Btw, &btw_item)
            .expect("push");

        let turn_count = db
            .count_pending(&meta.session_id, QueueType::Turn)
            .expect("count");
        let btw_count = db
            .count_pending(&meta.session_id, QueueType::Btw)
            .expect("count");
        assert_eq!(turn_count, 1);
        assert_eq!(btw_count, 1);

        db.clear_pending(&meta.session_id, QueueType::Btw)
            .expect("clear");
        let btw_count = db
            .count_pending(&meta.session_id, QueueType::Btw)
            .expect("count");
        assert_eq!(btw_count, 0);

        let turn_count = db
            .count_pending(&meta.session_id, QueueType::Turn)
            .expect("count");
        assert_eq!(turn_count, 1);
    }

    #[test]
    fn drain_pending_empty_returns_empty() {
        let (db, _dir) = test_db();
        let meta = sample_session("session-1");
        db.upsert_session(&meta).expect("upsert");

        let drained = db
            .drain_pending(&meta.session_id, QueueType::Turn)
            .expect("drain");
        assert!(drained.is_empty());
    }
}
