//! Handlers for `.session` subcommands plus the exit/transition prompts
//! and the history-inheritance helper used by `.session start`.

use anyhow::{Result, anyhow};
use std::io::Write;

use crate::gemini::types::{Content, Part};
use crate::models::alias;
use crate::session::{ActiveSession, db::Database, messages_to_contents};
use crate::ui;

use super::ReplState;
use super::commands::SessionCmd;

pub(super) async fn handle_session_cmd(state: &mut ReplState, cmd: SessionCmd) -> Result<()> {
    match cmd {
        SessionCmd::Show => {
            if let Some(s) = &state.session {
                if s.ephemeral() {
                    eprintln!("session: <temporary> ({} message(s))", state.history.len());
                } else {
                    eprintln!("session: {} ({} message(s))", s.label(), state.history.len());
                }
            } else {
                eprintln!("session: <none>");
            }
        }
        SessionCmd::Start => start_ephemeral_session(state)?,
        SessionCmd::Save { name } => save_current_session_as(state, &name)?,
        SessionCmd::Switch { name } => switch_named_session(state, &name).await?,
        SessionCmd::Rename { name } => rename_current_session(state, &name)?,
        SessionCmd::List => {
            let current = state.session.as_ref().and_then(|s| s.name());
            for s in state.db.list_sessions()? {
                println!("{}", crate::session::format_summary(&s, current));
            }
        }
        SessionCmd::Drop => drop_current_session(state)?,
        SessionCmd::Delete { name } => {
            if state.db.delete_session_ref(&name)? {
                eprintln!("deleted session: {name}");
            } else {
                eprintln!("no session named {name}");
            }
        }
        SessionCmd::Export { name } => export_session_inline(state, &name)?,
    }
    Ok(())
}

/// On REPL exit, ask whether to save / discard an in-flight temporary session.
/// Returns `true` to cancel exit (user chose 'cancel'), `false` to proceed.
pub(super) fn maybe_prompt_save_on_exit(state: &mut ReplState) -> Result<bool> {
    let Some(session) = &state.session else {
        return Ok(false);
    };
    if !session.ephemeral() || state.history.is_empty() {
        return Ok(false);
    }
    loop {
        eprint!("Temporary session has unsaved turns. [s]ave as / [d]iscard / [c]ancel? ");
        let _ = std::io::stderr().flush();
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        match buf.trim().to_lowercase().as_str() {
            "s" | "save" => {
                let name = ui::read_required("Session name")?;
                save_current_session_as(state, &name)?;
                return Ok(false);
            }
            "d" | "discard" => return Ok(false),
            "c" | "cancel" | "" => return Ok(true),
            _ => eprintln!("Please answer s, d, or c."),
        }
    }
}

fn start_ephemeral_session(state: &mut ReplState) -> Result<()> {
    if let Some(s) = &state.session
        && s.ephemeral()
    {
        eprintln!("(already in a temporary session)");
        return Ok(());
    }
    let session = state
        .db
        .create_ephemeral_session(Some(&state.model.id), state.system_prompt.as_deref())?;
    let active = ActiveSession { db_session: session };
    if !state.history.is_empty() {
        let pair_count = state.history.len() / 2;
        if pair_count > 0
            && ui::confirm(
                &format!("Include {} previous turn(s) in this temporary session?", pair_count),
                true,
            )?
        {
            inherit_history_into_session(&mut state.db, active.id(), &state.history, &state.model.id)?;
        }
    }
    let msgs = state.db.load_messages(active.id())?;
    state.history = messages_to_contents(&msgs);
    state.session = Some(active);
    eprintln!("temporary session started ({} message(s))", state.history.len());
    Ok(())
}

fn save_current_session_as(state: &mut ReplState, name: &str) -> Result<()> {
    let Some(session) = &mut state.session else {
        anyhow::bail!("no active session to save");
    };
    if !session.ephemeral() {
        anyhow::bail!("current session is already named; use .session rename <name>");
    }
    if state.db.get_session(name)?.is_some() {
        anyhow::bail!("session {name} already exists");
    }
    state.db.promote_to_named(session.id(), name)?;
    session.db_session.name = name.to_string();
    session.db_session.ephemeral = false;
    eprintln!("saved temporary session as: {name}");
    Ok(())
}

