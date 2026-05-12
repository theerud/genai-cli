use anyhow::{Context, Result, anyhow, bail};
use futures_util::{Stream, StreamExt};
use std::pin::Pin;

use super::Client;
use super::types::{
    ApiErrorEnvelope, Content, GenerateContentRequest, GenerateContentResponse, GenerationConfig,
    Part,
};

pub struct ChatRequest {
    pub model: String,
    pub contents: Vec<Content>,
    pub system_instruction: Option<String>,
    pub generation_config: Option<GenerationConfig>,
}

pub enum ChatEvent {
    TextDelta(String),
    Finish {
        reason: Option<String>,
        prompt_tokens: Option<u32>,
        output_tokens: Option<u32>,
        total_tokens: Option<u32>,
    },
}

pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatEvent>> + Send>>;

impl Client {
    pub async fn stream_chat(&self, req: ChatRequest) -> Result<ChatStream> {
        let url = format!(
            "{}/v1beta/models/{}:streamGenerateContent?alt=sse",
            self.base, req.model
        );

        let body = GenerateContentRequest {
            contents: req.contents,
            system_instruction: req.system_instruction.map(|t| Content {
                role: None,
                parts: vec![Part::Text { text: t }],
            }),
            generation_config: req.generation_config,
        };

        let resp = self
            .http
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("sending streamGenerateContent request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if let Ok(env) = serde_json::from_str::<ApiErrorEnvelope>(&text) {
                bail!("API {}: {}", status, env.error.message);
            }
            bail!("API {}: {}", status, text);
        }

        let byte_stream = resp.bytes_stream();
        let event_stream = sse_events(byte_stream).flat_map(|ev| {
            let events: Vec<Result<ChatEvent>> = match ev {
                Err(e) => vec![Err(e)],
                Ok(data) => match parse_sse_data(&data) {
                    Ok(v) => v.into_iter().map(Ok).collect(),
                    Err(e) => vec![Err(e)],
                },
            };
            futures_util::stream::iter(events)
        });

        Ok(Box::pin(event_stream))
    }
}

fn parse_sse_data(data: &str) -> Result<Vec<ChatEvent>> {
    let resp: GenerateContentResponse =
        serde_json::from_str(data).with_context(|| format!("parsing SSE data: {data}"))?;

    let mut text = String::new();
    let mut finish_reason = None;
    for c in &resp.candidates {
        if let Some(content) = &c.content {
            for part in &content.parts {
                if let Part::Text { text: t } = part {
                    text.push_str(t);
                }
            }
        }
        if c.finish_reason.is_some() {
            finish_reason.clone_from(&c.finish_reason);
        }
    }

    let mut out = Vec::new();
    if !text.is_empty() {
        out.push(ChatEvent::TextDelta(text));
    }
    let usage = resp.usage_metadata;
    if finish_reason.is_some() || usage.is_some() {
        let (prompt_tokens, output_tokens, total_tokens) = match &usage {
            Some(u) => (u.prompt_token_count, u.candidates_token_count, u.total_token_count),
            None => (None, None, None),
        };
        out.push(ChatEvent::Finish {
            reason: finish_reason,
            prompt_tokens,
            output_tokens,
            total_tokens,
        });
    }
    Ok(out)
}

/// Parse an SSE byte stream into individual `data:` payloads (one per event).
fn sse_events<S>(stream: S) -> impl Stream<Item = Result<String>>
where
    S: Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    async_stream::stream! {
        let mut buf = String::new();
        let mut stream = Box::pin(stream);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| anyhow!("http stream: {e}"))?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            loop {
                let Some(idx) = find_event_terminator(&buf) else { break; };
                let (event, rest) = buf.split_at(idx.end);
                let data = extract_data(&event[..idx.start]);
                let rest = rest.to_string();
                buf = rest;
                if let Some(d) = data {
                    yield Ok(d);
                }
            }
        }
        if !buf.is_empty() {
            if let Some(d) = extract_data(&buf) {
                yield Ok(d);
            }
        }
    }
}

struct EventBounds {
    start: usize,
    end: usize,
}

fn find_event_terminator(s: &str) -> Option<EventBounds> {
    if let Some(pos) = s.find("\n\n") {
        return Some(EventBounds {
            start: pos,
            end: pos + 2,
        });
    }
    if let Some(pos) = s.find("\r\n\r\n") {
        return Some(EventBounds {
            start: pos,
            end: pos + 4,
        });
    }
    None
}

fn extract_data(event: &str) -> Option<String> {
    let mut data = String::new();
    let mut has = false;
    for line in event.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest);
            has = true;
        }
    }
    if has { Some(data) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_single_data_line() {
        let ev = "data: hello\n";
        assert_eq!(extract_data(ev).as_deref(), Some("hello"));
    }

    #[test]
    fn extracts_multi_data_lines() {
        let ev = "data: line1\ndata: line2\n";
        assert_eq!(extract_data(ev).as_deref(), Some("line1\nline2"));
    }

    #[test]
    fn ignores_non_data_fields() {
        let ev = "event: ping\ndata: ok\nid: 1\n";
        assert_eq!(extract_data(ev).as_deref(), Some("ok"));
    }

    #[test]
    fn returns_none_without_data() {
        let ev = "event: keepalive\n";
        assert_eq!(extract_data(ev), None);
    }
}
