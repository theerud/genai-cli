pub mod attachment;
pub mod db;

use anyhow::Result;
use base64::Engine;
use std::io::Write;
use std::path::PathBuf;

use crate::config;
use crate::gemini::types::{Content, InlineData, Part};
use attachment::Attachment;
use db::{Database, MessageRecord, Session as DbSession, SessionSummary};

pub struct ActiveSession {
    pub db_session: DbSession,
}

impl ActiveSession {
    pub fn id(&self) -> i64 {
        self.db_session.id
    }
    pub fn name(&self) -> Option<&str> {
        if self.db_session.ephemeral {
            None
        } else {
            Some(&self.db_session.name)
        }
    }
    pub fn label(&self) -> &str {
        if self.db_session.ephemeral {
            "*"
        } else {
            &self.db_session.name
        }
    }
    pub fn ephemeral(&self) -> bool {
        self.db_session.ephemeral
    }
}

pub fn db_path() -> Result<PathBuf> {
    let p = config::paths()?;
    Ok(p.data_dir.join("data.db"))
}

pub fn open_db() -> Result<Database> {
    Database::open(&db_path()?)
}

pub fn messages_to_contents(msgs: &[MessageRecord]) -> Vec<Content> {
    msgs.iter()
        .map(|m| Content {
            role: Some(m.role.clone()),
            parts: m.parts.clone(),
        })
        .collect()
}

pub fn export_jsonl<W: Write>(
    db: &Database,
    session: &DbSession,
    mut out: W,
) -> Result<()> {
    let header = serde_json::json!({
        "type": "session",
        "name": session.name,
        "model": session.model,
        "system_prompt": session.system_prompt,
    });
    writeln!(out, "{header}")?;
    for m in db.load_messages(session.id)? {
        let line = serde_json::json!({
            "seq": m.seq,
            "role": m.role,
            "parts": m.parts.iter().map(part_to_json).collect::<Vec<_>>(),
            "created_at": m.created_at,
        });
        writeln!(out, "{line}")?;
    }
    Ok(())
}

fn part_to_json(p: &Part) -> serde_json::Value {
    match p {
        Part::Text { text } => serde_json::json!({"type":"text","text":text}),
        Part::InlineData { inline_data } => serde_json::json!({
            "type":"inline_data",
            "mime_type": inline_data.mime_type,
            "size": inline_data.data.len(),
        }),
        Part::FunctionCall { function_call, .. } => serde_json::json!({
            "type":"function_call",
            "name": function_call.name,
            "args": function_call.args,
        }),
        Part::FunctionResponse { function_response } => serde_json::json!({
            "type":"function_response",
            "name": function_response.name,
            "response": function_response.response,
        }),
    }
}

/// Load files from disk, build a user message that inlines them, return the
/// message plus the loaded attachments (caller decides whether to persist).
///
/// When `files` is non-empty, the prompt is prefixed with a
/// `[attached: <abs-path>, ...]` line so tools that take path arguments
/// (e.g. `generate_media.image.input_paths`) can be invoked with the
/// actual file paths rather than relying on the LLM to invent them.
pub fn build_user_message(prompt: String, files: &[PathBuf]) -> Result<(Content, Vec<Attachment>)> {
    let mut parts: Vec<Part> = Vec::with_capacity(1 + files.len());
    let resolved_paths: Vec<String> = files
        .iter()
        .map(|f| {
            std::fs::canonicalize(f)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| f.display().to_string())
        })
        .collect();
    let prompt_with_preamble = if resolved_paths.is_empty() {
        prompt
    } else {
        format!("[attached: {}]\n{prompt}", resolved_paths.join(", "))
    };
    parts.push(Part::Text { text: prompt_with_preamble });
    let mut atts = Vec::with_capacity(files.len());
    for f in files {
        let att = attachment::load(f)?;
        let data = base64::engine::general_purpose::STANDARD.encode(&att.bytes);
        parts.push(Part::InlineData {
            inline_data: InlineData {
                mime_type: att.mime.clone(),
                data,
            },
        });
        atts.push(att);
    }
    Ok((
        Content {
            role: Some("user".to_string()),
            parts,
        },
        atts,
    ))
}

/// For each attachment: copy blob to disk, ensure DB row exists, return hashes.
pub fn persist_attachments(db: &mut Database, atts: &[Attachment]) -> Result<Vec<String>> {
    let mut hashes = Vec::with_capacity(atts.len());
    for a in atts {
        attachment::store(a)?;
        db.upsert_attachment(&a.hash, Some(&a.mime), a.size)?;
        hashes.push(a.hash.clone());
    }
    Ok(hashes)
}

pub fn gc_blobs(db: &mut Database) -> Result<usize> {
    let orphans = db.orphan_attachment_hashes()?;
    let n = orphans.len();
    for hash in orphans {
        attachment::delete_blob(&hash)?;
        db.delete_attachment_row(&hash)?;
    }
    Ok(n)
}

pub fn format_summary(s: &SessionSummary, current: Option<&str>) -> String {
    let marker = if Some(s.name.as_str()) == current { "*" } else { " " };
    format!(
        "{marker} #{:<4} {:<24} {:<28}  {} msg",
        s.id,
        s.name,
        s.model.as_deref().unwrap_or("-"),
        s.message_count
    )
}
