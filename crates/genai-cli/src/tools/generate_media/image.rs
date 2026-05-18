//! Image kind: handles both Imagen-style (structured aspect/count) and
//! conversational (nano-banana, accepts input image references).

use anyhow::Result;
use serde_json::{Value, json};

pub fn build_schema_object(image_model: &str, is_structured: bool) -> Value {
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
            "input_images".to_string(),
            json!({
                "type": "array",
                "items": {"type": "string"},
                "maxItems": 10,
                "description": format!(
                    "Optional reference image paths for {image_model} to edit / vary."
                )
            }),
        );
    }

    if is_structured {
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
    }
}

pub fn run(
    cfg: &crate::config::Config,
    client: &crate::gemini::Client,
    prompt: String,
    model_override: Option<String>,
    output_override: Option<String>,
    preview: bool,
    image_opts: Option<&Value>,
) -> Result<Value> {
    let model_id =
        model_override.unwrap_or_else(|| cfg.media_default(crate::config::MediaKind::Image));
    let resolved = crate::models::alias::resolve(cfg, &model_id);

    let aspect = image_opts
        .and_then(|v| v.get("aspect"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let count = image_opts
        .and_then(|v| v.get("count"))
        .and_then(Value::as_u64)
        .map(|n| n as u32);
    let input_images: Vec<String> = image_opts
        .and_then(|v| v.get("input_images"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let inputs = crate::output::load_input_images(&input_images)?;
    let (aspect_ratio, n, mut warnings) =
        crate::output::partition_imagen_params(&resolved.id, aspect.as_deref(), count);

    // Reference images only make sense for conversational image models.
    if !input_images.is_empty() && resolved.id.starts_with("imagen") {
        warnings.push(format!(
            "input_images ignored for {}: only image/conversational models (e.g. gemini-*-image) accept edit inputs.",
            resolved.id
        ));
    }
    let inputs_for_request = if !input_images.is_empty() && resolved.id.starts_with("imagen") {
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
    let written_strs: Vec<String> = written.iter().map(|p| p.display().to_string()).collect();

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
