use anyhow::{Result, anyhow};
use serde_json::Value;

use crate::gemini::Client;
use crate::gemini::chat::ChatRequest;
use crate::gemini::types::{Content, FunctionResponse, GenerationConfig, Part};

use super::{build_request_tools, lookup_local};

pub const MAX_TOOL_ITERATIONS: usize = 8;

pub struct ToolLoopRequest {
    pub model: String,
    pub contents: Vec<Content>,
    pub system_instruction: Option<String>,
    pub generation_config: Option<GenerationConfig>,
    pub enabled_tools: Vec<String>,
}

pub struct ToolLoopOutcome {
    /// Assistant and tool-response messages produced during the loop, in order.
    /// The final entry is always the assistant text message.
    pub exchange: Vec<Content>,
    pub final_text: String,
    pub prompt_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

pub enum Confirmation {
    Allow,
    Deny,
}

pub trait ToolUi {
    fn announce_call(&mut self, name: &str, summary: &str);
    fn announce_result(&mut self, name: &str, ok: bool, preview: &str);
    /// Called once before a side-effecting tool runs. Returning `Deny` causes
    /// the loop to feed a synthetic error response back to the model instead
    /// of executing the tool.
    fn confirm(&mut self, name: &str, summary: &str) -> Confirmation;
}

pub async fn run(
    client: &Client,
    req: ToolLoopRequest,
    ui: &mut dyn ToolUi,
) -> Result<ToolLoopOutcome> {
    let tools = build_request_tools(&req.enabled_tools);
    let mut contents = req.contents;
    let mut exchange: Vec<Content> = Vec::new();
    let mut last_prompt;
    let mut last_output;

    for _ in 0..MAX_TOOL_ITERATIONS {
        let chat_req = ChatRequest {
            model: req.model.clone(),
            contents: contents.clone(),
            system_instruction: req.system_instruction.clone(),
            generation_config: req.generation_config.clone(),
            tools: tools.clone(),
        };
        let resp = client.generate_content(chat_req).await?;
        last_prompt = resp.prompt_tokens;
        last_output = resp.output_tokens;
        let model_content = resp
            .content
            .ok_or_else(|| anyhow!("model returned no content"))?;

        let calls: Vec<_> = model_content
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::FunctionCall { function_call } => Some(function_call.clone()),
                _ => None,
            })
            .collect();

        // Persist this model turn into both history (for the next API call)
        // and the exchange (for caller persistence).
        contents.push(model_content.clone());
        exchange.push(model_content.clone());

        if calls.is_empty() {
            let text = collect_text(&model_content);
            return Ok(ToolLoopOutcome {
                exchange,
                final_text: text,
                prompt_tokens: last_prompt,
                output_tokens: last_output,
            });
        }

        // Execute each requested function call, in order, and append a single
        // user message containing all functionResponse parts. Gemini accepts
        // either one response per message or batched responses; batching keeps
        // the turn count down.
        let mut response_parts = Vec::with_capacity(calls.len());
        for call in calls {
            let Some(tool) = lookup_local(&call.name) else {
                let err = serde_json::json!({"error": format!("unknown tool '{}'", call.name)});
                ui.announce_result(&call.name, false, "unknown tool");
                response_parts.push(Part::FunctionResponse {
                    function_response: FunctionResponse {
                        name: call.name.clone(),
                        response: err,
                    },
                });
                continue;
            };
            let summary = tool.describe_call(&call.args);
            ui.announce_call(&call.name, &summary);

            let response_value = if tool.requires_confirmation() {
                match ui.confirm(&call.name, &summary) {
                    Confirmation::Allow => execute(tool, &call.args, &mut |ok, preview| {
                        ui.announce_result(&call.name, ok, preview)
                    }),
                    Confirmation::Deny => {
                        let v = serde_json::json!({
                            "error": "user denied tool execution",
                        });
                        ui.announce_result(&call.name, false, "denied by user");
                        v
                    }
                }
            } else {
                execute(tool, &call.args, &mut |ok, preview| {
                    ui.announce_result(&call.name, ok, preview)
                })
            };

            response_parts.push(Part::FunctionResponse {
                function_response: FunctionResponse {
                    name: call.name.clone(),
                    response: response_value,
                },
            });
        }

        let tool_content = Content {
            role: Some("user".to_string()),
            parts: response_parts,
        };
        contents.push(tool_content.clone());
        exchange.push(tool_content);
    }

    Err(anyhow!(
        "tool loop exceeded {MAX_TOOL_ITERATIONS} iterations without a final answer"
    ))
}

fn execute(
    tool: &dyn super::LocalTool,
    args: &Value,
    report: &mut dyn FnMut(bool, &str),
) -> Value {
    match tool.run(args) {
        Ok(v) => {
            let preview = result_preview(&v);
            report(true, &preview);
            v
        }
        Err(e) => {
            let msg = e.to_string();
            report(false, &msg);
            serde_json::json!({"error": msg})
        }
    }
}

fn collect_text(c: &Content) -> String {
    let mut s = String::new();
    for p in &c.parts {
        if let Part::Text { text } = p {
            s.push_str(text);
        }
    }
    s
}

fn result_preview(v: &Value) -> String {
    // Short, informative preview rather than full JSON.
    match v {
        Value::Object(map) => {
            let mut bits = Vec::new();
            if let Some(b) = map.get("bytes").and_then(Value::as_u64) {
                bits.push(format!("{b} B"));
            }
            if let Some(s) = map.get("status").and_then(Value::as_u64) {
                bits.push(format!("HTTP {s}"));
            }
            if let Some(c) = map.get("exit_code").and_then(Value::as_i64) {
                bits.push(format!("exit={c}"));
            }
            if let Some(entries) = map.get("entries").and_then(Value::as_array) {
                bits.push(format!("{} entries", entries.len()));
            }
            if let Some(t) = map.get("truncated").and_then(Value::as_bool)
                && t
            {
                bits.push("truncated".to_string());
            }
            if bits.is_empty() {
                "ok".to_string()
            } else {
                bits.join(", ")
            }
        }
        _ => "ok".to_string(),
    }
}
