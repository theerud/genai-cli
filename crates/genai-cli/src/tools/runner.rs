use anyhow::{Result, anyhow};
use serde_json::Value;
use tracing::debug;

use crate::gemini::Client;
use crate::gemini::chat::ChatRequest;
use crate::gemini::types::{Content, FunctionResponse, GenerationConfig, Part};

use super::lookup_local;

pub struct ToolLoopRequest {
    pub model: String,
    pub contents: Vec<Content>,
    pub system_instruction: Option<String>,
    pub generation_config: Option<GenerationConfig>,
    /// Tools list to send with each `generateContent` request, pre-built
    /// by the caller from the active role/cfg context (so the schema
    /// reflects role-level media model overrides).
    pub request_tools: Option<Vec<crate::gemini::types::Tool>>,
    /// Initial iteration budget.
    pub max_iterations: u32,
    /// True when invoked from a role with `mode = "loop"`. Controls the
    /// continue-prompt behavior and the trailer added when the loop stops
    /// at the cap.
    pub loop_mode: bool,
    /// Pre-resolved media models for this turn (role overrides applied).
    /// The runner injects these as the `model` arg of matching
    /// `generate_media` calls so the tool runtime doesn't have to know
    /// about the active role.
    pub media_models: MediaModels,
}

#[derive(Debug, Clone, Default)]
pub struct MediaModels {
    pub image: Option<String>,
    pub speech: Option<String>,
    pub music: Option<String>,
}

pub struct ToolLoopOutcome {
    /// Assistant and tool-response messages produced during the loop, in order.
    /// The final entry is the assistant text message (possibly synthetic, if
    /// the loop stopped at the iteration cap).
    pub exchange: Vec<Content>,
    pub final_text: String,
    pub prompt_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    /// Iterations actually consumed.
    #[allow(dead_code)]
    pub iterations: u32,
    /// True when the loop hit the iteration cap. Reserved for callers that
    /// want to differentiate cap-stop vs natural completion.
    #[allow(dead_code)]
    pub capped: bool,
}

pub enum Confirmation {
    Allow,
    Deny,
}

/// How many additional iterations the user wants to grant when the loop
/// hits the cap. `0` (or non-interactive default) means stop.
pub trait ToolUi {
    fn announce_call(&mut self, name: &str, summary: &str);
    fn announce_result(&mut self, name: &str, ok: bool, preview: &str);
    /// Called once before a side-effecting tool runs. Returning `Deny` causes
    /// the loop to feed a synthetic error response back to the model instead
    /// of executing the tool.
    fn confirm(&mut self, name: &str, summary: &str) -> Confirmation;
    /// Called when the loop hits its iteration cap in `loop_mode`.
    /// Return the number of extra iterations to grant; `0` stops the loop.
    fn continue_loop(&mut self, used: u32, max: u32) -> u32 {
        let _ = (used, max);
        0
    }
}

