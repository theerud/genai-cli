use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::Deserialize;

use super::Client;
use super::types::{ApiErrorEnvelope, Content, Part};

pub struct TtsRequest {
    pub model: String,
    pub text: String,
    /// Speech config. Either single-voice (`Single(name)`) or
    /// multi-speaker (`Speakers([{name, voice}; 2])`). When None, the
    /// API's default voice is used.
    pub speech: Option<SpeechConfig>,
}

#[derive(Debug, Clone)]
pub enum SpeechConfig {
    Single(String),
    Speakers(Vec<SpeakerConfig>),
}

#[derive(Debug, Clone)]
pub struct SpeakerConfig {
    pub name: String,
    pub voice: String,
}

pub struct MusicRequest {
    pub model: String,
    pub prompt: String,
    /// Reference images for visual mood/atmosphere influence (Lyria
    /// multimodal input). Up to 10.
    pub input_images: Vec<super::image::InputImage>,
    /// Output format. `Some("mp3")` or `Some("wav")`; None lets the
    /// API pick its default. Lyria 3 Clip only supports mp3 — the
    /// tool layer drops wav with a warning for Clip before reaching
    /// this point.
    pub response_format: Option<String>,
}

pub struct AudioOut {
    pub mime: String,
    pub bytes: Vec<u8>,
    /// Sample rate in Hz, parsed from mime parameters when present.
    pub sample_rate: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenContentResponse {
    #[serde(default)]
    candidates: Vec<GenCandidate>,
}

#[derive(Deserialize)]
struct GenCandidate {
    content: Option<Content>,
}

impl Client {
    pub async fn synthesize_speech(&self, req: TtsRequest) -> Result<AudioOut> {
        let url = format!("{}/v1beta/models/{}:generateContent", self.base, req.model);
        let speech_config = match &req.speech {
            Some(SpeechConfig::Single(name)) => serde_json::json!({
                "voiceConfig": {
                    "prebuiltVoiceConfig": {"voiceName": name}
                }
            }),
            Some(SpeechConfig::Speakers(speakers)) => {
                let configs: Vec<_> = speakers
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "speaker": s.name,
                            "voiceConfig": {
                                "prebuiltVoiceConfig": {"voiceName": s.voice}
                            }
                        })
                    })
                    .collect();
                serde_json::json!({
                    "multiSpeakerVoiceConfig": {
                        "speakerVoiceConfigs": configs
                    }
                })
            }
            None => serde_json::json!({
                "voiceConfig": {
                    "prebuiltVoiceConfig": {"voiceName": "Kore"}
                }
            }),
        };
        let body = serde_json::json!({
            "contents": [{"role": "user", "parts": [{"text": req.text}]}],
            "generationConfig": {
                "responseModalities": ["AUDIO"],
                "speechConfig": speech_config
            }
        });

        let resp = self
            .http
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("TTS request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if let Ok(env) = serde_json::from_str::<ApiErrorEnvelope>(&text) {
                bail!("TTS {}: {}", status, env.error.message);
            }
            bail!("TTS {}: {}", status, text);
        }

        let parsed: GenContentResponse = resp.json().await.context("parsing TTS response")?;
        for c in parsed.candidates {
            let Some(content) = c.content else { continue };
            for part in content.parts {
                if let Part::InlineData { inline_data } = part {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(inline_data.data.as_bytes())
                        .context("decoding TTS audio base64")?;
                    let sample_rate = parse_sample_rate(&inline_data.mime_type);
                    return Ok(AudioOut {
                        mime: inline_data.mime_type,
                        bytes,
                        sample_rate,
                    });
                }
            }
        }
        bail!("TTS returned no audio");
    }
}

impl Client {
    /// Generate music via Lyria. Uses the same generateContent + AUDIO modality
    /// path as TTS, without speechConfig. If Lyria's endpoint shape differs
    /// in the deployed API, this call will surface the server's error verbatim.
    pub async fn generate_music(&self, req: MusicRequest) -> Result<AudioOut> {
        let url = format!("{}/v1beta/models/{}:generateContent", self.base, req.model);
        let mut parts: Vec<serde_json::Value> =
            vec![serde_json::json!({"text": req.prompt})];
        for img in &req.input_images {
            let data = base64::engine::general_purpose::STANDARD.encode(&img.bytes);
            parts.push(serde_json::json!({
                "inlineData": {
                    "mimeType": img.mime,
                    "data": data
                }
            }));
        }
        let mut gen_cfg = serde_json::json!({"responseModalities": ["AUDIO"]});
        if let Some(fmt) = &req.response_format {
            gen_cfg["responseFormat"] = serde_json::Value::String(fmt.clone());
        }
        let body = serde_json::json!({
            "contents": [{"role": "user", "parts": parts}],
            "generationConfig": gen_cfg
        });

        let resp = self
            .http
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("music request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if let Ok(env) = serde_json::from_str::<ApiErrorEnvelope>(&text) {
                bail!("music {}: {}", status, env.error.message);
            }
            bail!("music {}: {}", status, text);
        }
        let parsed: GenContentResponse = resp.json().await.context("parsing music response")?;
        for c in parsed.candidates {
            let Some(content) = c.content else { continue };
            for part in content.parts {
                if let Part::InlineData { inline_data } = part {
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(inline_data.data.as_bytes())
                        .context("decoding music base64")?;
                    let sample_rate = parse_sample_rate(&inline_data.mime_type);
                    return Ok(AudioOut {
                        mime: inline_data.mime_type,
                        bytes,
                        sample_rate,
                    });
                }
            }
        }
        bail!("music returned no audio");
    }
}

fn parse_sample_rate(mime: &str) -> Option<u32> {
    for part in mime.split(';') {
        let kv = part.trim();
        if let Some(rest) = kv.strip_prefix("rate=") {
            return rest.parse().ok();
        }
    }
    None
}

/// Natural file extension for an audio MIME type.
pub fn extension_for_mime(mime: &str) -> &'static str {
    if mime.starts_with("audio/L16") || mime.starts_with("audio/pcm") {
        return "wav";
    }
    match mime {
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "audio/aac" => "aac",
        "audio/opus" => "opus",
        _ => "bin",
    }
}

/// Wrap raw 16-bit little-endian PCM into a WAV file blob.
pub fn pcm16_to_wav(pcm: &[u8], sample_rate: u32, channels: u16) -> Vec<u8> {
    let byte_rate = sample_rate * (channels as u32) * 2;
    let block_align: u16 = channels * 2;
    let data_len = pcm.len() as u32;
    let riff_len = 36 + data_len;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sample_rate() {
        assert_eq!(parse_sample_rate("audio/L16;rate=24000"), Some(24000));
        assert_eq!(parse_sample_rate("audio/L16; rate=16000"), Some(16000));
        assert_eq!(parse_sample_rate("audio/wav"), None);
    }

    #[test]
    fn wav_header_shape() {
        let pcm = vec![0u8; 100];
        let wav = pcm16_to_wav(&pcm, 24000, 1);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(wav.len(), 44 + 100);
    }
}
