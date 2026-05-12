use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::{Deserialize, Serialize};

use super::Client;
use super::types::{ApiErrorEnvelope, Content, Part};

pub struct ImageRequest {
    pub model: String,
    pub prompt: String,
    pub input_images: Vec<InputImage>,
    pub aspect_ratio: Option<String>,
    pub count: Option<u32>,
}

pub struct InputImage {
    pub mime: String,
    pub bytes: Vec<u8>,
}

pub struct ImageOut {
    pub mime: String,
    pub bytes: Vec<u8>,
}

#[derive(Serialize)]
struct ImagenInstance {
    prompt: String,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct ImagenParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    sample_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aspect_ratio: Option<String>,
}

#[derive(Serialize)]
struct ImagenRequest {
    instances: Vec<ImagenInstance>,
    parameters: ImagenParams,
}

#[derive(Deserialize)]
struct ImagenResponse {
    #[serde(default)]
    predictions: Vec<ImagenPrediction>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImagenPrediction {
    #[serde(default)]
    bytes_base64_encoded: Option<String>,
    #[serde(default)]
    mime_type: Option<String>,
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
    pub async fn generate_image(&self, req: ImageRequest) -> Result<Vec<ImageOut>> {
        if req.model.starts_with("imagen") {
            self.imagen_predict(req).await
        } else {
            self.gemini_image_gen(req).await
        }
    }

    async fn imagen_predict(&self, req: ImageRequest) -> Result<Vec<ImageOut>> {
        if !req.input_images.is_empty() {
            bail!("Imagen models do not accept input images; use a gemini-image model");
        }
        let url = format!("{}/v1beta/models/{}:predict", self.base, req.model);
        let body = ImagenRequest {
            instances: vec![ImagenInstance {
                prompt: req.prompt,
            }],
            parameters: ImagenParams {
                sample_count: req.count,
                aspect_ratio: req.aspect_ratio,
            },
        };
        let resp = self
            .http
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("imagen predict request")?;
        if !resp.status().is_success() {
            return Err(api_err("imagen predict", resp).await);
        }
        let parsed: ImagenResponse = resp.json().await.context("parsing imagen response")?;
        let mut out = Vec::with_capacity(parsed.predictions.len());
        for p in parsed.predictions {
            let Some(b64) = p.bytes_base64_encoded else {
                continue;
            };
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64.as_bytes())
                .context("decoding imagen base64")?;
            out.push(ImageOut {
                mime: p.mime_type.unwrap_or_else(|| "image/png".to_string()),
                bytes,
            });
        }
        if out.is_empty() {
            bail!("imagen returned no images");
        }
        Ok(out)
    }

    async fn gemini_image_gen(&self, req: ImageRequest) -> Result<Vec<ImageOut>> {
        let url = format!("{}/v1beta/models/{}:generateContent", self.base, req.model);

        let mut parts: Vec<Part> = Vec::new();
        parts.push(Part::Text { text: req.prompt });
        for img in req.input_images {
            let data = base64::engine::general_purpose::STANDARD.encode(&img.bytes);
            parts.push(Part::InlineData {
                inline_data: super::types::InlineData {
                    mime_type: img.mime,
                    data,
                },
            });
        }

        let body = serde_json::json!({
            "contents": [{"role": "user", "parts": serde_json::to_value(&parts)?}],
            "generationConfig": {
                "responseModalities": ["TEXT", "IMAGE"]
            }
        });
        let resp = self
            .http
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("gemini image generation request")?;
        if !resp.status().is_success() {
            return Err(api_err("gemini image", resp).await);
        }
        let parsed: GenContentResponse = resp.json().await.context("parsing gemini image response")?;
        let mut out = Vec::new();
        for c in parsed.candidates {
            let Some(content) = c.content else { continue };
            for part in content.parts {
                if let Part::InlineData { inline_data } = part {
                    if inline_data.mime_type.starts_with("image/") {
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(inline_data.data.as_bytes())
                            .context("decoding gemini image base64")?;
                        out.push(ImageOut {
                            mime: inline_data.mime_type,
                            bytes,
                        });
                    }
                }
            }
        }
        if out.is_empty() {
            bail!("gemini image generation returned no image parts");
        }
        Ok(out)
    }
}

async fn api_err(label: &str, resp: reqwest::Response) -> anyhow::Error {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if let Ok(env) = serde_json::from_str::<ApiErrorEnvelope>(&text) {
        anyhow::anyhow!("{label} {}: {}", status, env.error.message)
    } else {
        anyhow::anyhow!("{label} {}: {}", status, text)
    }
}

pub fn extension_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "bin",
    }
}
