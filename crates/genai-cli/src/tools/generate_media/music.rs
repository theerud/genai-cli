//! Music kind: Lyria 3 dispatch. Structured controls are
//! `input_images` (multimodal mood input, up to 10) and
//! `response_format` (mp3/wav; wav is Pro-only). Lyrics, structure
//! tags, and timing live inside the prompt text.

use anyhow::{Result, bail};
use serde_json::{Value, json};

pub fn build_schema_object() -> Value {
    json!({
        "type": "object",
        "description":
            "Music-only options. Lyrics, song structure, and timing live INSIDE the prompt text — use [Verse]/[Chorus]/[Bridge] tags for sections, embed lyrics inside the tags. Lyria 3 Pro additionally accepts [0:00-0:10]-style timestamp ranges. Duration: Lyria 3 Clip always outputs ~30s; Lyria 3 Pro defaults to ~2 minutes and is steered by prompt phrasing like 'a 2-minute piece in...'. For a long lyric sheet, write descriptor + structure + lyrics to a file and pass it via top-level prompt_file. There is no separate `lyrics` field on purpose.",
        "properties": {
            "input_images": {
                "type": "array",
                "items": {"type": "string"},
                "maxItems": 10,
                "description":
                    "Up to 10 reference images (paths) for visual mood / atmosphere / palette influence. Lyria 3 multimodal input."
            },
            "response_format": {
                "type": "string",
                "enum": ["mp3", "wav"],
                "description":
                    "Output format. Default mp3 (both models). wav is supported only by Lyria 3 Pro — the tool drops it back to mp3 with a warning when the resolved model is Lyria 3 Clip."
            }
        }
    })
}

pub fn run(
    cfg: &crate::config::Config,
    client: &crate::gemini::Client,
    prompt: String,
    model_override: Option<String>,
    output_override: Option<String>,
    music_opts: Option<&Value>,
) -> Result<Value> {
    let model_id =
        model_override.unwrap_or_else(|| cfg.media_default(crate::config::MediaKind::Music));
    let resolved = crate::models::alias::resolve(cfg, &model_id);
    let mut warnings: Vec<String> = Vec::new();

    let input_image_paths: Vec<String> = music_opts
        .and_then(|v| v.get("input_images"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if input_image_paths.len() > 10 {
        bail!(
            "music.input_images: at most 10 entries allowed (got {})",
            input_image_paths.len()
        );
    }
    let input_images = crate::output::load_input_images(&input_image_paths)?;

    // response_format: mp3 is default for both; wav is Pro-only.
    let response_format = music_opts
        .and_then(|v| v.get("response_format"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let response_format = match response_format {
        Some(f) if f == "wav" && resolved.id.contains("clip") => {
            warnings.push(format!(
                "response_format=wav not supported by {}; using default (mp3)",
                resolved.id
            ));
            None
        }
        other => other,
    };

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
        input_images,
        response_format,
    };
    let handle = tokio::runtime::Handle::current();
    let audio = tokio::task::block_in_place(|| handle.block_on(client.generate_music(req)))?;
    let bytes = audio.bytes.len();
    let mime = audio.mime.clone();
    crate::output::write_audio(&out_path, &audio)?;
    let mut out = json!({
        "kind": "music",
        "path": out_path,
        "bytes": bytes,
        "mime": mime,
        "model": resolved.id,
    });
    if !warnings.is_empty() {
        out["warnings"] = json!(warnings);
    }
    Ok(out)
}
