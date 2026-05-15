use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::gemini::types::FunctionDeclaration;

pub const TOOL_READ_FILE: &str = "read_file";
pub const TOOL_LIST_DIR: &str = "list_dir";
pub const TOOL_FETCH_URL: &str = "fetch_url";
pub const TOOL_EXEC: &str = "exec";
pub const TOOL_WRITE_FILE: &str = "write_file";

const MAX_READ_BYTES: usize = 256 * 1024;
const MAX_LIST_ENTRIES: usize = 200;
const MAX_FETCH_BYTES: usize = 1024 * 1024;
const FETCH_TIMEOUT_SECS: u64 = 15;
const EXEC_TIMEOUT_SECS: u64 = 30;
const EXEC_MAX_OUTPUT: usize = 64 * 1024;
const MAX_WRITE_BYTES: usize = 10 * 1024 * 1024;

/// A client-side tool that Gemini can call via function calling.
pub trait LocalTool: Sync + Send {
    fn name(&self) -> &str;
    fn declaration(&self) -> FunctionDeclaration;
    /// True when the tool may have side effects and should be gated by a
    /// user-facing confirmation prompt before each call.
    fn requires_confirmation(&self) -> bool {
        false
    }
    fn run(&self, args: &Value) -> Result<Value>;
    /// One-line, user-facing summary of an invocation (rendered in the REPL).
    fn describe_call(&self, args: &Value) -> String;

    /// Pre-process args before policy evaluation: canonicalize paths,
    /// extract hosts from URLs, etc. The returned value is what the
    /// rule matcher sees. Default: pass through unchanged.
    fn normalize_for_policy(&self, args: &Value) -> Value {
        args.clone()
    }
}

/// All built-in client-side tools as boxed trait objects. The registry layer
/// merges these with any user-defined tools discovered on disk.
pub fn builtin_locals() -> Vec<Box<dyn LocalTool>> {
    vec![
        Box::new(ReadFile),
        Box::new(ListDir),
        Box::new(FetchUrl),
        Box::new(Exec),
        Box::new(WriteFile),
    ]
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing required string argument '{key}'"))
}

// ---------- read_file ----------

struct ReadFile;

