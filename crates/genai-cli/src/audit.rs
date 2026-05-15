//! Append-only audit log for tool invocations. One JSON line per call
//! at `<data_dir>/tool-log.jsonl`. Soft line-count cap with in-place
//! trim — keep the last `max_lines` once the file grows 10% past that.
//!
//! Best-effort: I/O failures are logged via `tracing` but never bubble
//! up. We don't want a full disk to break tool execution.

use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

struct Settings {
    enabled: bool,
    max_lines: usize,
    path: Option<PathBuf>,
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();

fn settings() -> &'static Settings {
    SETTINGS.get_or_init(|| {
        let cfg = crate::config::load().unwrap_or_default();
        let path = crate::config::paths().ok().map(|p| p.data_dir.join("tool-log.jsonl"));
        Settings {
            enabled: cfg.security.audit.enabled && path.is_some(),
            max_lines: cfg.security.audit.max_lines.max(1),
            path,
        }
    })
}

/// Path to the audit log, if logging is enabled and the data dir is
/// resolvable. Returns `None` when the log is disabled or unreachable.
pub fn log_path() -> Option<&'static std::path::Path> {
    let s = settings();
    if !s.enabled {
        return None;
    }
    s.path.as_deref()
}

/// Read the last `n` lines of the audit log, oldest-first. Returns an
/// empty vec if the log doesn't exist or is disabled.
pub fn tail(n: usize) -> Vec<String> {
    let Some(path) = log_path() else {
        return Vec::new();
    };
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = contents.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].iter().map(|s| s.to_string()).collect()
}

/// Per-tool list of argument names whose values are always replaced
/// with a `"<redacted, N bytes>"` placeholder before logging — for
/// fields that carry whole-file payloads or could carry secrets.
const REDACT_BY_TOOL: &[(&str, &[&str])] = &[
    ("write_file", &["content"]),
];

/// Case-insensitive substrings that mark a field as secret regardless
/// of which tool produced it. Catches user-defined tools that accept
/// API keys, tokens, passwords, etc.
const SECRET_NAME_SUBSTRINGS: &[&str] = &[
    "password", "passwd", "secret", "token", "api_key", "apikey", "credential",
];

/// Any individual string field longer than this is replaced with its
/// head + "<truncated, N bytes total>" tail. Keeps the audit line
/// readable and bounded in size.
const MAX_FIELD_BYTES: usize = 4096;

/// Strip / cap fields likely to leak content or secrets. Operates on
/// the top-level object only — secrets in deeply nested user-tool
/// shapes will still show up, but the common shapes (write_file.content,
/// flat user-tool args) are covered.
fn redact_args(tool: &str, args: &Value) -> Value {
    let mut redacted = args.clone();
    if let Value::Object(map) = &mut redacted {
        let tool_fields: &[&str] = REDACT_BY_TOOL
            .iter()
            .find(|(t, _)| *t == tool)
            .map(|(_, f)| *f)
            .unwrap_or(&[]);
        for (k, v) in map.iter_mut() {
            let key_lower = k.to_ascii_lowercase();
            let force = tool_fields.contains(&k.as_str())
                || SECRET_NAME_SUBSTRINGS.iter().any(|p| key_lower.contains(p));
            *v = redact_value(v, force);
        }
    }
    redacted
}

fn redact_value(v: &Value, force: bool) -> Value {
    if force {
        let n = match v {
            Value::String(s) => s.len(),
            other => other.to_string().len(),
        };
        return json!(format!("<redacted, {n} bytes>"));
    }
    if let Value::String(s) = v
        && s.len() > MAX_FIELD_BYTES
    {
        let head_end = utf8_floor(s, MAX_FIELD_BYTES);
        return json!(format!("{}…<truncated, {} bytes total>", &s[..head_end], s.len()));
    }
    v.clone()
}

