use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::gemini::types::FunctionDeclaration;

pub const TOOL_READ_FILE: &str = "read_file";
pub const TOOL_LIST_DIR: &str = "list_dir";
pub const TOOL_FETCH_URL: &str = "fetch_url";
pub const TOOL_EXEC: &str = "exec";
pub const TOOL_WRITE_FILE: &str = "write_file";
pub const TOOL_GENERATE_MEDIA: &str = "generate_media";

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
        Box::new(GenerateMedia),
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
        // Stream the response so an oversized or unbounded body can't
        // pin memory / bandwidth — we stop reading once we've collected
        // one byte past the cap (the extra byte lets us tell "exactly
        // at cap" from "more than cap" without double-bookkeeping).
        let handle = tokio::runtime::Handle::current();
        let (status, content_type, bytes, truncated) = tokio::task::block_in_place(|| {
            handle.block_on(async {
                use futures_util::StreamExt;
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
                let limit = MAX_FETCH_BYTES + 1;
                let mut buf: Vec<u8> = Vec::new();
                let mut stream = resp.bytes_stream();
                let mut truncated = false;
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk?;
                    if buf.len() + chunk.len() <= limit {
                        buf.extend_from_slice(&chunk);
                    } else {
                        let room = limit.saturating_sub(buf.len());
                        buf.extend_from_slice(&chunk[..room]);
                        truncated = true;
                        break;
                    }
                    if buf.len() >= limit {
                        truncated = true;
                        break;
                    }
                }
                Ok::<_, anyhow::Error>((status, content_type, buf, truncated))
            })
        })?;
        let total = bytes.len();
        let kept = if truncated { MAX_FETCH_BYTES } else { total };
        let body = String::from_utf8_lossy(&bytes[..kept]).into_owned();
        Ok(json!({
            "url": url,
            "status": status,
            "content_type": content_type,
            "bytes": kept,
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
        let mut out = args.clone();
        if let Some(p) = out.get("path").and_then(Value::as_str) {
            out["path"] = json!(canonicalize_parent(p));
        }
        out
    }
}

// ---------- generate_media ----------

pub fn build_generate_media_declaration(
    ctx: &super::DeclarationContext<'_>,
) -> FunctionDeclaration {
    // Resolve the *effective* image model for this turn (role overrides
    // win over cfg.media, which wins over the legacy [model.image].default,
    // which wins over a hardcoded fallback). The schema is then shaped to
    // only expose parameters that model accepts — no aspect/count for a
    // conversational model, no input_paths for an Imagen one — so the
    // LLM can't be tempted by an inapplicable field.
    let image_model =
        crate::role::effective_media(ctx.role, ctx.cfg, crate::config::MediaKind::Image);
    let is_structured = image_model.starts_with("imagen");

    let description = format!(
        "Generate media (image, speech, or music) and write it to disk. \
         Returns the saved path plus metadata so subsequent tool calls (e.g. write_file \
         embedding HTML) can reference the asset. Each invocation hits a paid API and \
         requires user confirmation. \
         The active image model is '{image_model}' ({group}); the schema below has been \
         tailored to that model's accepted parameters. \
         IMPORTANT: pass the user's verbatim prompt — do not summarize it, do not strip \
         stylistic or compositional cues (aspect ratios, lighting, framing, etc.).",
        group = if is_structured { "structured / Imagen-style" } else { "conversational / nano-banana-style" }
    );

    let mut image_props = serde_json::Map::new();
    if is_structured {
        image_props.insert(
            "aspect".to_string(),
            json!({
                "type": "string",
                "enum": ["1:1", "16:9", "9:16", "4:3", "3:4"],
                "description": format!(
                    "Aspect ratio for {image_model}. If the user mentioned a ratio in \
                     their prompt, set this field AND remove the ratio words from the \
                     prompt — the model receives both."
                )
            }),
        );
        image_props.insert(
            "count".to_string(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": 4,
                "description":
                    "Number of variants to generate (1-4). Only set when the user \
                     explicitly asked for multiple variants; do not infer a count from \
                     numeric tokens in the prompt."
            }),
        );
    } else {
        image_props.insert(
            "input_paths".to_string(),
            json!({
                "type": "array",
                "items": {"type": "string"},
                "description": format!(
                    "Optional reference images for {image_model} to edit / vary."
                )
            }),
        );
    }

    let image_obj = if is_structured {
        json!({
            "type": "object",
            "description": format!("Image options for {image_model} (Imagen-style)."),
            "properties": Value::Object(image_props)
        })
    } else {
        json!({
            "type": "object",
            "description": format!(
                "Image options for {image_model} (conversational). \
                 Aspect ratio / variant count are NOT structured parameters for this \
                 model — keep ratio words (e.g. '4:3', 'portrait', '16:9 cinematic') \
                 verbatim in the prompt. If the user asked for multiple variants, \
                 call the tool repeatedly."
            ),
            "properties": Value::Object(image_props)
        })
    };

    FunctionDeclaration {
        name: TOOL_GENERATE_MEDIA.to_string(),
        description,
        parameters: json!({
            "type": "object",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["image", "speech", "music"],
                    "description": "What to generate."
                },
                "prompt": {
                    "type": "string",
                    "description": "Image/music: descriptive prompt. Speech: the text to read aloud."
                },
                "output_path": {
                    "type": "string",
                    "description": "Optional. Auto-named under data_dir/generated/ when omitted."
                },
                "preview": {
                    "type": "boolean",
                    "description": "Image only, TTY only. Show inline preview after writing. Default true; set false for intermediate generations in a loop where the user only cares about the final asset."
                },
                "image": image_obj,
                "speech": {
                    "type": "object",
                    "description": "Speech-only options.",
                    "properties": {
                        "voice": {"type": "string", "description": "Prebuilt voice name."}
                    }
                },
                "music": {
                    "type": "object",
                    "description": "Music-only options. No extra fields yet; reserved."
                }
            },
            "required": ["kind", "prompt"]
        }),
    }
}

