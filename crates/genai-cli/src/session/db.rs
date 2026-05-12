use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::Path;

use crate::gemini::types::{Content, Part};

pub struct Database {
    pub(crate) conn: Connection,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: i64,
    pub name: String,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub name: String,
    pub model: Option<String>,
    pub updated_at: String,
    pub message_count: i64,
}

#[derive(Debug, Clone)]
pub struct MessageRecord {
    pub seq: i64,
    pub role: String,
    pub parts: Vec<Part>,
    pub created_at: String,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id            INTEGER PRIMARY KEY,
    name          TEXT UNIQUE NOT NULL,
    model         TEXT,
    system_prompt TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    meta          TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS messages (
    id          INTEGER PRIMARY KEY,
    session_id  INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,
    role        TEXT NOT NULL,
    parts       TEXT NOT NULL,
    token_count INTEGER,
    created_at  TEXT NOT NULL,
    UNIQUE(session_id, seq)
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);

CREATE TABLE IF NOT EXISTS attachments (
    hash       TEXT PRIMARY KEY,
    mime       TEXT,
    size       INTEGER,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS message_attachments (
    message_id      INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    attachment_hash TEXT NOT NULL REFERENCES attachments(hash),
    PRIMARY KEY(message_id, attachment_hash)
);
"#;

const SCHEMA_VERSION: i32 = 1;

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < SCHEMA_VERSION {
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { conn })
    }