/// Largest valid UTF-8 boundary `<= cap` in `s`. Avoids slicing inside
/// a multi-byte char when we truncate.
fn utf8_floor(s: &str, cap: usize) -> usize {
    if s.len() <= cap {
        return s.len();
    }
    let mut idx = cap;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

/// Record a tool invocation. `result` is one of `"ok"`, `"err"`,
/// `"denied"`. `preview` is a short human-readable summary already shown
/// to the user; we replay it into the audit line so the log is
/// self-contained. Argument values are redacted / truncated before
/// serialization — see `redact_args`.
pub fn log(tool: &str, args: &Value, result: &str, preview: &str) {
    let s = settings();
    if !s.enabled {
        return;
    }
    let Some(path) = s.path.as_deref() else {
        return;
    };
    let entry = json!({
        "ts": now_iso(),
        "tool": tool,
        "args": redact_args(tool, args),
        "result": result,
        "preview": preview,
    });
    let line = match serde_json::to_string(&entry) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "audit: failed to serialize");
            return;
        }
    };
    if let Err(e) = append_line(path, &line) {
        tracing::warn!(error = %e, "audit: append failed");
        return;
    }
    if let Err(e) = trim_if_needed(path, s.max_lines) {
        tracing::warn!(error = %e, "audit: trim failed");
    }
}

fn append_line(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{line}")
}

fn trim_if_needed(path: &std::path::Path, max_lines: usize) -> std::io::Result<()> {
    // Hysteresis: only trim when we've grown 10% past the cap.
    let trigger = max_lines + max_lines / 10;
    let contents = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = contents.lines().collect();
    if lines.len() <= trigger {
        return Ok(());
    }
    let keep_from = lines.len() - max_lines;
    let kept: String = lines[keep_from..].join("\n");
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, format!("{kept}\n"))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_unix_iso(secs as i64)
}

fn format_unix_iso(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let secs_in_day = secs.rem_euclid(86400);
    let h = secs_in_day / 3600;
    let m = (secs_in_day % 3600) / 60;
    let s = secs_in_day % 60;
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

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
    use tempfile::TempDir;

    #[test]
    fn redacts_write_file_content() {
        let args = json!({"path": "/tmp/foo", "content": "the quick brown fox"});
        let red = redact_args("write_file", &args);
        assert_eq!(red["path"], json!("/tmp/foo"));
        let c = red["content"].as_str().unwrap();
        assert!(c.starts_with("<redacted, "));
        assert!(c.contains("19 bytes"));
    }

    #[test]
    fn redacts_secret_named_fields_regardless_of_tool() {
        let args = json!({"endpoint": "https://api.x", "api_key": "sk-abc123"});
        let red = redact_args("any_tool", &args);
        assert_eq!(red["endpoint"], json!("https://api.x"));
        assert!(red["api_key"].as_str().unwrap().starts_with("<redacted, "));
    }

    #[test]
    fn truncates_long_string_field() {
        let long = "x".repeat(MAX_FIELD_BYTES + 50);
        let args = json!({"command": long});
        let red = redact_args("exec", &args);
        let c = red["command"].as_str().unwrap();
        assert!(c.ends_with("bytes total>"));
        assert!(c.len() < MAX_FIELD_BYTES + 100);
    }

    #[test]
    fn small_fields_pass_through() {
        let args = json!({"url": "https://example.com"});
        let red = redact_args("fetch_url", &args);
        assert_eq!(red, args);
    }

    #[test]
    fn utf8_floor_doesnt_split_multibyte() {
        let s = "abcé"; // 'é' is 2 bytes -> total 5 bytes
        // cap=4 lands on first byte of 'é'; floor should back to 3.
        assert_eq!(utf8_floor(s, 4), 3);
        // cap that hits a boundary stays put.
        assert_eq!(utf8_floor(s, 3), 3);
        // cap larger than s returns s.len().
        assert_eq!(utf8_floor(s, 100), s.len());
    }

    #[test]
    fn trim_keeps_last_max_lines_after_threshold() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("log.jsonl");
        let max = 10;
        // Trigger is max + max/10 = 11. Write exactly 11 — no trim.
        for i in 0..11 {
            append_line(&path, &format!("line {i}")).unwrap();
        }
        trim_if_needed(&path, max).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 11);

        // One more line pushes past the trigger; trim back to max.
        append_line(&path, "line 11").unwrap();
        trim_if_needed(&path, max).unwrap();
        let after: Vec<String> = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        assert_eq!(after.len(), 10);
        assert_eq!(after[0], "line 2"); // oldest two dropped
        assert_eq!(after[9], "line 11");
    }
}
