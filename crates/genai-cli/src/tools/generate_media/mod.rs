//! `generate_media` — the unified image / speech / music tool. The
//! schema is shaped per-turn by the active role's image model so the
//! LLM only sees parameters the active backend actually accepts. Three
//! per-kind handler files live alongside this one.

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::gemini::types::FunctionDeclaration;
use crate::tools::local::{LocalTool, canonicalize_parent, str_arg};

mod image;
mod music;
mod speech;
#[cfg(test)]
mod tests;

pub const TOOL_GENERATE_MEDIA: &str = "generate_media";

/// Safety cap for `prompt_file`. The API has tighter limits — this is
/// just a sanity net against the LLM passing a gigabyte log file by
/// mistake.
const MAX_PROMPT_FILE_BYTES: u64 = 1024 * 1024;

pub struct GenerateMedia;

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
        // `build_declaration` via `build_request_tools`, which passes the
        // active role/cfg. This impl exists so the `LocalTool` trait
        // stays uniform and tests that probe the trait method still work.
        let cfg = crate::config::load().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "generate_media declaration: config load failed");
            crate::config::Config::default()
        });
        build_declaration(&crate::tools::DeclarationContext {
            role: None,
            cfg: &cfg,
        })
    }

    fn describe_call(&self, args: &Value) -> String {
        let kind = args.get("kind").and_then(Value::as_str).unwrap_or("?");
        let prompt = if let Some(file) = args.get("prompt_file").and_then(Value::as_str) {
            format!("from {file}")
        } else {
            args.get("prompt")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .take(60)
                .collect::<String>()
        };
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
        let prompt = resolve_prompt(args)?;
        let cfg = crate::config::load()?;
        let api_key = cfg.require_api_key()?.to_string();
        let base = cfg.api_base().to_string();
        let client = crate::gemini::Client::new(api_key, base)?;

        let model_override = args.get("model").and_then(Value::as_str).map(String::from);
        let output_override = args
            .get("output_path")
            .and_then(Value::as_str)
            .map(String::from);
        // Default true: a one-off generation should flash on screen on a
        // capable terminal. Loop-mode roles that don't want intermediate
        // previews instruct the model to set this false in the system
        // prompt. image_preview::show is silent when the terminal can't
        // render, so leaving it on is safe.
        let preview = args.get("preview").and_then(Value::as_bool).unwrap_or(true);

        match kind.as_str() {
            "image" => image::run(
                &cfg,
                &client,
                prompt,
                model_override,
                output_override,
                preview,
                args.get("image"),
            ),
            "speech" => speech::run(
                &cfg,
                &client,
                prompt,
                model_override,
                output_override,
                args.get("speech"),
            ),
            "music" => music::run(
                &cfg,
                &client,
                prompt,
                model_override,
                output_override,
                args.get("music"),
            ),
            other => bail!("invalid kind '{other}': expected image / speech / music"),
        }
    }

    fn normalize_for_policy(&self, args: &Value) -> Value {
        let mut out = args.clone();
        if let Some(p) = out.get("output_path").and_then(Value::as_str) {
            out["output_path"] = json!(canonicalize_parent(p));
        }
        if let Some(p) = out.get("prompt_file").and_then(Value::as_str) {
            // prompt_file is a read — fully canonicalize when the file
            // exists so the policy floor sees the resolved path.
            let expanded = crate::output::expand_path(p);
            let canon = std::fs::canonicalize(&expanded)
                .map(|c| c.display().to_string())
                .unwrap_or(expanded);
            out["prompt_file"] = json!(canon);
        }
        out
    }
}

pub fn build_declaration(
    ctx: &crate::tools::DeclarationContext<'_>,
) -> FunctionDeclaration {
    // Resolve the *effective* image model for this turn (role overrides
    // win over cfg.media, which wins over the legacy [model.image].default,
    // which wins over a hardcoded fallback). The schema is then shaped to
    // only expose parameters that model accepts — no aspect/count for a
    // conversational model, no input_images for an Imagen one — so the
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
        group = if is_structured {
            "structured / Imagen-style"
        } else {
            "conversational / nano-banana-style"
        }
    );

    let image_obj = image::build_schema_object(&image_model, is_structured);
    let speech_obj = speech::build_schema_object();
    let music_obj = music::build_schema_object();

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
                    "description": "Image/music: descriptive prompt. Speech: the text to read aloud. Mutually exclusive with prompt_file."
                },
                "prompt_file": {
                    "type": "string",
                    "description": "Absolute path to a UTF-8 text file whose content is used as the prompt. Mutually exclusive with prompt. Use this for long inputs (e.g., podcast transcripts) so the content doesn't have to be re-emitted by the model. When the user attached a text file via -f, its path appears in the [attached: ...] preamble — use that path here."
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
                "speech": speech_obj,
                "music": music_obj
            },
            "required": ["kind"]
        }),
    }
}

/// Resolve the prompt content for `generate_media`. Either `prompt` is
/// set (literal string) or `prompt_file` points at a UTF-8 text file we
/// read from disk. Mutually exclusive — both unset or both set is an
/// error.
fn resolve_prompt(args: &Value) -> Result<String> {
    let inline = args
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let file = args
        .get("prompt_file")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    match (inline, file) {
        (Some(p), None) => Ok(p.to_string()),
        (None, Some(path)) => {
            let expanded = crate::output::expand_path(path);
            let canon = std::fs::canonicalize(&expanded)
                .map_err(|e| anyhow::anyhow!("cannot resolve prompt_file '{expanded}': {e}"))?;
            let meta = std::fs::metadata(&canon)?;
            if !meta.is_file() {
                bail!("prompt_file is not a regular file: {}", canon.display());
            }
            if meta.len() > MAX_PROMPT_FILE_BYTES {
                bail!(
                    "prompt_file exceeds {} KB cap ({} bytes)",
                    MAX_PROMPT_FILE_BYTES / 1024,
                    meta.len()
                );
            }
            let text = std::fs::read_to_string(&canon)?;
            Ok(text)
        }
        (Some(_), Some(_)) => bail!("set either 'prompt' or 'prompt_file', not both"),
        (None, None) => bail!("missing 'prompt' or 'prompt_file'"),
    }
}
