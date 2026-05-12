use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::gemini::types::FunctionDeclaration;

pub const TOOL_READ_FILE: &str = "read_file";
pub const TOOL_LIST_DIR: &str = "list_dir";
pub const TOOL_FETCH_URL: &str = "fetch_url";
pub const TOOL_EXEC: &str = "exec";

pub const LOCAL_TOOL_NAMES: &[&str] = &[TOOL_READ_FILE, TOOL_LIST_DIR, TOOL_FETCH_URL, TOOL_EXEC];

const MAX_READ_BYTES: usize = 256 * 1024;
const MAX_LIST_ENTRIES: usize = 200;
const MAX_FETCH_BYTES: usize = 1024 * 1024;
const FETCH_TIMEOUT_SECS: u64 = 15;
const EXEC_TIMEOUT_SECS: u64 = 30;
const EXEC_MAX_OUTPUT: usize = 64 * 1024;

/// A client-side tool that Gemini can call via function calling.
pub trait LocalTool: Sync + Send {
    fn declaration(&self) -> FunctionDeclaration;
    /// True when the tool may have side effects and should be gated by a
    /// user-facing confirmation prompt before each call.
    fn requires_confirmation(&self) -> bool {
        false
    }
    /// Run synchronously. Async would be cleaner for fetch_url, but keeping
    /// the trait object-safe and synchronous is enough for v0. The fetch
    /// implementation runs the request on a fresh blocking client.
    fn run(&self, args: &Value) -> Result<Value>;
    /// One-line, user-facing summary of an invocation (rendered in the REPL).
    fn describe_call(&self, args: &Value) -> String;
}

pub fn local_names() -> &'static [&'static str] {
    LOCAL_TOOL_NAMES
}

pub fn lookup_local(name: &str) -> Option<&'static dyn LocalTool> {
    match name {
        TOOL_READ_FILE => Some(&ReadFile),
        TOOL_LIST_DIR => Some(&ListDir),
        TOOL_FETCH_URL => Some(&FetchUrl),
        TOOL_EXEC => Some(&Exec),
        _ => None,
    }
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
        let path = expand_tilde(str_arg(args, "path")?);
        let meta = std::fs::metadata(&path)?;
        if !meta.is_file() {
            bail!("not a regular file: {path}");
        }
        let bytes = std::fs::read(&path)?;
        let total = bytes.len();
        let truncated = total > MAX_READ_BYTES;
        let slice = if truncated { &bytes[..MAX_READ_BYTES] } else { &bytes[..] };
        let text = String::from_utf8_lossy(slice).into_owned();
        Ok(json!({
            "path": path,
            "bytes": total,
            "truncated": truncated,
            "content": text,
        }))
    }
}

// ---------- list_dir ----------

struct ListDir;

impl LocalTool for ListDir {

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
        let path = expand_tilde(str_arg(args, "path")?);
        let meta = std::fs::metadata(&path)?;
        if !meta.is_dir() {
            bail!("not a directory: {path}");
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
            "path": path,
            "entries": entries,
            "truncated": truncated,
        }))
    }
}

// ---------- fetch_url ----------

struct FetchUrl;

impl LocalTool for FetchUrl {

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
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let deadline = Instant::now() + Duration::from_secs(EXEC_TIMEOUT_SECS);
        let exit_status = loop {
            if let Some(status) = child.try_wait()? {
                break Some(status);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };

        let timed_out = exit_status.is_none();
        let exit_code = exit_status.and_then(|s| s.code());
        let stdout = read_capped(child.stdout.take(), EXEC_MAX_OUTPUT);
        let stderr = read_capped(child.stderr.take(), EXEC_MAX_OUTPUT);
        Ok(json!({
            "command": cmd,
            "exit_code": exit_code,
            "timed_out": timed_out,
            "stdout": stdout,
            "stderr": stderr,
        }))
    }
}

fn read_capped<R: std::io::Read>(stream: Option<R>, cap: usize) -> String {
    let Some(mut r) = stream else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = std::io::Read::read_to_end(&mut r, &mut buf);
    let truncated = buf.len() > cap;
    if truncated {
        buf.truncate(cap);
    }
    let mut out = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        out.push_str("\n…[truncated]");
    }
    out
}

fn expand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    s.to_string()
}