    pub fn get_session(&self, name: &str) -> Result<Option<Session>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, name, model, system_prompt FROM sessions WHERE name = ?1",
                params![name],
                |r| {
                    Ok(Session {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        model: r.get(2)?,
                        system_prompt: r.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn create_session(
        &mut self,
        name: &str,
        model: Option<&str>,
        system_prompt: Option<&str>,
    ) -> Result<Session> {
        let now = now_iso();
        self.conn.execute(
            "INSERT INTO sessions (name, model, system_prompt, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![name, model, system_prompt, now],
        )?;
        let id = self.conn.last_insert_rowid();
        Ok(Session {
            id,
            name: name.to_string(),
            model: model.map(String::from),
            system_prompt: system_prompt.map(String::from),
        })
    }

    pub fn get_or_create_session(
        &mut self,
        name: &str,
        model: Option<&str>,
        system_prompt: Option<&str>,
    ) -> Result<Session> {
        if let Some(s) = self.get_session(name)? {
            return Ok(s);
        }
        self.create_session(name, model, system_prompt)
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.name, s.model, s.updated_at, COUNT(m.id) \
             FROM sessions s LEFT JOIN messages m ON m.session_id = s.id \
             GROUP BY s.id ORDER BY s.updated_at DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SessionSummary {
                    name: r.get(0)?,
                    model: r.get(1)?,
                    updated_at: r.get(2)?,
                    message_count: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn delete_session(&mut self, name: &str) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM sessions WHERE name = ?1", params![name])?;
        Ok(n > 0)
    }

    pub fn load_messages(&self, session_id: i64) -> Result<Vec<MessageRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, role, parts, created_at FROM messages \
             WHERE session_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt
            .query_map(params![session_id], |r| {
                let parts_json: String = r.get(2)?;
                let parts: Vec<Part> = serde_json::from_str(&parts_json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                Ok(MessageRecord {
                    seq: r.get(0)?,
                    role: r.get(1)?,
                    parts,
                    created_at: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Atomic commit of a completed turn: insert user message + assistant message,
    /// bump session.updated_at.
    pub fn commit_turn(
        &mut self,
        session_id: i64,
        user: &Content,
        assistant: &Content,
        model: Option<&str>,
        attachment_hashes: &[String],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let next_seq = next_seq(&tx, session_id)?;
        let now = now_iso();

        let user_id = insert_message(&tx, session_id, next_seq, user, &now)?;
        for hash in attachment_hashes {
            tx.execute(
                "INSERT OR IGNORE INTO message_attachments (message_id, attachment_hash) VALUES (?1, ?2)",
                params![user_id, hash],
            )?;
        }
        insert_message(&tx, session_id, next_seq + 1, assistant, &now)?;
        tx.execute(
            "UPDATE sessions SET updated_at = ?1, model = COALESCE(?2, model) WHERE id = ?3",
            params![now, model, session_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn upsert_attachment(&mut self, hash: &str, mime: Option<&str>, size: u64) -> Result<()> {
        let now = now_iso();
        self.conn.execute(
            "INSERT OR IGNORE INTO attachments (hash, mime, size, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![hash, mime, size as i64, now],
        )?;
        Ok(())
    }

    pub fn orphan_attachment_hashes(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.hash FROM attachments a \
             LEFT JOIN message_attachments ma ON ma.attachment_hash = a.hash \
             WHERE ma.attachment_hash IS NULL",
        )?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn delete_attachment_row(&mut self, hash: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM attachments WHERE hash = ?1", params![hash])?;
        Ok(())
    }

    /// Drop the last completed turn (the highest-seq assistant message plus the
    /// preceding user message) from a session. Returns true if anything was dropped.
    pub fn pop_last_turn(&mut self, session_id: i64) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let last_seq: Option<i64> = tx
            .query_row(
                "SELECT MAX(seq) FROM messages WHERE session_id = ?1",
                params![session_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let Some(seq) = last_seq else {
            return Ok(false);
        };
        // Drop last assistant + preceding user. If history is odd (shouldn't be),
        // dropping the last message alone is still better than nothing.
        let lo = (seq - 1).max(1);
        tx.execute(
            "DELETE FROM messages WHERE session_id = ?1 AND seq >= ?2",
            params![session_id, lo],
        )?;
        tx.commit()?;
        Ok(true)
    }
}

fn next_seq(tx: &Transaction, session_id: i64) -> Result<i64> {
    let v: Option<i64> = tx
        .query_row(
            "SELECT MAX(seq) FROM messages WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(v.map(|s| s + 1).unwrap_or(1))
}

fn insert_message(
    tx: &Transaction,
    session_id: i64,
    seq: i64,
    msg: &Content,
    now: &str,
) -> Result<i64> {
    let role = msg.role.as_deref().unwrap_or("user");
    let parts_json = serde_json::to_string(&msg.parts)?;
    tx.execute(
        "INSERT INTO messages (session_id, seq, role, parts, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![session_id, seq, role, parts_json, now],
    )?;
    Ok(tx.last_insert_rowid())
}

fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_unix_iso(secs as i64)
}

fn format_unix_iso(secs: i64) -> String {
    // Minimal UTC formatter: YYYY-MM-DDTHH:MM:SSZ
    let days = secs.div_euclid(86400);
    let secs_in_day = secs.rem_euclid(86400);
    let h = secs_in_day / 3600;
    let m = (secs_in_day % 3600) / 60;
    let s = secs_in_day % 60;
    let (y, mo, d) = civil_from_days(days);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, m, s)
}

// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i32) + (era as i32) * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gemini::types::Part;
    use tempfile::TempDir;

    fn user(text: &str) -> Content {
        Content {
            role: Some("user".into()),
            parts: vec![Part::Text { text: text.into() }],
        }
    }

    fn assistant(text: &str) -> Content {
        Content {
            role: Some("model".into()),
            parts: vec![Part::Text { text: text.into() }],
        }
    }

    #[test]
    fn round_trip_session() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let mut db = Database::open(&path).unwrap();
        let s = db.create_session("foo", Some("gemini-2.5-flash"), None).unwrap();
        db.commit_turn(s.id, &user("hi"), &assistant("hello"), Some("gemini-2.5-flash"), &[])
            .unwrap();
        let msgs = db.load_messages(s.id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[1].role, "model");
    }

    #[test]
    fn list_and_delete() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let mut db = Database::open(&path).unwrap();
        db.create_session("a", None, None).unwrap();
        db.create_session("b", None, None).unwrap();
        assert_eq!(db.list_sessions().unwrap().len(), 2);
        assert!(db.delete_session("a").unwrap());
        assert_eq!(db.list_sessions().unwrap().len(), 1);
    }

    #[test]
    fn iso_format_is_sane() {
        let s = format_unix_iso(0);
        assert_eq!(s, "1970-01-01T00:00:00Z");
        let s = format_unix_iso(1700000000);
        assert!(s.starts_with("2023-11-14T"), "got {s}");
    }
}
