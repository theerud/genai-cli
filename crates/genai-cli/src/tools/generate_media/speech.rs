//! Speech kind: single-voice or two-speaker dialog. The hard
//! validations (mutually-exclusive voice/speakers, exactly 2 speakers,
//! distinct voices and names, transcript prefix presence, catalog
//! membership) all live in `resolve_speech_config`.

use anyhow::{Result, bail};
use serde_json::{Value, json};

pub fn build_schema_object() -> Value {
    json!({
        "type": "object",
        "description": "Speech-only options. Set either `voice` (single-speaker) OR `speakers` (two-speaker dialog) — they are mutually exclusive. For two-speaker mode, the transcript in `prompt` / `prompt_file` MUST use `Name:` line prefixes that match each speaker's `name` here, otherwise the API renders the prefix as literal text in a single voice.",
        "properties": {
            "voice": {
                "type": "string",
                "enum": crate::voices::names(),
                "description": format!(
                    "Prebuilt voice name for single-speaker output. Pick by style/gender — each voice has a documented character. Catalog: {}.",
                    crate::voices::descriptor_list()
                )
            },
            "speakers": {
                "type": "array",
                "minItems": 2,
                "maxItems": 2,
                "description": "Exactly two speakers for dialog. Names must be distinct, voices must be distinct, and each name must appear as a `Name:` prefix in the transcript.",
                "items": {
                    "type": "object",
                    "required": ["name", "voice"],
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Speaker label as it appears in the transcript (e.g. 'Alice' for lines starting `Alice:`)."
                        },
                        "voice": {
                            "type": "string",
                            "enum": crate::voices::names(),
                            "description": "Prebuilt voice id from the catalog above."
                        }
                    }
                }
            }
        }
    })
}

pub fn run(
    cfg: &crate::config::Config,
    client: &crate::gemini::Client,
    text: String,
    model_override: Option<String>,
    output_override: Option<String>,
    speech_opts: Option<&Value>,
) -> Result<Value> {
    let model_id =
        model_override.unwrap_or_else(|| cfg.media_default(crate::config::MediaKind::Speech));
    let resolved = crate::models::alias::resolve(cfg, &model_id);
    let speech = resolve_speech_config(speech_opts, cfg, &text)?;

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
        speech,
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

/// Build the TTS speech config from the `speech` sub-object in args.
/// All hard validations live here.
pub(super) fn resolve_speech_config(
    speech_opts: Option<&Value>,
    cfg: &crate::config::Config,
    text: &str,
) -> Result<Option<crate::gemini::tts::SpeechConfig>> {
    use crate::gemini::tts::{SpeakerConfig, SpeechConfig};

    let Some(opts) = speech_opts else {
        // Fall back to the cfg-level default voice (legacy [model.tts].voice).
        return Ok(cfg.model.tts.voice.clone().map(SpeechConfig::Single));
    };

    let voice = opts.get("voice").and_then(Value::as_str);
    let speakers = opts.get("speakers").and_then(Value::as_array);

    if voice.is_some() && speakers.is_some() {
        bail!("speech: set either 'voice' (single) or 'speakers' (multi), not both");
    }

    if let Some(arr) = speakers {
        if arr.len() != 2 {
            bail!(
                "speech.speakers: must have exactly 2 entries (got {}); use 'voice' for single-speaker",
                arr.len()
            );
        }
        let mut configs: Vec<SpeakerConfig> = Vec::with_capacity(2);
        for (i, entry) in arr.iter().enumerate() {
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("speech.speakers[{i}]: missing 'name'"))?;
            let voice = entry
                .get("voice")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("speech.speakers[{i}]: missing 'voice'"))?;
            if crate::voices::CATALOG.iter().all(|v| v.name != voice) {
                bail!(
                    "speech.speakers[{i}].voice: '{voice}' is not in the prebuilt catalog (see `genai voices list`)"
                );
            }
            configs.push(SpeakerConfig {
                name: name.to_string(),
                voice: voice.to_string(),
            });
        }
        if configs[0].name == configs[1].name {
            bail!(
                "speech.speakers: duplicate name '{}'; speakers must have distinct labels",
                configs[0].name
            );
        }
        if configs[0].voice == configs[1].voice {
            bail!(
                "speech.speakers: both speakers use voice '{}'; pick distinct voices",
                configs[0].voice
            );
        }
        // Each speaker name must appear as a `Name:` prefix at least once
        // in the transcript text — otherwise the API renders the literal
        // `Alice:` as plain text in a single voice.
        for c in &configs {
            if !has_speaker_prefix(text, &c.name) {
                bail!(
                    "speech.speakers: '{}' is configured but no '{}:' prefix appears in the transcript",
                    c.name,
                    c.name
                );
            }
        }
        return Ok(Some(SpeechConfig::Speakers(configs)));
    }

    if let Some(v) = voice {
        if crate::voices::CATALOG.iter().all(|x| x.name != v) {
            bail!("speech.voice: '{v}' is not in the prebuilt catalog (see `genai voices list`)");
        }
        return Ok(Some(SpeechConfig::Single(v.to_string())));
    }

    Ok(cfg.model.tts.voice.clone().map(SpeechConfig::Single))
}

/// True if any line in `text` starts with `<name>:` (whitespace
/// permitted before the name).
pub(super) fn has_speaker_prefix(text: &str, name: &str) -> bool {
    let needle = format!("{name}:");
    text.lines()
        .any(|line| line.trim_start().starts_with(&needle))
}