impl LocalTool for ReadFile {
    fn name(&self) -> &str {
        TOOL_READ_FILE
    }

    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration {
            name: TOOL_READ_FILE.to_string(),
            description: format!(
                "Read up to {} KB of UTF-8 text from a local file path. \
                 Returns the file contents truncated to that size. \
                 Use this to inspect source code, config, logs, or other text files \
                 the user has on disk.",
                MAX_READ_BYTES / 1024
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or tilde-expanded path to a file."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn describe_call(&self, args: &Value) -> String {
        format!("read_file({})", args.get("path").and_then(Value::as_str).unwrap_or("?"))
    }

    fn run(&self, args: &Value) -> Result<Value> {
        let raw = str_arg(args, "path")?;
        let expanded = crate::output::expand_path(raw);
        let path = std::fs::canonicalize(&expanded)
            .map_err(|e| anyhow::anyhow!("cannot resolve {expanded}: {e}"))?;
        let meta = std::fs::metadata(&path)?;
        if !meta.is_file() {
            bail!("not a regular file: {}", path.display());
        }
        let bytes = std::fs::read(&path)?;
        let total = bytes.len();
        let truncated = total > MAX_READ_BYTES;
        let slice = if truncated { &bytes[..MAX_READ_BYTES] } else { &bytes[..] };
        let text = String::from_utf8_lossy(slice).into_owned();
        Ok(json!({
            "path": path.display().to_string(),
            "bytes": total,
            "truncated": truncated,
            "content": text,
        }))
    }

    fn normalize_for_policy(&self, args: &Value) -> Value {
        canonicalize_path_arg(args, "path")
    }
}

// ---------- list_dir ----------

struct ListDir;

impl LocalTool for ListDir {
    fn name(&self) -> &str {
        TOOL_LIST_DIR
    }

    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration {
            name: TOOL_LIST_DIR.to_string(),
            description: format!(
                "List the contents of a directory. Returns up to {MAX_LIST_ENTRIES} entries \
                 with name, type ('file'|'dir'|'other'), and size for files. \
                 Use this to discover what's in a directory before reading specific files."
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or tilde-expanded path to a directory."
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn describe_call(&self, args: &Value) -> String {
        format!("list_dir({})", args.get("path").and_then(Value::as_str).unwrap_or("?"))
    }

    fn run(&self, args: &Value) -> Result<Value> {
        let raw = str_arg(args, "path")?;
        let expanded = crate::output::expand_path(raw);
        let path = std::fs::canonicalize(&expanded)
            .map_err(|e| anyhow::anyhow!("cannot resolve {expanded}: {e}"))?;
        let meta = std::fs::metadata(&path)?;
        if !meta.is_dir() {
            bail!("not a directory: {}", path.display());
        }
        let mut entries = Vec::new();
        let mut truncated = false;
        for (i, entry) in std::fs::read_dir(&path)?.enumerate() {
            if i >= MAX_LIST_ENTRIES {
                truncated = true;
                break;
            }
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let ft = entry.file_type()?;
            let kind = if ft.is_dir() { "dir" } else if ft.is_file() { "file" } else { "other" };
            let mut item = json!({"name": name, "type": kind});
            if ft.is_file()
                && let Ok(m) = entry.metadata()
            {
                item["size"] = json!(m.len());
            }
            entries.push(item);
        }
        Ok(json!({
            "path": path.display().to_string(),
            "entries": entries,
            "truncated": truncated,
        }))
    }

    fn normalize_for_policy(&self, args: &Value) -> Value {
        canonicalize_path_arg(args, "path")
    }
}

// ---------- fetch_url ----------

struct FetchUrl;

impl LocalTool for FetchUrl {
    fn name(&self) -> &str {
        TOOL_FETCH_URL
    }

    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration {
            name: TOOL_FETCH_URL.to_string(),
            description: format!(
                "HTTP GET a URL and return the response body as text \
                 (up to {} KB). Only http/https schemes are allowed. Use this to fetch web \
                 pages, JSON APIs, or other text resources when answering the user.",
                MAX_FETCH_BYTES / 1024
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "http:// or https:// URL to fetch."
                    }
                },
                "required": ["url"]
            }),
        }
    }

    fn describe_call(&self, args: &Value) -> String {
        format!("fetch_url({})", args.get("url").and_then(Value::as_str).unwrap_or("?"))
    }

    fn run(&self, args: &Value) -> Result<Value> {
        let url = str_arg(args, "url")?.to_string();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            bail!("only http(s) URLs allowed");
        }
        // The trait is sync but we run inside a multi-thread tokio runtime;
        // block in place and reuse the async reqwest client.
        let handle = tokio::runtime::Handle::current();
        let (status, content_type, bytes) = tokio::task::block_in_place(|| {
            handle.block_on(async {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
                    .user_agent(concat!("genai-cli/", env!("CARGO_PKG_VERSION")))
                    .build()?;
                let resp = client.get(&url).send().await?;
                let status = resp.status().as_u16();
                let content_type = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let bytes = resp.bytes().await?;
                Ok::<_, anyhow::Error>((status, content_type, bytes))
            })
        })?;
        let total = bytes.len();
        let truncated = total > MAX_FETCH_BYTES;
        let slice = if truncated { &bytes[..MAX_FETCH_BYTES] } else { &bytes[..] };
        let body = String::from_utf8_lossy(slice).into_owned();
        Ok(json!({
            "url": url,
            "status": status,
            "content_type": content_type,
            "bytes": total,
            "truncated": truncated,
            "body": body,
        }))
    }
}

// ---------- exec ----------

struct Exec;

