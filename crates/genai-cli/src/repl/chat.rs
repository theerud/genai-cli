//! Chat-turn execution: streaming for plain chat, the tool loop when any
//! local tool is active. Persistence and in-memory history updates happen
//! here so the dispatch layer stays a thin match.

use anyhow::Result;
use futures_util::StreamExt;
use std::io::{self, IsTerminal};
use tokio::signal;

use crate::gemini::chat::{ChatEvent, ChatRequest};
use crate::gemini::types::{Content, FinishReason, Part};
use crate::tools;

use super::ReplState;
use super::render::{self, Renderer};

pub(super) async fn chat_turn(state: &mut ReplState, user_text: String) -> Result<()> {
    let files = state.pending_files.clone();
    let (user_msg, attachments) = crate::session::build_user_message(user_text, &files)?;

    let mut contents = state.history.clone();
    contents.push(user_msg.clone());

    if tools::has_local(&state.active_tools) {
        return chat_turn_with_tools(state, contents, user_msg, attachments).await;
    }

    let gen_cfg = state.build_generation_config();
    let tools_list = tools::build_request_tools(&state.active_tools);
    let req = ChatRequest {
        model: &state.model.id,
        contents: &contents,
        system_instruction: state.system_prompt.as_deref(),
        generation_config: gen_cfg.as_ref(),
        tools: tools_list.as_deref(),
    };

    let stream = state.client.stream_chat(req).await?;

    let stdout = io::stdout();
    let tty = stdout.is_terminal();
    let style = render::pick_style(tty, state.cfg.repl.color, state.cfg.repl.markdown);
    let mut renderer = render::make_boxed(stdout, tty, style);

    let mut accumulated = String::new();
    let mut last_usage = (None, None);
    let outcome =
        consume_stream(stream, renderer.as_mut(), &mut accumulated, &mut last_usage).await;
    renderer.finish();

    match outcome {
        Ok(()) => {
            let assistant = Content {
                role: Some("model".to_string()),
                parts: vec![Part::Text { text: accumulated }],
            };
            if let Some(s) = &state.session {
                let session_id = s.id();
                if let Err(e) = persist_turn(
                    &mut state.db,
                    session_id,
                    &user_msg,
                    &assistant,
                    &state.model.id,
                    &attachments,
                ) {
                    eprintln!("warning: failed to persist turn (in-memory history kept): {e}");
                }
            }
            state.history.push(user_msg);
            state.history.push(assistant);
            state.pending_files.clear();
            state
                .usage
                .accumulate(last_usage.0, last_usage.1, &state.registry, &state.model.id);
            Ok(())
        }
        Err(StreamErr::Cancelled) => {
            eprintln!("(cancelled — turn discarded)");
            Ok(())
        }
        Err(StreamErr::Failed(e)) => Err(e),
    }
}

fn persist_turn(
    db: &mut crate::session::db::Database,
    session_id: i64,
    user: &Content,
    assistant: &Content,
    model_id: &str,
    attachments: &[crate::session::attachment::Attachment],
) -> Result<()> {
    let hashes = crate::session::persist_attachments(db, attachments)?;
    db.commit_turn(session_id, user, assistant, Some(model_id), &hashes)
}

fn persist_exchange(
    db: &mut crate::session::db::Database,
    session_id: i64,
    user: &Content,
    chain: &[Content],
    model_id: &str,
    attachments: &[crate::session::attachment::Attachment],
) -> Result<()> {
    let hashes = crate::session::persist_attachments(db, attachments)?;
    db.commit_exchange(session_id, user, chain, Some(model_id), &hashes)
}

async fn chat_turn_with_tools(
    state: &mut ReplState,
    contents: Vec<Content>,
    user_msg: Content,
    attachments: Vec<crate::session::attachment::Attachment>,
) -> Result<()> {
    let req = tools::runner::ToolLoopRequest {
        model: state.model.id.clone(),
        contents,
        system_instruction: state.system_prompt.clone(),
        generation_config: state.build_generation_config(),
        enabled_tools: state.active_tools.clone(),
    };
    let mut ui = tools::cli_ui::CliToolUi;
    let outcome = match tools::runner::run(&state.client, req, &mut ui).await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("(tool loop failed — turn discarded: {e})");
            return Ok(());
        }
    };

    // Render the final assistant text now that the loop has settled.
    let stdout = io::stdout();
    let tty = stdout.is_terminal();
    let style = render::pick_style(tty, state.cfg.repl.color, state.cfg.repl.markdown);
    let mut renderer = render::make_boxed(stdout, tty, style);
    renderer.push(&outcome.final_text);
    renderer.finish();

    if let Some(s) = &state.session {
        let session_id = s.id();
        if let Err(e) = persist_exchange(
            &mut state.db,
            session_id,
            &user_msg,
            &outcome.exchange,
            &state.model.id,
            &attachments,
        ) {
            eprintln!("warning: failed to persist turn (in-memory history kept): {e}");
        }
    }
    state.history.push(user_msg);
    state.history.extend(outcome.exchange);
    state.pending_files.clear();
    state.usage.accumulate(
        outcome.prompt_tokens,
        outcome.output_tokens,
        &state.registry,
        &state.model.id,
    );
    Ok(())
}

enum StreamErr {
    Cancelled,
    Failed(anyhow::Error),
}

async fn consume_stream(
    mut stream: crate::gemini::chat::ChatStream,
    renderer: &mut dyn Renderer,
    accumulated: &mut String,
    last_usage: &mut (Option<u32>, Option<u32>),
) -> std::result::Result<(), StreamErr> {
    loop {
        tokio::select! {
            biased;
            _ = signal::ctrl_c() => return Err(StreamErr::Cancelled),
            ev = stream.next() => match ev {
                None => return Ok(()),
                Some(Err(e)) => return Err(StreamErr::Failed(e)),
                Some(Ok(ChatEvent::TextDelta(text))) => {
                    accumulated.push_str(&text);
                    renderer.push(&text);
                }
                Some(Ok(ChatEvent::Finish { prompt_tokens, output_tokens, reason, message })) => {
                    *last_usage = (prompt_tokens, output_tokens);
                    warn_abnormal_finish(accumulated, reason.as_ref(), message.as_deref());
                    return Ok(());
                }
            }
        }
    }
}

/// Emit a stderr diagnostic when the model finished for a reason other than
/// `STOP`. Silent abnormal completions (e.g. `MALFORMED_FUNCTION_CALL`) leave
/// the user staring at an empty prompt with no idea what went wrong.
fn warn_abnormal_finish(text: &str, reason: Option<&FinishReason>, message: Option<&str>) {
    let Some(r) = reason else {
        return;
    };
    if r.is_normal() {
        return;
    }
    if text.is_empty() {
        eprintln!("(no response — finish_reason={r})");
    } else {
        eprintln!("(finish_reason={r})");
    }
    if let Some(m) = message
        && !m.is_empty()
    {
        eprintln!("  {m}");
    }
}
