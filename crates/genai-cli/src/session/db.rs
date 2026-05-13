use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::Path;

use crate::gemini::types::{Content, Part};

pub struct Database {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: i64,
    pub name: String,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub ephemeral: bool,
}

#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: i64,
    pub name: String,
    pub model: Option<String>,
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
    ephemeral     INTEGER NOT NULL DEFAULT 0,
    meta          TEXT NOT NULL DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS messages (
    id          INTEGER PRIMARY KEY,
    session_id  INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,
    turn_id     INTEGER NOT NULL DEFAULT 0,
    role        TEXT NOT NULL,
    parts       TEXT NOT NULL,
    token_count INTEGER,
    created_at  TEXT NOT NULL,
    UNIQUE(session_id, seq)
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq);
CREATE INDEX IF NOT EXISTS idx_messages_turn    ON messages(session_id, turn_id);

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

const SCHEMA_VERSION: i32 = 3;

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
            if version < 2 {
                migrate_v1_to_v2(&conn)?;
            }
            if version < 3 {
                migrate_v2_to_v3(&conn)?;
            }
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { conn })
    }

    pub fn get_session(&self, name: &str) -> Result<Option<Session>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, name, model, system_prompt, ephemeral FROM sessions WHERE name = ?1",
                params![name],
                |r| {
                    Ok(Session {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        model: r.get(2)?,
                        system_prompt: r.get(3)?,
                        ephemeral: r.get::<_, i64>(4)? != 0,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn get_session_by_id(&self, id: i64) -> Result<Option<Session>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, name, model, system_prompt, ephemeral FROM sessions WHERE id = ?1",
                params![id],
                |r| {
                    Ok(Session {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        model: r.get(2)?,
                        system_prompt: r.get(3)?,
                        ephemeral: r.get::<_, i64>(4)? != 0,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn resolve_session_ref(&self, value: &str) -> Result<Option<Session>> {
        if let Ok(id) = value.parse::<i64>()
            && id > 0
                && let Some(s) = self.get_session_by_id(id)? {
                    return Ok(Some(s));
                }
        self.get_session(value)
    }

    pub fn create_session(
        &mut self,
        name: &str,
        model: Option<&str>,
        system_prompt: Option<&str>,
    ) -> Result<Session> {
        self.create_session_inner(name, model, system_prompt, false)
    }

    /// Create a session with a generated unique placeholder name and the
    /// `ephemeral` flag set. The caller flips the flag (and renames) via
    /// `promote_to_named` if the user decides to keep it.
    pub fn create_ephemeral_session(
        &mut self,
        model: Option<&str>,
        system_prompt: Option<&str>,
    ) -> Result<Session> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let name = format!("__ephemeral_{nanos:032}");
        self.create_session_inner(&name, model, system_prompt, true)
    }

    fn create_session_inner(
        &mut self,
        name: &str,
        model: Option<&str>,
        system_prompt: Option<&str>,
        ephemeral: bool,
    ) -> Result<Session> {
        let now = now_iso();
        self.conn.execute(
            "INSERT INTO sessions (name, model, system_prompt, created_at, updated_at, ephemeral) \
             VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            params![name, model, system_prompt, now, ephemeral as i64],
        )?;
        let id = self.conn.last_insert_rowid();
        Ok(Session {
            id,
            name: name.to_string(),
            model: model.map(String::from),
            system_prompt: system_prompt.map(String::from),
            ephemeral,
        })
    }

    /// Rename a session and clear its ephemeral flag. Used when the user
    /// saves a temporary session under a real name.
    pub fn promote_to_named(&mut self, session_id: i64, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET name = ?1, ephemeral = 0 WHERE id = ?2",
            params![name, session_id],
        )?;
        Ok(())
    }

    /// Rename a session without changing its ephemeral status.
    pub fn rename_session(&mut self, session_id: i64, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET name = ?1 WHERE id = ?2",
            params![name, session_id],
        )?;
        Ok(())
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
            "SELECT s.id, s.name, s.model, COUNT(m.id) \
             FROM sessions s LEFT JOIN messages m ON m.session_id = s.id \
             WHERE s.ephemeral = 0 \
             GROUP BY s.id ORDER BY s.updated_at DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SessionSummary {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    model: r.get(2)?,
                    message_count: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn delete_session_ref(&mut self, value: &str) -> Result<bool> {
        if let Ok(id) = value.parse::<i64>()
            && id > 0 {
                let n = self
                    .conn
                    .execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
                return Ok(n > 0);
            }
        let n = self
            .conn
            .execute("DELETE FROM sessions WHERE name = ?1", params![value])?;
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
        self.commit_exchange(
            session_id,
            user,
            std::slice::from_ref(assistant),
            model,
            attachment_hashes,
        )
    }

    /// Like `commit_turn`, but the assistant side may be a chain of multiple
    /// model and tool-response messages produced by a function-calling loop.
    /// All messages share one `turn_id` and are persisted in one transaction.
    pub fn commit_exchange(
        &mut self,
        session_id: i64,
        user: &Content,
        chain: &[Content],
        model: Option<&str>,
        attachment_hashes: &[String],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let next_seq = next_seq(&tx, session_id)?;
        let next_turn = next_turn_id(&tx, session_id)?;
        let now = now_iso();

        let user_id = insert_message(&tx, session_id, next_seq, next_turn, user, &now)?;
        for hash in attachment_hashes {
            tx.execute(
                "INSERT OR IGNORE INTO message_attachments (message_id, attachment_hash) VALUES (?1, ?2)",
                params![user_id, hash],
            )?;
        }
        for (i, msg) in chain.iter().enumerate() {
            insert_message(&tx, session_id, next_seq + 1 + i as i64, next_turn, msg, &now)?;
        }
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

    /// Drop the last completed turn — all messages sharing the highest
    /// `turn_id` for this session. Works for plain chat (2 rows) and for
    /// function-calling exchanges (more). Returns the number of rows removed.
    pub fn pop_last_turn(&mut self, session_id: i64) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let last_turn: Option<i64> = tx
            .query_row(
                "SELECT MAX(turn_id) FROM messages WHERE session_id = ?1",
                params![session_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let Some(turn) = last_turn else {
            return Ok(0);
        };
        let removed = tx.execute(
            "DELETE FROM messages WHERE session_id = ?1 AND turn_id = ?2",
            params![session_id, turn],
        )?;
        tx.commit()?;
        Ok(removed)
    }
}

fn migrate_v2_to_v3(conn: &Connection) -> Result<()> {
    let has_col: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'ephemeral'",
        [],
        |r| r.get(0),
    )?;
    if has_col == 0 {
        conn.execute_batch("ALTER TABLE sessions ADD COLUMN ephemeral INTEGER NOT NULL DEFAULT 0;")?;
    }
    // Mark any legacy PID-named temp sessions ephemeral so they stop showing
    // up in listings. The user can delete them manually if desired.
    conn.execute_batch(
        "UPDATE sessions SET ephemeral = 1 WHERE ephemeral = 0 AND name LIKE '__tmp_repl_%';",
    )?;
    Ok(())
}

fn migrate_v1_to_v2(conn: &Connection) -> Result<()> {
    // v1 had no turn_id column; the migration was just the ALTER TABLE,
    // which CREATE TABLE IF NOT EXISTS won't apply to an existing table.
    let has_col: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'turn_id'",
        [],
        |r| r.get(0),
    )?;
    if has_col == 0 {
        conn.execute_batch("ALTER TABLE messages ADD COLUMN turn_id INTEGER NOT NULL DEFAULT 0;")?;
    }
    // Backfill turn_id assuming the legacy 2-message-per-turn invariant.
    // Tool exchanges committed under v1 will be split apart by this — not
    // ideal, but `.undo` on those was already broken under the old code.
    conn.execute_batch(
        "UPDATE messages SET turn_id = (seq + 1) / 2 WHERE turn_id = 0;\
         CREATE INDEX IF NOT EXISTS idx_messages_turn ON messages(session_id, turn_id);",
    )?;
    Ok(())
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

fn next_turn_id(tx: &Transaction, session_id: i64) -> Result<i64> {
    let v: Option<i64> = tx
        .query_row(
            "SELECT MAX(turn_id) FROM messages WHERE session_id = ?1",
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
    turn_id: i64,
    msg: &Content,
    now: &str,
) -> Result<i64> {
    let role = msg.role.as_deref().unwrap_or("user");
    let parts_json = serde_json::to_string(&msg.parts)?;
    tx.execute(
        "INSERT INTO messages (session_id, seq, turn_id, role, parts, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![session_id, seq, turn_id, role, parts_json, now],
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
    fn pop_last_turn_handles_tool_exchange() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let mut db = Database::open(&path).unwrap();
        let s = db.create_session("t", None, None).unwrap();

        // Plain turn: 2 rows.
        db.commit_turn(s.id, &user("hi"), &assistant("hello"), None, &[])
            .unwrap();
        // Tool exchange: user + 3 chain rows (model fn-call, tool response, model text).
        let model_call = Content {
            role: Some("model".into()),
            parts: vec![Part::Text { text: "calling tool".into() }],
        };
        let tool_resp = Content {
            role: Some("user".into()),
            parts: vec![Part::Text { text: "tool result".into() }],
        };
        let final_text = Content {
            role: Some("model".into()),
            parts: vec![Part::Text { text: "answer".into() }],
        };
        db.commit_exchange(
            s.id,
            &user("what's up"),
            &[model_call, tool_resp, final_text],
            None,
            &[],
        )
        .unwrap();
        assert_eq!(db.load_messages(s.id).unwrap().len(), 6);

        // Undo should remove the entire tool exchange (4 rows), not just 2.
        let removed = db.pop_last_turn(s.id).unwrap();
        assert_eq!(removed, 4);
        assert_eq!(db.load_messages(s.id).unwrap().len(), 2);

        // Undo again should remove the plain turn (2 rows).
        let removed = db.pop_last_turn(s.id).unwrap();
        assert_eq!(removed, 2);
        assert_eq!(db.load_messages(s.id).unwrap().len(), 0);

        // Undo on an empty session returns 0.
        assert_eq!(db.pop_last_turn(s.id).unwrap(), 0);
    }

    #[test]
    fn list_and_delete() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let mut db = Database::open(&path).unwrap();
        db.create_session("a", None, None).unwrap();
        db.create_session("b", None, None).unwrap();
        assert_eq!(db.list_sessions().unwrap().len(), 2);
        assert!(db.delete_session_ref("a").unwrap());
        assert_eq!(db.list_sessions().unwrap().len(), 1);
    }

    #[test]
    fn ephemeral_sessions_excluded_from_list() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let mut db = Database::open(&path).unwrap();
        db.create_session("real", None, None).unwrap();
        let tmp = db.create_ephemeral_session(None, None).unwrap();
        assert!(tmp.ephemeral);
        let names: Vec<_> = db
            .list_sessions()
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["real".to_string()]);
        // Promote and confirm it shows up.
        db.promote_to_named(tmp.id, "promoted").unwrap();
        let mut names: Vec<_> = db
            .list_sessions()
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["promoted".to_string(), "real".to_string()]);
    }

    #[test]
    fn iso_format_is_sane() {
        let s = format_unix_iso(0);
        assert_eq!(s, "1970-01-01T00:00:00Z");
        let s = format_unix_iso(1700000000);
        assert!(s.starts_with("2023-11-14T"), "got {s}");
    }
}
