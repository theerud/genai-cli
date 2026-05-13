//! Dot-command dispatcher plus handlers that don't earn their own module:
//! `.role`, `.tools`, `.set`, `.undo`, `.retry`, `.info`, `.help`, `.edit`.

use anyhow::{Context, Result, anyhow};
use std::path::PathBuf;

use crate::gemini::types::Part;
use crate::models::alias;
use crate::output;
use crate::role;
use crate::tools;

use super::ReplState;
use super::chat;
use super::commands::DotCmd;
use super::media;
use super::sessions;

pub(super) async fn handle_command(state: &mut ReplState, cmd: DotCmd) -> Result<()> {
    match cmd {
        DotCmd::Help => print_help(),
        DotCmd::Exit => unreachable!("handled by caller"),
        DotCmd::Info => print_info(state),
        DotCmd::Clear => {
            state.history.clear();
            state.pending_files.clear();
            state.session = None;
            eprintln!("(history cleared, dropped session)");
        }
        DotCmd::Model(None) => {
            eprintln!("model: {}", state.model.id);
        }
        DotCmd::Model(Some(arg)) => {
            if arg == "-" {
                let default_id = state.cfg.default_chat_model().to_string();
                state.model = alias::resolve(&state.cfg, &default_id);
                eprintln!("model reset to default: {}", state.model.id);
            } else {
                let resolved = alias::resolve(&state.cfg, &arg);
                crate::models::validate(&state.registry, &resolved.id, crate::models::CAP_CHAT);
                state.model = resolved;
                eprintln!("model: {}", state.model.id);
            }
        }
        DotCmd::Set { key, value } => apply_set(state, &key, &value)?,
        DotCmd::File(paths) => {
            for p in paths {
                let pb = PathBuf::from(output::expand_path(&p));
                if !pb.exists() {
                    eprintln!("warning: {} does not exist", pb.display());
                    continue;
                }
                state.pending_files.push(pb);
            }
            eprintln!("{} file(s) pending", state.pending_files.len());
        }
        DotCmd::Edit => match edit_via_editor()? {
            Some(text) if !text.trim().is_empty() => {
                chat::chat_turn(state, text).await?;
            }
            _ => eprintln!("(no input)"),
        },
        DotCmd::Session(cmd) => sessions::handle_session_cmd(state, cmd).await?,
        DotCmd::Role(arg) => handle_role_cmd(state, arg)?,
        DotCmd::Image(args) => media::handle_image_cmd(state, args).await?,
        DotCmd::Tts(args) => media::handle_tts_cmd(state, args).await?,
        DotCmd::Music(args) => media::handle_music_cmd(state, args).await?,
        DotCmd::Tools(arg) => handle_tools_cmd(state, arg)?,
        DotCmd::Preview(path) => handle_preview(state, &path)?,
        DotCmd::Undo => handle_undo(state)?,
        DotCmd::Retry => handle_retry(state).await?,
        DotCmd::Unknown(msg) => eprintln!("{msg}"),
    }
    Ok(())
}

fn handle_role_cmd(state: &mut ReplState, arg: Option<String>) -> Result<()> {
    match arg.as_deref() {
        None => match &state.role {
            Some(r) => eprintln!("role: {}", r.name),
            None => {
                let names = role::list_available()?;
                if names.is_empty() {
                    eprintln!("(no roles in {})", role::roles_dir()?.display());
                } else {
                    for n in names {
                        eprintln!("  {n}");
                    }
                }
            }
        },
        Some("list") => {
            let names = role::list_available()?;
            if names.is_empty() {
                eprintln!("(no roles defined)");
            } else {
                for n in names {
                    let marker = if state.role.as_ref().map(|r| r.name.as_str()) == Some(&n) {
                        "*"
                    } else {
                        " "
                    };
                    eprintln!("{marker} {n}");
                }
            }
        }
        Some("-") => {
            state.role = None;
            let default_id = state.cfg.default_chat_model().to_string();
            state.model = alias::resolve(&state.cfg, &default_id);
            state.system_prompt = state.cfg.model.chat.system_prompt.clone();
            eprintln!("(role cleared)");
        }
        Some(name) => apply_role(state, name)?,
    }
    Ok(())
}