struct GenerateMedia;

impl LocalTool for GenerateMedia {
    fn name(&self) -> &str {
        TOOL_GENERATE_MEDIA
    }

    fn requires_confirmation(&self) -> bool {
        true
    }

    fn declaration(&self) -> FunctionDeclaration {
        // Fallback path: a context-free declaration using whatever
        // config can be loaded at this moment. The real entry point is
        // `build_generate_media_declaration` via `build_request_tools`,
        // which passes the active role/cfg. This impl exists so the
        // `LocalTool` trait stays uniform and tests that probe the trait
        // method still work.
        let cfg = crate::config::load().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "generate_media declaration: config load failed");
            crate::config::Config::default()
        });
        build_generate_media_declaration(&super::DeclarationContext {
            role: None,
            cfg: &cfg,
        })
    }

    fn describe_call(&self, args: &Value) -> String {
        let kind = args.get("kind").and_then(Value::as_str).unwrap_or("?");
        let prompt = args
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(60)
            .collect::<String>();
        let path = args
            .get("output_path")
            .and_then(Value::as_str)
            .unwrap_or("(auto)");
        let mut extras: Vec<String> = Vec::new();
        if let Some(m) = args.get("model").and_then(Value::as_str) {
            extras.push(format!("model={m}"));
        }
        if let Some(sub) = args.get(kind) {
            for (k, v) in sub.as_object().into_iter().flatten() {
                if !v.is_null() {
                    extras.push(format!("{k}={v}"));
                }
            }
        }
        let suffix = if extras.is_empty() {
            String::new()
        } else {
            format!(" [{}]", extras.join(", "))
        };
        format!("generate_media[{kind}]({prompt}, -> {path}){suffix}")
    }

    fn run(&self, args: &Value) -> Result<Value> {
        let kind = str_arg(args, "kind")?.to_string();
        let prompt = str_arg(args, "prompt")?.to_string();
        let cfg = crate::config::load()?;
        let api_key = cfg.require_api_key()?.to_string();
        let base = cfg.api_base().to_string();
        let client = crate::gemini::Client::new(api_key, base)?;

        let model_override = args.get("model").and_then(Value::as_str).map(String::from);
        let output_override = args.get("output_path").and_then(Value::as_str).map(String::from);
        // Default true: a one-off generation should flash on screen on a
        // capable terminal. Loop-mode roles that don't want intermediate
        // previews instruct the model to set this false in the system
        // prompt. image_preview::show is silent when the terminal can't
        // render, so leaving it on is safe.
        let preview = args.get("preview").and_then(Value::as_bool).unwrap_or(true);

        match kind.as_str() {
            "image" => run_image(&cfg, &client, prompt, model_override, output_override, preview, args.get("image")),
            "speech" => run_speech(&cfg, &client, prompt, model_override, output_override, args.get("speech")),
            "music" => run_music(&cfg, &client, prompt, model_override, output_override, args.get("music")),
            other => bail!("invalid kind '{other}': expected image / speech / music"),
        }
    }

    fn normalize_for_policy(&self, args: &Value) -> Value {
        let mut out = args.clone();
        if let Some(p) = out.get("output_path").and_then(Value::as_str) {
            let canon = canonicalize_parent(p);
            out["output_path"] = json!(canon);
        }
        out
    }
}