pub async fn run(
    client: &Client,
    req: ToolLoopRequest,
    ui: &mut dyn ToolUi,
) -> Result<ToolLoopOutcome> {
    let tools = req.request_tools;
    let mut contents = req.contents;
    let exchange_start = contents.len();
    let mut last_prompt = None;
    let mut last_output = None;
    let mut budget: u32 = req.max_iterations.max(1);
    let mut iter: u32 = 0;

    loop {
        if iter >= budget {
            if req.loop_mode {
                let extra = ui.continue_loop(iter, budget);
                if extra == 0 {
                    return Ok(stopped_at_cap(
                        &mut contents,
                        exchange_start,
                        iter,
                        budget,
                        last_prompt,
                        last_output,
                    ));
                }
                budget = budget.saturating_add(extra);
                continue;
            }
            return Err(anyhow!(
                "tool loop exceeded {budget} iterations without a final answer"
            ));
        }
        debug!(iteration = iter, budget, "tool loop iteration");
        let chat_req = ChatRequest {
            model: &req.model,
            contents: &contents,
            system_instruction: req.system_instruction.as_deref(),
            generation_config: req.generation_config.as_ref(),
            tools: tools.as_deref(),
        };
        let resp = {
            let label = format!("[{}/{budget}] thinking...", iter + 1);
            let _s = crate::spinner::Spinner::start(&label);
            client.generate_content(chat_req).await?
        };
        last_prompt = resp.prompt_tokens;
        last_output = resp.output_tokens;
        let model_content = resp
            .content
            .ok_or_else(|| anyhow!("model returned no content"))?;

        let calls: Vec<_> = model_content
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::FunctionCall { function_call, .. } => Some(function_call.clone()),
                _ => None,
            })
            .collect();

        if calls.is_empty() {
            let text = collect_text(&model_content);
            contents.push(model_content);
            let exchange = contents.split_off(exchange_start);
            return Ok(ToolLoopOutcome {
                exchange,
                final_text: text,
                prompt_tokens: last_prompt,
                output_tokens: last_output,
                iterations: iter + 1,
                capped: false,
            });
        }
        contents.push(model_content);

        let mut response_parts = Vec::with_capacity(calls.len());
        for mut call in calls {
            inject_media_model(&mut call, &req.media_models);
            let Some(tool) = lookup_local(&call.name) else {
                let err = serde_json::json!({"error": format!("unknown tool '{}'", call.name)});
                ui.announce_result(&call.name, false, "unknown tool");
                crate::audit::log(&call.name, &call.args, "err", "unknown tool");
                response_parts.push(Part::FunctionResponse {
                    function_response: FunctionResponse {
                        name: call.name.clone(),
                        response: err,
                    },
                });
                continue;
            };
            let summary = tool.describe_call(&call.args);
            debug!(tool = %call.name, %summary, "tool call");
            ui.announce_call(&call.name, &summary);

            // The policy decision is made against normalized args (canonical
            // paths, etc.); to keep TOCTOU as narrow as possible the tool
            // runtime and audit log also use those normalized args. A
            // symlink swap between normalize and run still loses, but the
            // policy and runtime now agree on what they're working with.
            let normalized = tool.normalize_for_policy(&call.args);
            let outcome = super::policy::evaluate(&call.name, &normalized);
            debug!(tool = %call.name, decision = ?outcome.decision, source = %outcome.source.label(), "policy");

            let response_value = match outcome.decision {
                crate::config::Decision::Deny => {
                    let msg = format!("policy denied (matched {})", outcome.source.label());
                    let v = serde_json::json!({"error": msg.clone()});
                    ui.announce_result(&call.name, false, &msg);
                    crate::audit::log(&call.name, &normalized, "denied", &msg);
                    v
                }
                crate::config::Decision::Allow => {
                    execute_and_audit(&call.name, tool, &normalized, iter + 1, budget, ui)
                }
                crate::config::Decision::Prompt => {
                    if !tool.requires_confirmation() {
                        execute_and_audit(&call.name, tool, &normalized, iter + 1, budget, ui)
                    } else {
                        match ui.confirm(&call.name, &summary) {
                            Confirmation::Allow => {
                                execute_and_audit(&call.name, tool, &normalized, iter + 1, budget, ui)
                            }
                            Confirmation::Deny => {
                                let v = serde_json::json!({
                                    "error": "user denied tool execution",
                                });
                                ui.announce_result(&call.name, false, "denied by user");
                                crate::audit::log(
                                    &call.name,
                                    &normalized,
                                    "denied",
                                    "denied by user",
                                );
                                v
                            }
                        }
                    }
                }
            };

            response_parts.push(Part::FunctionResponse {
                function_response: FunctionResponse {
                    name: call.name.clone(),
                    response: response_value,
                },
            });
        }

        contents.push(Content {
            role: Some("user".to_string()),
            parts: response_parts,
        });
        iter += 1;
    }
}

/// Build the outcome returned when a loop-mode invocation hits the cap
/// and the user chose to stop. Appends a synthetic assistant trailer
/// summarizing the cap; the caller still gets a coherent final text.
fn stopped_at_cap(
    contents: &mut Vec<Content>,
    exchange_start: usize,
    iterations: u32,
    budget: u32,
    prompt_tokens: Option<u32>,
    output_tokens: Option<u32>,
) -> ToolLoopOutcome {
    // Take the last assistant text we saw, if any, and append a trailer.
    let mut last_text = String::new();
    for c in contents.iter().rev() {
        if c.role.as_deref() == Some("model") {
            last_text = collect_text(c);
            if !last_text.is_empty() {
                break;
            }
        }
    }
    let trailer = format!("\n\n_[loop ended at {iterations}/{budget} iterations]_");
    let final_text = if last_text.is_empty() {
        format!("[loop ended at {iterations}/{budget} iterations]")
    } else {
        format!("{last_text}{trailer}")
    };
    contents.push(Content {
        role: Some("model".to_string()),
        parts: vec![Part::Text { text: final_text.clone() }],
    });
    let exchange = contents.split_off(exchange_start);
    ToolLoopOutcome {
        exchange,
        final_text,
        prompt_tokens,
        output_tokens,
        iterations,
        capped: true,
    }
}