fn apply_role(state: &mut ReplState, name: &str) -> Result<()> {
    let role = role::load(name)?;
    let chat_capable = role::is_chat_capable(&role, &state.registry);
    if let Some(model_id) = role.model.as_deref() {
        state.model = alias::resolve(&state.cfg, model_id);
    }
    if chat_capable {
        if let Some(sp) = &role.system_prompt {
            state.system_prompt = Some(sp.clone());
        }
        if let Some(t) = role.temperature {
            state.temperature = Some(t);
        }
        if let Some(m) = role.max_tokens {
            state.max_tokens = Some(m);
        }
        if let Some(t) = &role.thinking_level {
            state.model.thinking_level = Some(t.clone());
        }
    } else {
        let default_id = state.cfg.default_chat_model().to_string();
        state.model = alias::resolve(&state.cfg, &default_id);
        state.system_prompt = state.cfg.model.chat.system_prompt.clone();
        eprintln!(
            "Role '{}' is output-only ({}). Chat will use default model.",
            role.name,
            role.model.as_deref().unwrap_or("?")
        );
    }
    eprintln!("role: {} (model: {})", role.name, state.model.id);
    state.role = Some(role);
    Ok(())
}

fn apply_set(state: &mut ReplState, key: &str, value: &str) -> Result<()> {
    match key {
        "temperature" | "temp" => {
            state.temperature = Some(value.parse().context("temperature must be a number")?);
            eprintln!("temperature: {:?}", state.temperature);
        }
        "max-tokens" | "max_tokens" => {
            state.max_tokens = Some(value.parse().context("max-tokens must be an integer")?);
            eprintln!("max-tokens: {:?}", state.max_tokens);
        }
        "thinking" => {
            state.model.thinking_level = Some(value.to_string());
            eprintln!("thinking: {value}");
        }
        _ => eprintln!("unknown setting: {key}"),
    }
    Ok(())
}

fn print_help() {
    eprintln!(
        "
Commands:
  .help              this help
  .exit / .quit      leave
  .info              current model and stats
  .clear             clear in-memory history, drop session
  .model [id|-]      show / switch / reset chat model
  .set <key> <val>   temperature, max-tokens, thinking
  .file <path>...    queue file(s) for next message
  .edit              compose message in $EDITOR
  .session           show current session state
  .session start     start an unnamed temporary session
  .session save <name>
                     save temporary session as a named one
  .session switch <name-or-id>
                     switch to / create a named session
  .session rename <name>
                     rename current named session
  .session list      list named sessions with ids
  .session drop      leave session mode
  .session delete <name-or-id>
                     delete a named session
  .session export <name-or-id>
                     export session as JSONL to stdout
  .role [name|list|-]       switch / list / clear role
  .tools [list|name]        show / list / toggle built-in Gemini tools
  .image / .tts / .music    one-off generation in REPL
  .preview <path>    render an image inline (Kitty / iTerm2 terminals)
  .undo              drop last completed turn from history (+ session DB)
  .retry             re-run the last user message
"
    );
}

fn handle_tools_cmd(state: &mut ReplState, arg: Option<String>) -> Result<()> {
    match arg.as_deref() {
        None => {
            if state.active_tools.is_empty() {
                eprintln!("tools: <none>");
            } else {
                eprintln!("tools: {}", state.active_tools.join(", "));
            }
        }
        Some("list") => {
            eprintln!("Gemini built-ins:");
            for name in tools::builtin_names() {
                let marker = if state.active_tools.iter().any(|t| t == name) {
                    "*"
                } else {
                    " "
                };
                eprintln!("  {marker} {name}");
            }
            eprintln!("Local (client-side):");
            for name in tools::local_names() {
                let marker = if state.active_tools.iter().any(|t| t == name) {
                    "*"
                } else {
                    " "
                };
                eprintln!("  {marker} {name}");
            }
        }
        Some(name) => {
            let known = tools::parse_builtin(name).is_some() || tools::lookup_local(name).is_some();
            if !known {
                anyhow::bail!("unknown tool: {name}");
            }
            if state.active_tools.iter().any(|t| t == name) {
                state.active_tools.retain(|t| t != name);
                eprintln!("tool disabled: {name}");
            } else {
                tools::validate_enabled_tool(&state.registry, &state.model.id, name);
                state.active_tools.push(name.to_string());
                eprintln!("tool enabled: {name}");
            }
        }
    }
    Ok(())
}

