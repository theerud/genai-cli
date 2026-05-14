//! REPL handlers for one-off media generation: `.image`, `.tts`, `.music`.

use anyhow::Result;

use crate::gemini::image::ImageRequest;
use crate::gemini::tts::{MusicRequest, TtsRequest};
use crate::models::alias;
use crate::output;
use crate::role;
use crate::ui;

use super::ReplState;
use super::commands::ActionArgs;

pub(super) async fn handle_image_cmd(state: &mut ReplState, args: ActionArgs) -> Result<()> {
    let model_id = args
        .model
        .clone()
        .or_else(|| {
            state
                .role
                .as_ref()
                .filter(|r| {
                    !role::is_chat_capable(r, &state.registry)
                        || r.model
                            .as_deref()
                            .and_then(|m| state.registry.get(m))
                            .map(|e| e.has(crate::models::CAP_IMAGE_OUT))
                            .unwrap_or(false)
                })
                .and_then(|r| r.model.clone())
        })
        .or_else(|| state.cfg.model.image.default.clone())
        .unwrap_or_else(|| "imagen-4".to_string());
    let resolved = alias::resolve(&state.cfg, &model_id);
    crate::models::validate(&state.registry, &resolved.id, crate::models::CAP_IMAGE_OUT);

    let out_path = match &args.output {
        Some(s) => s.clone(),
        None => ui::read_line("Output path (or '-' for stdout)")?,
    };

    let inputs = output::load_input_images(&args.files)?;

    let req = ImageRequest {
        model: resolved.id,
        prompt: args.prompt.clone(),
        input_images: inputs,
        aspect_ratio: None,
        count: None,
    };

    let images = {
        let _s = crate::spinner::Spinner::start("generating image...");
        state.client.generate_image(req).await?
    };
    let preview =
        output::image_preview::Preference::from_config(state.cfg.output.image_preview.as_deref());
    output::write_images(&out_path, &images, preview)?;
    Ok(())
}

pub(super) async fn handle_tts_cmd(state: &mut ReplState, args: ActionArgs) -> Result<()> {
    let model_id = args
        .model
        .clone()
        .or_else(|| state.cfg.model.tts.default.clone())
        .unwrap_or_else(|| "gemini-2.5-flash-preview-tts".to_string());
    let resolved = alias::resolve(&state.cfg, &model_id);
    crate::models::validate(&state.registry, &resolved.id, crate::models::CAP_TTS);

    let out_path = match &args.output {
        Some(s) => s.clone(),
        None => ui::read_line("Output path (or '-' for stdout)")?,
    };
    let voice = args
        .voice
        .clone()
        .or_else(|| state.cfg.model.tts.voice.clone());

    let audio = {
        let _s = crate::spinner::Spinner::start("synthesizing speech...");
        state
            .client
            .synthesize_speech(TtsRequest {
                model: resolved.id,
                text: args.prompt,
                voice,
            })
            .await?
    };
    output::write_audio(&out_path, &audio)?;
    Ok(())
}

pub(super) async fn handle_music_cmd(state: &mut ReplState, args: ActionArgs) -> Result<()> {
    let model_id = args
        .model
        .clone()
        .or_else(|| state.role.as_ref().and_then(|r| r.model.clone()))
        .unwrap_or_else(|| "lyria-3-pro-preview".to_string());
    let resolved = alias::resolve(&state.cfg, &model_id);
    crate::models::validate(&state.registry, &resolved.id, crate::models::CAP_MUSIC_OUT);

    let out_path = match &args.output {
        Some(s) => s.clone(),
        None => ui::read_line("Output path (or '-' for stdout)")?,
    };

    let audio = {
        let _s = crate::spinner::Spinner::start("generating music...");
        state
            .client
            .generate_music(MusicRequest {
                model: resolved.id,
                prompt: args.prompt,
            })
            .await?
    };
    output::write_audio(&out_path, &audio)?;
    Ok(())
}