async fn switch_named_session(state: &mut ReplState, name: &str) -> Result<()> {
    maybe_resolve_ephemeral_before_transition(state)?;
    let session = state
        .db
        .get_or_create_session(name, Some(&state.model.id), state.system_prompt.as_deref())?;
    let msgs = state.db.load_messages(session.id)?;
    state.history = messages_to_contents(&msgs);
    if let Some(model) = &session.model {
        state.model = alias::resolve(&state.cfg, model);
    }
    if let Some(sp) = &session.system_prompt {
        state.system_prompt = Some(sp.clone());
    }
    state.session = Some(ActiveSession { db_session: session });
    eprintln!(
        "session: {} ({} message(s))",
        state.session.as_ref().unwrap().label(),
        state.history.len()
    );
    Ok(())
}

fn rename_current_session(state: &mut ReplState, name: &str) -> Result<()> {
    let Some(session) = &mut state.session else {
        anyhow::bail!("no active session to rename");
    };
    if session.ephemeral() {
        anyhow::bail!("temporary session has no name; use .session save <name>");
    }
    if state.db.get_session(name)?.is_some() {
        anyhow::bail!("session {name} already exists");
    }
    state.db.rename_session(session.id(), name)?;
    session.db_session.name = name.to_string();
    eprintln!("renamed session to: {name}");
    Ok(())
}

fn drop_current_session(state: &mut ReplState) -> Result<()> {
    maybe_resolve_ephemeral_before_transition(state)?;
    state.session = None;
    state.history.clear();
    eprintln!("(dropped session)");
    Ok(())
}

fn export_session_inline(state: &mut ReplState, name: &str) -> Result<()> {
    let s = state
        .db
        .get_session(name)?
        .ok_or_else(|| anyhow!("no session named {name}"))?;
    crate::session::export_jsonl(&state.db, &s, std::io::stdout().lock())
}

fn maybe_resolve_ephemeral_before_transition(state: &mut ReplState) -> Result<()> {
    let Some(session) = &state.session else {
        return Ok(());
    };
    if !session.ephemeral() {
        return Ok(());
    }
    if state.history.is_empty() {
        let tmp = state.session.take().unwrap();
        state.db.delete_session_ref(&tmp.id().to_string())?;
        return Ok(());
    }
    loop {
        eprint!("Temporary session has unsaved turns. [s]ave as / [d]iscard / [c]ancel? ");
        let _ = std::io::stderr().flush();
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        match buf.trim().to_lowercase().as_str() {
            "s" | "save" => {
                let name = ui::read_required("Session name")?;
                save_current_session_as(state, &name)?;
                return Ok(());
            }
            "d" | "discard" => {
                let tmp = state.session.take().unwrap();
                state.db.delete_session_ref(&tmp.id().to_string())?;
                return Ok(());
            }
            "c" | "cancel" | "" => anyhow::bail!("cancelled"),
            _ => eprintln!("Please answer s, d, or c."),
        }
    }
}

/// Commit history rows into a new session, preserving turn boundaries.
/// A turn starts at each fresh user-text message and runs until (exclusive)
/// the next one — so a function-calling exchange (user text → model call →
/// user functionResponse → … → model text) gets committed as a single turn
/// instead of being split apart.
fn inherit_history_into_session(
    db: &mut Database,
    session_id: i64,
    history: &[Content],
    model_id: &str,
) -> Result<()> {
    let mut start: Option<usize> = None;
    let commit = |db: &mut Database, slice: &[Content]| -> Result<()> {
        if slice.len() < 2 {
            return Ok(()); // half-finished turn; skip
        }
        db.commit_exchange(session_id, &slice[0], &slice[1..], Some(model_id), &[])
    };
    for (i, c) in history.iter().enumerate() {
        if is_fresh_user_message(c)
            && let Some(s) = start.replace(i)
        {
            commit(db, &history[s..i])?;
        }
    }
    if let Some(s) = start {
        commit(db, &history[s..])?;
    }
    Ok(())
}

fn is_fresh_user_message(c: &Content) -> bool {
    if c.role.as_deref() != Some("user") {
        return false;
    }
    let mut has_text = false;
    for p in &c.parts {
        match p {
            Part::Text { .. } => has_text = true,
            Part::FunctionResponse { .. } => return false,
            _ => {}
        }
    }
    has_text
}