fn handle_preview(state: &ReplState, path: &str) -> Result<()> {
    let expanded = output::expand_path(path);
    let bytes = std::fs::read(&expanded)
        .with_context(|| format!("reading {expanded}"))?;
    let pref =
        output::image_preview::Preference::from_config(state.cfg.output.image_preview.as_deref());
    let proto = output::image_preview::detect(pref);
    if matches!(proto, output::image_preview::Protocol::None) {
        eprintln!("(no image preview: terminal not supported or image_preview = off)");
        return Ok(());
    }
    eprintln!("{} {}", expanded, output::describe_image(&bytes));
    // Kitty's f=100 transport only accepts PNG; iTerm2's OSC 1337 accepts
    // any format. Warn the user when we know rendering will silently fail.
    if matches!(proto, output::image_preview::Protocol::Kitty)
        && !looks_like_png(&bytes)
    {
        eprintln!(
            "warning: file isn't PNG; the kitty graphics protocol may not render it. \
             Try `image_preview = \"iterm2\"` in config if your terminal supports it."
        );
    }
    output::image_preview::show(pref, &bytes)?;
    Ok(())
}

fn looks_like_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A])
}

fn handle_undo(state: &mut ReplState) -> Result<()> {
    if pop_last_turn_state(state)? == 0 {
        eprintln!("(nothing to undo)");
    } else {
        eprintln!("(undid last turn)");
    }
    Ok(())
}

async fn handle_retry(state: &mut ReplState) -> Result<()> {
    let last_user_text = state
        .history
        .iter()
        .rev()
        .find(|c| c.role.as_deref() == Some("user"))
        .and_then(|c| {
            c.parts.iter().find_map(|p| match p {
                Part::Text { text } => Some(text.clone()),
                _ => None,
            })
        });
    let Some(text) = last_user_text else {
        eprintln!("(no previous turn to retry)");
        return Ok(());
    };
    if pop_last_turn_state(state)? == 0 {
        eprintln!("(nothing to retry)");
        return Ok(());
    }
    chat::chat_turn(state, text).await
}

/// Drop the last turn from both the DB (if a session is active) and the
/// in-memory history. With a session, the DB is the source of truth for how
/// many rows the turn covers; without one, we fall back to "everything since
/// the last user message" so a function-calling exchange still collapses
/// cleanly.
fn pop_last_turn_state(state: &mut ReplState) -> Result<usize> {
    if let Some(s) = &state.session {
        let n = state.db.pop_last_turn(s.id())?;
        if n == 0 {
            return Ok(0);
        }
        let new_len = state.history.len().saturating_sub(n);
        state.history.truncate(new_len);
        return Ok(n);
    }
    let last_user = state
        .history
        .iter()
        .rposition(|c| c.role.as_deref() == Some("user"));
    let Some(idx) = last_user else {
        return Ok(0);
    };
    let removed = state.history.len() - idx;
    state.history.truncate(idx);
    Ok(removed)
}

fn print_info(state: &ReplState) {
    eprintln!("model:        {}", state.model.id);
    if let Some(r) = &state.role {
        eprintln!("role:         {}", r.name);
    }
    if let Some(s) = &state.session {
        eprintln!("session:      {}", s.label());
    }
    if !state.active_tools.is_empty() {
        eprintln!("tools:        {}", state.active_tools.join(", "));
    }
    if let Some(t) = state.temperature.or(state.model.temperature) {
        eprintln!("temperature:  {t}");
    }
    if let Some(m) = state.max_tokens {
        eprintln!("max-tokens:   {m}");
    }
    if let Some(t) = &state.model.thinking_level {
        eprintln!("thinking:     {t}");
    }
    eprintln!("history:      {} message(s)", state.history.len());
    if state.usage.turns > 0 {
        eprintln!(
            "usage:        {} turn(s), prompt={} output={} tokens",
            state.usage.turns, state.usage.prompt_tokens, state.usage.output_tokens
        );
        if state.usage.estimated_cost_usd > 0.0 {
            eprintln!("cost (est.):  ${:.4}", state.usage.estimated_cost_usd);
        }
    }
}

fn edit_via_editor() -> Result<Option<String>> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let dir = std::env::temp_dir();
    let path = dir.join(format!("genai-edit-{}.md", std::process::id()));
    std::fs::write(&path, "")?;
    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("launching {editor}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&path);
        return Err(anyhow!("editor exited with {status}"));
    }
    let text = std::fs::read_to_string(&path)?;
    let _ = std::fs::remove_file(&path);
    Ok(Some(text))
}