impl LocalTool for Exec {
    fn name(&self) -> &str {
        TOOL_EXEC
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration {
            name: TOOL_EXEC.to_string(),
            description: format!(
                "Execute a shell command on the user's machine and return its stdout, stderr, \
                 and exit code. The command is run via `sh -c`. Output is truncated to {} KB. \
                 Each invocation requires explicit user confirmation, so prefer a single \
                 well-formed command over many small calls.",
                EXEC_MAX_OUTPUT / 1024
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command to run via `sh -c`."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    fn describe_call(&self, args: &Value) -> String {
        let cmd = args.get("command").and_then(Value::as_str).unwrap_or("?");
        format!("exec({cmd})")
    }

    fn run(&self, args: &Value) -> Result<Value> {
        let cmd = str_arg(args, "command")?.to_string();
        let captured = super::process::run_with_caps(
            std::process::Command::new("sh").arg("-c").arg(&cmd),
            std::time::Duration::from_secs(EXEC_TIMEOUT_SECS),
            EXEC_MAX_OUTPUT,
        )?;
        Ok(json!({
            "command": cmd,
            "exit_code": captured.exit_code,
            "timed_out": captured.timed_out,
            "stdout": captured.stdout,
            "stderr": captured.stderr,
        }))
    }
}

// ---------- write_file ----------

struct WriteFile;

impl LocalTool for WriteFile {
    fn name(&self) -> &str {
        TOOL_WRITE_FILE
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    fn declaration(&self) -> FunctionDeclaration {
        FunctionDeclaration {
            name: TOOL_WRITE_FILE.to_string(),
            description: format!(
                "Write UTF-8 text to a local file. Creates parent directories. \
                 Default mode 'overwrite' replaces the file; 'append' appends to it. \
                 Content is capped at {} KB. Each invocation requires explicit user \
                 confirmation.",
                MAX_WRITE_BYTES / 1024
            ),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute or tilde-expanded path to write."
                    },
                    "content": {
                        "type": "string",
                        "description": "UTF-8 text to write."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["overwrite", "append"],
                        "description": "How to write. Defaults to 'overwrite'."
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    fn describe_call(&self, args: &Value) -> String {
        let path = args.get("path").and_then(Value::as_str).unwrap_or("?");
        let bytes = args.get("content").and_then(Value::as_str).map(str::len).unwrap_or(0);
        let mode = args.get("mode").and_then(Value::as_str).unwrap_or("overwrite");
        format!("write_file({path}, {bytes} B, {mode})")
    }

    fn run(&self, args: &Value) -> Result<Value> {
        let raw = str_arg(args, "path")?;
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing 'content'"))?;
        if content.len() > MAX_WRITE_BYTES {
            bail!(
                "content exceeds {} KB cap ({} bytes)",
                MAX_WRITE_BYTES / 1024,
                content.len()
            );
        }
        let mode = args.get("mode").and_then(Value::as_str).unwrap_or("overwrite");
        let append = match mode {
            "overwrite" => false,
            "append" => true,
            other => bail!("invalid mode '{other}': expected 'overwrite' or 'append'"),
        };
        let expanded = crate::output::expand_path(raw);
        let path = std::path::PathBuf::from(&expanded);
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("creating parent {}: {e}", parent.display()))?;
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true);
        if append {
            opts.append(true);
        } else {
            opts.truncate(true);
        }
        use std::io::Write as _;
        let mut f = opts
            .open(&path)
            .map_err(|e| anyhow::anyhow!("opening {}: {e}", path.display()))?;
        f.write_all(content.as_bytes())?;
        let total = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Ok(json!({
            "path": path.display().to_string(),
            "bytes": content.len(),
            "total_bytes": total,
            "mode": mode,
        }))
    }

    fn normalize_for_policy(&self, args: &Value) -> Value {
        // Canonicalize against the parent directory when the file doesn't
        // exist yet — that's the case the policy actually needs to see.
        let mut out = args.clone();
        if let Some(p) = out.get("path").and_then(Value::as_str) {
            let expanded = crate::output::expand_path(p);
            let path = std::path::PathBuf::from(&expanded);
            let resolved = match path.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => match std::fs::canonicalize(parent)
                {
                    Ok(canon_parent) => {
                        let file_name = path.file_name().map(|f| f.to_owned()).unwrap_or_default();
                        canon_parent.join(file_name).display().to_string()
                    }
                    Err(_) => expanded,
                },
                _ => expanded,
            };
            out["path"] = json!(resolved);
        }
        out
    }
}

/// Replace a path-valued arg with its canonicalized absolute form. Used
/// by tools that take path args (`read_file`, `list_dir`) so the policy
/// matcher sees the resolved target — a symlink can't bypass a deny rule.
/// If the path doesn't exist yet (rare for read tools), passes through.
fn canonicalize_path_arg(args: &Value, key: &str) -> Value {
    let mut out = args.clone();
    if let Some(p) = out.get(key).and_then(Value::as_str) {
        let expanded = crate::output::expand_path(p);
        if let Ok(c) = std::fs::canonicalize(&expanded) {
            out[key] = json!(c.display().to_string());
        } else {
            out[key] = json!(expanded);
        }
    }
    out
}