/// Run the tool, surface the result to the UI, and append an audit-log
/// entry. Returns the value to feed back to the model. We don't reuse
/// the lower-level `execute` helper here because the spinner has to be
/// torn down *before* the result line prints — otherwise the cleared
/// spinner line and the eprintln race on the same row.
/// For `generate_media` calls without an explicit `model` arg, fill in
/// the resolved model from this turn's `MediaModels`. Without this the
/// tool runtime would fall back to the global cfg default and ignore
/// role-level overrides — the schema would be shaped for the role's
/// image model but the wrong backend would actually run.
fn inject_media_model(
    call: &mut crate::gemini::types::FunctionCall,
    models: &MediaModels,
) {
    if call.name != super::generate_media::TOOL_GENERATE_MEDIA {
        return;
    }
    if call.args.get("model").and_then(|v| v.as_str()).is_some() {
        return;
    }
    let kind = call
        .args
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let resolved = match kind {
        "image" => models.image.as_deref(),
        "speech" => models.speech.as_deref(),
        "music" => models.music.as_deref(),
        _ => None,
    };
    if let Some(m) = resolved
        && let Some(obj) = call.args.as_object_mut()
    {
        obj.insert("model".to_string(), serde_json::json!(m));
    }
}

fn execute_and_audit(
    name: &str,
    tool: &dyn super::LocalTool,
    args: &Value,
    iter: u32,
    budget: u32,
    ui: &mut dyn ToolUi,
) -> Value {
    let spinner = crate::spinner::Spinner::start(&format!("[{iter}/{budget}] running {name}..."));
    let outcome = tool.run(args);
    drop(spinner);
    match outcome {
        Ok(v) => {
            let preview = result_preview(&v);
            ui.announce_result(name, true, &preview);
            crate::audit::log(name, args, "ok", &preview);
            v
        }
        Err(e) => {
            let msg = e.to_string();
            ui.announce_result(name, false, &msg);
            crate::audit::log(name, args, "err", &msg);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gemini::types::FunctionCall;
    use serde_json::json;

    fn call(name: &str, args: serde_json::Value) -> FunctionCall {
        FunctionCall { name: name.to_string(), args }
    }

    fn models(image: Option<&str>, speech: Option<&str>, music: Option<&str>) -> MediaModels {
        MediaModels {
            image: image.map(String::from),
            speech: speech.map(String::from),
            music: music.map(String::from),
        }
    }

    #[test]
    fn injects_image_model_when_kind_image() {
        let mut c = call("generate_media", json!({"kind": "image", "prompt": "x"}));
        inject_media_model(&mut c, &models(Some("imagen-4.0-generate-001"), None, None));
        assert_eq!(c.args["model"], "imagen-4.0-generate-001");
    }

    #[test]
    fn does_not_overwrite_explicit_model() {
        let mut c = call(
            "generate_media",
            json!({"kind": "image", "prompt": "x", "model": "user-pinned"}),
        );
        inject_media_model(&mut c, &models(Some("ignored"), None, None));
        assert_eq!(c.args["model"], "user-pinned");
    }

    #[test]
    fn injects_only_for_generate_media() {
        let mut c = call("write_file", json!({"path": "/tmp/x", "content": "y"}));
        inject_media_model(&mut c, &models(Some("imagen-4.0-generate-001"), None, None));
        assert!(c.args.get("model").is_none());
    }

    #[test]
    fn injects_per_kind() {
        let mut c = call("generate_media", json!({"kind": "speech", "prompt": "hi"}));
        inject_media_model(&mut c, &models(Some("i"), Some("gemini-2.5-flash-preview-tts"), Some("m")));
        assert_eq!(c.args["model"], "gemini-2.5-flash-preview-tts");
    }
}
