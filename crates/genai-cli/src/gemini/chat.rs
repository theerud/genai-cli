use anyhow::{Context, Result, anyhow, bail};
use futures_util::{Stream, StreamExt};
use std::pin::Pin;
use tracing::{debug, trace};

use super::Client;
use super::types::{
    ApiErrorEnvelope, Content, GenerateContentRequest, GenerateContentResponse, GenerationConfig,
    Part, Tool,
};

pub struct ChatRequest {
    pub model: String,
    pub contents: Vec<Content>,
    pub system_instruction: Option<String>,
    pub generation_config: Option<GenerationConfig>,
    pub tools: Option<Vec<Tool>>,
}

pub enum ChatEvent {
    TextDelta(String),
    Finish {
        prompt_tokens: Option<u32>,
        output_tokens: Option<u32>,
        /// The server-reported reason. `"STOP"` means a normal completion; any
        /// other value (`MALFORMED_FUNCTION_CALL`, `SAFETY`, `RECITATION`, …)
        /// is worth surfacing to the user, especially when no text was emitted.
        reason: Option<String>,
        /// Free-form server message attached to abnormal finishes (e.g. the
        /// rejected function-call body for `MALFORMED_FUNCTION_CALL`).
        message: Option<String>,
    },
}

pub type ChatStream = Pin<Box<dyn Stream<Item = Result<ChatEvent>> + Send>>;

pub struct ChatResponse {
    pub content: Option<Content>,
    pub prompt_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

impl Client {
    pub async fn generate_content(&self, req: ChatRequest) -> Result<ChatResponse> {
        let url = format!(
            "{}/v1beta/models/{}:generateContent",
            self.base, req.model
        );
        debug!(model = %req.model, msgs = req.contents.len(), "generate_content");
        let body = GenerateContentRequest {
            contents: req.contents,
            system_instruction: req.system_instruction.map(|t| Content {
                role: None,
                parts: vec![Part::Text { text: t }],
            }),
            generation_config: req.generation_config,
            tools: req.tools,
        };
        let resp = self
            .http
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .context("sending generateContent request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if let Ok(env) = serde_json::from_str::<ApiErrorEnvelope>(&text) {
                bail!("API {}: {}", status, env.error.message);
            }
            bail!("API {}: {}", status, text);
        }
        let parsed: GenerateContentResponse = resp.json().await.context("parsing response")?;
        let content = parsed.candidates.into_iter().next().and_then(|c| c.content);
        let (prompt_tokens, output_tokens) = match parsed.usage_metadata {
            Some(u) => (u.prompt_token_count, u.candidates_token_count),
            None => (None, None),
        };
        Ok(ChatResponse {
            content,
            prompt_tokens,
            output_tokens,
        })
    }

    pub async fn stream_chat(&self, req: ChatRequest) -> Result<ChatStream> {
        let url = format!(
            "{}/v1beta/models/{}:streamGenerateContent?alt=sse",
            self.base, req.model
        );
        debug!(model = %req.model, msgs = req.contents.len(), "stream_chat");

        let body = GenerateContentRequest {
            contents: req.contents,
            system_instruction: req.system_instruction.map(|t| Content {
                role: None,
                parts: vec![Part::Text { text: t }],
            }),
            generation_config: req.generation_config,
            tools: req.tools,
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
    trace!(bytes = data.len(), "sse event");
    let resp: GenerateContentResponse =
        serde_json::from_str(data).with_context(|| format!("parsing SSE data: {data}"))?;

    let mut text = String::new();
    let mut reason: Option<String> = None;
    let mut message: Option<String> = None;
    for c in &resp.candidates {
        if let Some(content) = &c.content {
            for part in &content.parts {
                if let Part::Text { text: t } = part {
                    text.push_str(t);
                }
            }
        }
        if c.finish_reason.is_some() {
            reason.clone_from(&c.finish_reason);
            message.clone_from(&c.finish_message);
        }
    }

    let mut out = Vec::new();
    if !text.is_empty() {
        out.push(ChatEvent::TextDelta(text));
    }
    let usage = resp.usage_metadata;
    if reason.is_some() || usage.is_some() {
        let (prompt_tokens, output_tokens) = match &usage {
            Some(u) => (u.prompt_token_count, u.candidates_token_count),
            None => (None, None),
        };
        out.push(ChatEvent::Finish {
            prompt_tokens,
            output_tokens,
            reason,
            message,
        });
    }
    Ok(out)
}

/// Parse an SSE byte stream into individual `data:` payloads (one per event).
///
/// Accumulates raw bytes rather than `String`s: HTTP chunks can split a
/// multi-byte UTF-8 codepoint, and an earlier version that did
/// `from_utf8_lossy` per chunk would insert U+FFFD at the boundary.
/// Event terminators (`\n\n` / `\r\n\r\n`) are pure ASCII, so we can search
/// for them in the byte buffer and only decode complete events to UTF-8.
fn sse_events<S>(stream: S) -> impl Stream<Item = Result<String>>
where
    S: Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    async_stream::stream! {
        let mut buf: Vec<u8> = Vec::new();
        let mut stream = Box::pin(stream);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| anyhow!("http stream: {e}"))?;
            buf.extend_from_slice(&chunk);
            while let Some(idx) = find_event_terminator_bytes(&buf) {
                let event_bytes = buf.drain(..idx.end).collect::<Vec<u8>>();
                let event_str = String::from_utf8_lossy(&event_bytes[..idx.start]);
                if let Some(d) = extract_data(&event_str) {
                    yield Ok(d);
                }
            }
        }
        if !buf.is_empty() {
            let tail = String::from_utf8_lossy(&buf);
            if let Some(d) = extract_data(&tail) {
                yield Ok(d);
            }
        }
    }
}

struct EventBounds {
    start: usize,
    end: usize,
}

fn find_event_terminator_bytes(buf: &[u8]) -> Option<EventBounds> {
    if let Some(pos) = find_subslice(buf, b"\n\n") {
        return Some(EventBounds {
            start: pos,
            end: pos + 2,
        });
    }
    if let Some(pos) = find_subslice(buf, b"\r\n\r\n") {
        return Some(EventBounds {
            start: pos,
            end: pos + 4,
        });
    }
    None
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
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

    #[tokio::test]
    async fn sse_handles_utf8_split_across_chunks() {
        // The euro sign is three bytes (0xE2 0x82 0xAC). Split it across two
        // chunks so the old per-chunk lossy decoder would insert U+FFFD.
        let body = "data: cost is €\n\n";
        let split = body.find('€').unwrap() + 1; // mid-codepoint
        let first = bytes::Bytes::copy_from_slice(&body.as_bytes()[..split]);
        let second = bytes::Bytes::copy_from_slice(&body.as_bytes()[split..]);
        let chunks: Vec<std::result::Result<bytes::Bytes, reqwest::Error>> =
            vec![Ok(first), Ok(second)];
        let stream = futures_util::stream::iter(chunks);
        let mut events = Box::pin(sse_events(stream));
        let ev = events.next().await.unwrap().unwrap();
        assert_eq!(ev, "cost is €");
    }
}
