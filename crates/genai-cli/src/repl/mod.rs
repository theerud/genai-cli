pub mod commands;
pub mod prompt;
pub mod render;

use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt;
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use tokio::signal;

use crate::config::Config;
use crate::gemini::Client;
use crate::gemini::chat::{ChatEvent, ChatRequest};
use crate::gemini::image::{ImageRequest, InputImage};
use crate::gemini::tts::{AudioOut, MusicRequest, TtsRequest, pcm16_to_wav};
use crate::gemini::types::{Content, GenerationConfig, Part};
use crate::models::Registry;
use crate::models::alias::{self, ResolvedModel};
use crate::role::{self, Role};
use crate::session::{ActiveSession, db::Database, messages_to_contents};
use commands::{ActionArgs, DotCmd, SessionCmd, parse as parse_cmd};
use prompt::PromptState;
use render::Renderer;

pub struct ReplState {
    pub cfg: Config,
    pub client: Client,
    pub registry: Registry,
    pub db: Database,
    pub model: ResolvedModel,
    pub system_prompt: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub history: Vec<Content>,
    pub pending_files: Vec<PathBuf>,
    pub session: Option<ActiveSession>,
    pub role: Option<Role>,
    pub usage: UsageStats,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UsageStats {
    pub turns: u64,
    pub prompt_tokens: u64,
    pub output_tokens: u64,
    pub estimated_cost_usd: f64,
}

impl UsageStats {
    pub fn accumulate(
        &mut self,
        prompt: Option<u32>,
        output: Option<u32>,
        registry: &Registry,
        model_id: &str,
    ) {
        self.turns += 1;
        let p = prompt.unwrap_or(0) as u64;
        let o = output.unwrap_or(0) as u64;
        self.prompt_tokens += p;
        self.output_tokens += o;
        if let Some(m) = registry.get(model_id) {
            self.estimated_cost_usd += (p as f64) / 1_000_000.0 * m.input_price_per_1m;
            self.estimated_cost_usd += (o as f64) / 1_000_000.0 * m.output_price_per_1m;
        }
    }
}

impl ReplState {
    pub fn new(
        cfg: Config,
        client: Client,
        registry: Registry,
        db: Database,
        session: Option<ActiveSession>,
        history: Vec<Content>,
        role: Option<Role>,
    ) -> Self {
        let default_id = cfg.default_chat_model().to_string();
        let model_id = role
            .as_ref()
            .and_then(|r| r.model.clone())
            .or_else(|| session.as_ref().and_then(|s| s.db_session.model.clone()))
            .unwrap_or(default_id);
        let model = alias::resolve(&cfg, &model_id);
        let role_chat_capable = role
            .as_ref()
            .map(|r| role::is_chat_capable(r, &registry))
            .unwrap_or(true);
        let role_sysprompt = role
            .as_ref()
            .filter(|_| role_chat_capable)
            .and_then(|r| r.system_prompt.clone());
        let system_prompt = role_sysprompt
            .or_else(|| session.as_ref().and_then(|s| s.db_session.system_prompt.clone()))
            .or_else(|| cfg.model.chat.system_prompt.clone());
        let temperature = role
            .as_ref()
            .and_then(|r| r.temperature)
            .or(cfg.model.chat.temperature);
        let max_tokens = role
            .as_ref()
            .and_then(|r| r.max_tokens)
            .or(cfg.model.chat.max_tokens);
        Self {
            cfg,
            client,
            registry,
            db,
            model,
            system_prompt,
            temperature,
            max_tokens,
            history,
            pending_files: Vec::new(),
            session,
            role,
            usage: UsageStats::default(),
        }
    }

    pub fn prompt(&self) -> String {
        PromptState {
            role: self.role.as_ref().map(|r| r.name.as_str()),
            session: self.session.as_ref().map(|s| s.label()),
        }
        .render()
    }