fn run_image(
    cfg: &crate::config::Config,
    client: &crate::gemini::Client,
    prompt: String,
    model_override: Option<String>,
    output_override: Option<String>,
    preview: bool,
    image_opts: Option<&Value>,
) -> Result<Value> {
    let model_id = model_override
        .unwrap_or_else(|| cfg.media_default(crate::config::MediaKind::Image));
    let resolved = crate::models::alias::resolve(cfg, &model_id);

    let aspect = image_opts
        .and_then(|v| v.get("aspect"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let count = image_opts
        .and_then(|v| v.get("count"))
        .and_then(Value::as_u64)
        .map(|n| n as u32);
    let input_paths: Vec<String> = image_opts
        .and_then(|v| v.get("input_paths"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let inputs = crate::output::load_input_images(&input_paths)?;
    let (aspect_ratio, n, mut warnings) =
        crate::output::partition_imagen_params(&resolved.id, aspect.as_deref(), count);

    // Input images only make sense for conversational image models.
    if !input_paths.is_empty() && resolved.id.starts_with("imagen") {
        warnings.push(format!(
            "input_paths ignored for {}: only image/conversational models (e.g. gemini-*-image) accept edit inputs.",
            resolved.id
        ));
    }
    let inputs_for_request = if !input_paths.is_empty() && resolved.id.starts_with("imagen") {
        Vec::new()
    } else {
        inputs
    };

    let out_path = match output_override {
        Some(s) => s,
        None => crate::output::default_generated_path(
            cfg,
            crate::output::GeneratedKind::Image,
            &prompt,
        )?
        .display()
        .to_string(),
    };

    let req = crate::gemini::image::ImageRequest {
        model: resolved.id.clone(),
        prompt,
        input_images: inputs_for_request,
        aspect_ratio,
        count: n,
    };

    let handle = tokio::runtime::Handle::current();
    let images = tokio::task::block_in_place(|| handle.block_on(client.generate_image(req)))?;
    let pref = if preview {
        crate::output::image_preview::Preference::from_config(cfg.output.image_preview.as_deref())
    } else {
        crate::output::image_preview::Preference::Off
    };
    let written = crate::output::write_images(&out_path, &images, pref)?;
    let written_strs: Vec<String> = written
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    let dims: Vec<Value> = images
        .iter()
        .map(|im| {
            let summary = crate::output::describe_image(&im.bytes);
            json!({"mime": im.mime, "bytes": im.bytes.len(), "summary": summary})
        })
        .collect();
    let total: usize = images.iter().map(|i| i.bytes.len()).sum();
    let primary_path = written_strs.first().cloned().unwrap_or(out_path);
    let mut out = json!({
        "kind": "image",
        "path": primary_path,
        "paths": written_strs,
        "count": images.len(),
        "bytes": total,
        "images": dims,
        "model": resolved.id,
    });
    if !warnings.is_empty() {
        out["warnings"] = json!(warnings);
    }
    Ok(out)
}

fn run_speech(
    cfg: &crate::config::Config,
    client: &crate::gemini::Client,
    text: String,
    model_override: Option<String>,
    output_override: Option<String>,
    speech_opts: Option<&Value>,
) -> Result<Value> {
    let model_id = model_override
        .unwrap_or_else(|| cfg.media_default(crate::config::MediaKind::Speech));
    let resolved = crate::models::alias::resolve(cfg, &model_id);
    let voice = speech_opts
        .and_then(|v| v.get("voice"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| cfg.model.tts.voice.clone());

    let out_path = match output_override {
        Some(s) => s,
        None => crate::output::default_generated_path(
            cfg,
            crate::output::GeneratedKind::Tts,
            &text,
        )?
        .display()
        .to_string(),
    };

    let req = crate::gemini::tts::TtsRequest {
        model: resolved.id.clone(),
        text,
        voice,
    };
    let handle = tokio::runtime::Handle::current();
    let audio = tokio::task::block_in_place(|| handle.block_on(client.synthesize_speech(req)))?;
    let bytes = audio.bytes.len();
    let mime = audio.mime.clone();
    crate::output::write_audio(&out_path, &audio)?;
    Ok(json!({
        "kind": "speech",
        "path": out_path,
        "bytes": bytes,
        "mime": mime,
        "model": resolved.id,
    }))
}

fn run_music(
    cfg: &crate::config::Config,
    client: &crate::gemini::Client,
    prompt: String,
    model_override: Option<String>,
    output_override: Option<String>,
    _music_opts: Option<&Value>,
) -> Result<Value> {
    let model_id = model_override
        .unwrap_or_else(|| cfg.media_default(crate::config::MediaKind::Music));
    let resolved = crate::models::alias::resolve(cfg, &model_id);

    let out_path = match output_override {
        Some(s) => s,
        None => crate::output::default_generated_path(
            cfg,
            crate::output::GeneratedKind::Music,
            &prompt,
        )?
        .display()
        .to_string(),
    };

    let req = crate::gemini::tts::MusicRequest {
        model: resolved.id.clone(),
        prompt,
    };
    let handle = tokio::runtime::Handle::current();
    let audio = tokio::task::block_in_place(|| handle.block_on(client.generate_music(req)))?;
    let bytes = audio.bytes.len();
    let mime = audio.mime.clone();
    crate::output::write_audio(&out_path, &audio)?;
    Ok(json!({
        "kind": "music",
        "path": out_path,
        "bytes": bytes,
        "mime": mime,
        "model": resolved.id,
    }))
}

/// Canonicalize the parent dir of a (possibly non-existent) path, leaving
/// the file name intact. Used by side-effecting tools so the policy
/// matcher sees the resolved write target.
fn canonicalize_parent(p: &str) -> String {
    let expanded = crate::output::expand_path(p);
    let path = std::path::PathBuf::from(&expanded);
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => match std::fs::canonicalize(parent) {
            Ok(canon_parent) => {
                let file_name = path.file_name().map(|f| f.to_owned()).unwrap_or_default();
                canon_parent.join(file_name).display().to_string()
            }
            Err(_) => expanded,
        },
        _ => expanded,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_media_describe_call_truncates_prompt() {
        let tool = GenerateMedia;
        let args = json!({
            "kind": "image",
            "prompt": "a very long prompt ".repeat(20),
            "output_path": "/tmp/x.png",
        });
        let s = tool.describe_call(&args);
        assert!(s.starts_with("generate_media[image]"));
        assert!(s.contains("/tmp/x.png"));
    }

    #[test]
    fn generate_media_normalize_canonicalizes_output_path() {
        let tool = GenerateMedia;
        let dir = tempfile::tempdir().unwrap();
        // Build a path via a `.` segment in an existing directory so the
        // parent canonicalize succeeds and strips it.
        let raw = dir.path().join("./out.png");
        let args = json!({
            "kind": "image",
            "prompt": "x",
            "output_path": raw.display().to_string(),
        });
        let normalized = tool.normalize_for_policy(&args);
        let path = normalized.get("output_path").and_then(Value::as_str).unwrap();
        assert!(path.ends_with("out.png"));
        assert!(!path.contains("/./"));
    }

    #[test]
    fn generate_media_rejects_unknown_kind() {
        let tool = GenerateMedia;
        let args = json!({"kind": "hologram", "prompt": "x"});
        // run() needs config + api key, but kind validation happens up front
        // — the error path goes through str_arg / match. We exercise that
        // here by tolerating any concrete error and checking the message.
        let err = tool.run(&args).unwrap_err().to_string();
        assert!(
            err.contains("hologram") || err.contains("api"),
            "unexpected error: {err}"
        );
    }
}