    pub fn build_generation_config(&self) -> Option<GenerationConfig> {
        let temperature = self.temperature.or(self.model.temperature);
        let max_output_tokens = self.max_tokens;
        let thinking_config = self
            .model
            .thinking_level
            .as_deref()
            .and_then(alias::thinking_for);
        if temperature.is_none() && max_output_tokens.is_none() && thinking_config.is_none() {
            return None;
        }
        Some(GenerationConfig {
            temperature,
            max_output_tokens,
            thinking_config,
            ..Default::default()
        })
    }
}

pub async fn run(mut state: ReplState) -> Result<()> {
    let mut rl = DefaultEditor::new()?;
    let history_path = history_file_path()?;
    let _ = rl.load_history(&history_path);

    print_banner(&state);

    loop {
        let prompt_str = state.prompt();
        let line = tokio::task::block_in_place(|| rl.readline(&prompt_str));
        match line {
            Ok(line) => {
                let _ = rl.add_history_entry(line.as_str());
                if let Some(cmd) = parse_cmd(&line) {
                    if matches!(cmd, DotCmd::Exit) {
                        break;
                    }
                    if let Err(e) = handle_command(&mut state, cmd).await {
                        eprintln!("error: {e}");
                    }
                    continue;
                }
                if line.trim().is_empty() {
                    continue;
                }
                if let Err(e) = chat_turn(&mut state, line).await {
                    eprintln!("error: {e}");
                }
            }
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => {
                if maybe_prompt_save_on_exit(&mut state)? {
                    continue;
                }
                break;
            },
            Err(e) => {
                eprintln!("readline: {e}");
                break;
            }
        }
    }

    let _ = rl.save_history(&history_path);
    Ok(())
}

fn print_banner(state: &ReplState) {
    let session = state
        .session
        .as_ref()
        .map(|s| format!(", session: {}", s.label()))
        .unwrap_or_default();
    eprintln!("genai-cli — model: {}{}", state.model.id, session);
    eprintln!("Type .help for commands, .exit or Ctrl-D to quit.");
}

fn history_file_path() -> Result<PathBuf> {
    let paths = crate::config::paths()?;
    std::fs::create_dir_all(&paths.cache_dir)
        .with_context(|| format!("creating {}", paths.cache_dir.display()))?;
    Ok(paths.cache_dir.join("history"))
}

async fn handle_command(state: &mut ReplState, cmd: DotCmd) -> Result<()> {
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
                let pb = PathBuf::from(shellexpand(&p));
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
                chat_turn(state, text).await?;
            }
            _ => eprintln!("(no input)"),
        },
        DotCmd::Session(cmd) => handle_session_cmd(state, cmd).await?,
        DotCmd::Role(arg) => handle_role_cmd(state, arg)?,
        DotCmd::Image(args) => handle_image_cmd(state, args).await?,
        DotCmd::Tts(args) => handle_tts_cmd(state, args).await?,
        DotCmd::Music(args) => handle_music_cmd(state, args).await?,
        DotCmd::Undo => handle_undo(state)?,
        DotCmd::Retry => handle_retry(state).await?,
        DotCmd::Unknown(msg) => eprintln!("{msg}"),
    }
    Ok(())
}

async fn handle_session_cmd(state: &mut ReplState, cmd: SessionCmd) -> Result<()> {
    match cmd {
        SessionCmd::Show => {
            if let Some(s) = &state.session {
                if s.ephemeral {
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

fn maybe_prompt_save_on_exit(state: &mut ReplState) -> Result<bool> {
    let Some(session) = &state.session else {
        return Ok(false);
    };
    if !session.ephemeral || state.history.is_empty() {
        return Ok(false);
    }
    loop {
        eprint!("Temporary session has unsaved turns. [s]ave as / [d]iscard / [c]ancel? ");
        let _ = std::io::stderr().flush();
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf)?;
        match buf.trim().to_lowercase().as_str() {
            "s" | "save" => {
                let name = prompt_text("Session name")?;
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
    if let Some(s) = &state.session {
        if s.ephemeral {
            eprintln!("(already in a temporary session)");
            return Ok(());
        }
    }
    let temp_name = format!("__tmp_repl_{}", std::process::id());
    let session = state
        .db
        .create_session(&temp_name, Some(&state.model.id), state.system_prompt.as_deref())?;
    let active = ActiveSession {
        db_session: session,
        ephemeral: true,
    };
    if !state.history.is_empty() {
        let pair_count = state.history.len() / 2;
        if pair_count > 0
            && prompt_yes_no(
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
    if !session.ephemeral {
        anyhow::bail!("current session is already named; use .session rename <name>");
    }
    if state.db.get_session(name)?.is_some() {
        anyhow::bail!("session {name} already exists");
    }
    rename_session_row(&mut state.db, session.id(), name)?;
    session.db_session.name = name.to_string();
    session.ephemeral = false;
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
    state.session = Some(ActiveSession {
        db_session: session,
        ephemeral: false,
    });
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
    if session.ephemeral {
        anyhow::bail!("temporary session has no name; use .session save <name>");
    }
    if state.db.get_session(name)?.is_some() {
        anyhow::bail!("session {name} already exists");
    }
    rename_session_row(&mut state.db, session.id(), name)?;
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
    if !session.ephemeral {
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
                let name = prompt_text("Session name")?;
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

fn inherit_history_into_session(
    db: &mut Database,
    session_id: i64,
    history: &[Content],
    model_id: &str,
) -> Result<()> {
    let mut i = 0;
    while i + 1 < history.len() {
        let u = &history[i];
        let a = &history[i + 1];
        if u.role.as_deref() == Some("user") && a.role.as_deref() == Some("model") {
            db.commit_turn(session_id, u, a, Some(model_id), &[])?;
        }
        i += 2;
    }
    Ok(())
}

fn rename_session_row(db: &mut Database, session_id: i64, name: &str) -> Result<()> {
    use rusqlite::params;
    db.conn.execute(
        "UPDATE sessions SET name = ?1 WHERE id = ?2",
        params![name, session_id],
    )?;
    Ok(())
}

fn prompt_yes_no(question: &str, default_yes: bool) -> Result<bool> {
    use std::io::Write;
    let suffix = if default_yes { "[Y/n]" } else { "[y/N]" };
    eprint!("{question} {suffix} ");
    let _ = std::io::stderr().flush();
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    let t = buf.trim().to_lowercase();
    Ok(match t.as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        _ => false,
    })
}

fn prompt_text(label: &str) -> Result<String> {
    eprint!("{label}: ");
    let _ = std::io::stderr().flush();
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    let out = buf.trim().to_string();
    if out.is_empty() {
        anyhow::bail!("name is required")
    }
    Ok(out)
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

async fn handle_image_cmd(state: &mut ReplState, args: ActionArgs) -> Result<()> {
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

    let output = match &args.output {
        Some(s) => s.clone(),
        None => prompt_user("Output path (or '-' for stdout)")?,
    };

    let inputs = load_input_images(&args.files)?;

    let req = ImageRequest {
        model: resolved.id,
        prompt: args.prompt.clone(),
        input_images: inputs,
        aspect_ratio: None,
        count: None,
    };

    let images = state.client.generate_image(req).await?;
    write_images(&output, &images)?;
    Ok(())
}

async fn handle_tts_cmd(state: &mut ReplState, args: ActionArgs) -> Result<()> {
    let model_id = args
        .model
        .clone()
        .or_else(|| state.cfg.model.tts.default.clone())
        .unwrap_or_else(|| "gemini-2.5-flash-preview-tts".to_string());
    let resolved = alias::resolve(&state.cfg, &model_id);
    crate::models::validate(&state.registry, &resolved.id, crate::models::CAP_TTS);

    let output = match &args.output {
        Some(s) => s.clone(),
        None => prompt_user("Output path (or '-' for stdout)")?,
    };
    let voice = args
        .voice
        .clone()
        .or_else(|| state.cfg.model.tts.voice.clone());

    let audio = state
        .client
        .synthesize_speech(TtsRequest {
            model: resolved.id,
            text: args.prompt,
            voice,
        })
        .await?;
    write_audio(&output, &audio)?;
    Ok(())
}

async fn handle_music_cmd(state: &mut ReplState, args: ActionArgs) -> Result<()> {
    let model_id = args
        .model
        .clone()
        .or_else(|| {
            state
                .role
                .as_ref()
                .and_then(|r| r.model.clone())
        })
        .unwrap_or_else(|| "lyria-3-pro-preview".to_string());
    let resolved = alias::resolve(&state.cfg, &model_id);
    crate::models::validate(&state.registry, &resolved.id, crate::models::CAP_MUSIC_OUT);

    let output = match &args.output {
        Some(s) => s.clone(),
        None => prompt_user("Output path (or '-' for stdout)")?,
    };

    let audio = state
        .client
        .generate_music(MusicRequest {
            model: resolved.id,
            prompt: args.prompt,
        })
        .await?;
    write_audio(&output, &audio)?;
    Ok(())
}

fn write_audio(output: &str, audio: &AudioOut) -> Result<()> {
    let natural_ext = crate::gemini::tts::extension_for_mime(&audio.mime);
    let (bytes, ext): (std::borrow::Cow<[u8]>, &str) =
        if audio.mime.starts_with("audio/L16") || audio.mime.starts_with("audio/pcm") {
            let sr = audio.sample_rate.unwrap_or(24000);
            (
                std::borrow::Cow::Owned(pcm16_to_wav(&audio.bytes, sr, 1)),
                natural_ext,
            )
        } else {
            (std::borrow::Cow::Borrowed(&audio.bytes), natural_ext)
        };

    if output == "-" {
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&bytes)?;
        return Ok(());
    }

    let mut path = PathBuf::from(shellexpand(output));
    let user_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase());
    if user_ext.is_none() {
        path.set_extension(ext);
    } else if let Some(u) = &user_ext {
        if u != ext && ext != "bin" {
            eprintln!(
                "warning: writing {} content to .{} file ({} would match the data)",
                audio.mime, u, ext
            );
        }
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&path, &*bytes)?;
    eprintln!("wrote {} ({})", path.display(), audio.mime);
    Ok(())
}

fn prompt_user(label: &str) -> Result<String> {
    eprint!("{label}: ");
    let _ = std::io::stderr().flush();
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    Ok(buf.trim().to_string())
}

fn load_input_images(paths: &[String]) -> Result<Vec<InputImage>> {
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let expanded = shellexpand(p);
        let path = PathBuf::from(&expanded);
        let att = crate::session::attachment::load(&path)?;
        if !att.mime.starts_with("image/") {
            eprintln!("warning: {} is {}, not an image", path.display(), att.mime);
        }
        out.push(InputImage {
            mime: att.mime,
            bytes: att.bytes,
        });
    }
    Ok(out)
}

fn write_images(output: &str, images: &[crate::gemini::image::ImageOut]) -> Result<()> {
    if output == "-" {
        if images.len() > 1 {
            anyhow::bail!("multiple images: cannot write all to stdout");
        }
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&images[0].bytes)?;
        return Ok(());
    }
    if images.len() == 1 {
        let path = PathBuf::from(shellexpand(output));
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(&path, &images[0].bytes)?;
        eprintln!("wrote {}", path.display());
        return Ok(());
    }
    let base = PathBuf::from(shellexpand(output));
    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("image");
    let ext_from_path = base.extension().and_then(|s| s.to_str()).map(String::from);
    let dir = base.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(dir)?;
    for (i, img) in images.iter().enumerate() {
        let ext = ext_from_path
            .clone()
            .unwrap_or_else(|| crate::gemini::image::extension_for_mime(&img.mime).to_string());
        let path = dir.join(format!("{stem}-{i}.{ext}"));
        std::fs::write(&path, &img.bytes)?;
        eprintln!("wrote {}", path.display());
    }
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
  .image / .tts / .music    one-off generation in REPL
  .undo              drop last completed turn from history (+ session DB)
  .retry             re-run the last user message
"
    );
}

fn handle_undo(state: &mut ReplState) -> Result<()> {
    if state.history.len() < 2 {
        eprintln!("(nothing to undo)");
        return Ok(());
    }
    state.history.pop();
    state.history.pop();
    if let Some(s) = &state.session {
        state.db.pop_last_turn(s.id())?;
    }
    eprintln!("(undid last turn)");
    Ok(())
}

async fn handle_retry(state: &mut ReplState) -> Result<()> {
    if state.history.is_empty() {
        eprintln!("(no previous turn to retry)");
        return Ok(());
    }
    // Find the last user message; drop the last completed turn from state + DB.
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
        eprintln!("(last user message has no text part to retry)");
        return Ok(());
    };
    if state.history.len() >= 2 {
        state.history.pop();
        state.history.pop();
    } else {
        state.history.clear();
    }
    if let Some(s) = &state.session {
        state.db.pop_last_turn(s.id())?;
    }
    chat_turn(state, text).await
}

fn print_info(state: &ReplState) {
    eprintln!("model:        {}", state.model.id);
    if let Some(r) = &state.role {
        eprintln!("role:         {}", r.name);
    }
    if let Some(s) = &state.session {
        eprintln!("session:      {}", s.label());
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

async fn chat_turn(state: &mut ReplState, user_text: String) -> Result<()> {
    let files = state.pending_files.clone();
    let (user_msg, attachments) = crate::session::build_user_message(user_text, &files)?;

    let mut contents = state.history.clone();
    contents.push(user_msg.clone());

    let req = ChatRequest {
        model: state.model.id.clone(),
        contents,
        system_instruction: state.system_prompt.clone(),
        generation_config: state.build_generation_config(),
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
                let hashes = crate::session::persist_attachments(&mut state.db, &attachments)?;
                let session_id = s.id();
                state.db.commit_turn(
                    session_id,
                    &user_msg,
                    &assistant,
                    Some(&state.model.id),
                    &hashes,
                )?;
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
                Some(Ok(ChatEvent::Finish { prompt_tokens, output_tokens, .. })) => {
                    *last_usage = (prompt_tokens, output_tokens);
                    return Ok(());
                }
            }
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

fn shellexpand(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    s.to_string()
}
