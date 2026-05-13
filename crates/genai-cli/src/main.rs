mod cli;
mod config;
mod gemini;
mod init;
mod models;
mod repl;
mod role;
mod session;
mod tools;

use anyhow::{Result, bail};
use clap::Parser;
use futures_util::StreamExt;
use std::io::{self, IsTerminal, Write};

use cli::{Cli, Command, ModelsCmd, SessionsCmd};
use gemini::Client;
use gemini::chat::{ChatEvent, ChatRequest};
use gemini::image::{self as image_api, ImageRequest, InputImage};
use gemini::tts::{AudioOut, MusicRequest, TtsRequest, pcm16_to_wav};
use gemini::types::{Content, GenerationConfig, Part};
use models::alias::{self, ResolvedModel};
use repl::render::{self};
use session::{ActiveSession, messages_to_contents, open_db};
use std::path::PathBuf;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config::load()?;

    if let Some(cmd) = &cli.command {
        return match cmd {
            Command::Models { sub } => match sub {
                ModelsCmd::List => cmd_models_list(),
            },
            Command::Sessions { sub } => match sub {
                SessionsCmd::List => cmd_sessions_list(),
                SessionsCmd::Delete { name } => cmd_sessions_delete(name),
                SessionsCmd::Export { name, output } => cmd_sessions_export(name, output.as_deref()),
            },
            Command::Gc => cmd_gc(),
            Command::Init { force } => init::run(*force),
        };
    }

    match cli.prompt_text() {
        Some(prompt) => run_one_shot(&cfg, &cli, prompt).await,
        None => run_repl(cfg, cli.session.as_deref(), cli.role.as_deref()).await,
    }
}

async fn run_one_shot(cfg: &config::Config, cli: &Cli, prompt: String) -> Result<()> {
    let registry = models::Registry::load()?;

    let role = match cli.role.as_deref() {
        Some(name) => Some(role::load(name)?),
        None => None,
    };

    let candidate_model = cli
        .model
        .clone()
        .or_else(|| role.as_ref().and_then(|r| r.model.clone()))
        .unwrap_or_else(|| cfg.default_chat_model().to_string());
    let resolved_candidate = alias::resolve(cfg, &candidate_model);
    let kind = registry
        .get(&resolved_candidate.id)
        .map(classify_kind)
        .unwrap_or(ModelKind::Chat);

    match kind {
        ModelKind::Chat => run_one_shot_chat(cfg, cli, prompt, role.as_ref(), &registry).await,
        ModelKind::Image => run_one_shot_image(cfg, cli, prompt, &resolved_candidate).await,
        ModelKind::Tts => run_one_shot_tts(cfg, cli, prompt, &resolved_candidate).await,
        ModelKind::Music => run_one_shot_music(cfg, cli, prompt, &resolved_candidate).await,
    }
}

#[derive(Copy, Clone)]
enum ModelKind {
    Chat,
    Image,
    Tts,
    Music,
}

fn classify_kind(e: &models::ModelEntry) -> ModelKind {
    if e.has(models::CAP_CHAT) {
        ModelKind::Chat
    } else if e.has(models::CAP_IMAGE_OUT) {
        ModelKind::Image
    } else if e.has(models::CAP_TTS) {
        ModelKind::Tts
    } else if e.has(models::CAP_MUSIC_OUT) {
        ModelKind::Music
    } else {
        ModelKind::Chat
    }
}

async fn run_one_shot_tts(
    cfg: &config::Config,
    cli: &Cli,
    text: String,
    resolved: &ResolvedModel,
) -> Result<()> {
    let api_key = cfg.require_api_key()?;
    let output = cli
        .output
        .clone()
        .ok_or_else(|| anyhow::anyhow!("TTS requires -o <path> or -o -"))?;
    let client = Client::new(api_key.to_string(), cfg.api_base().to_string())?;
    let audio = client
        .synthesize_speech(TtsRequest {
            model: resolved.id.clone(),
            text,
            voice: cfg.model.tts.voice.clone(),
        })
        .await?;
    write_audio(&output, &audio)
}

async fn run_one_shot_music(
    cfg: &config::Config,
    cli: &Cli,
    prompt: String,
    resolved: &ResolvedModel,
) -> Result<()> {
    let api_key = cfg.require_api_key()?;
    let output = cli
        .output
        .clone()
        .ok_or_else(|| anyhow::anyhow!("music requires -o <path> or -o -"))?;
    let client = Client::new(api_key.to_string(), cfg.api_base().to_string())?;
    let audio = client
        .generate_music(MusicRequest {
            model: resolved.id.clone(),
            prompt,
        })
        .await?;
    write_audio(&output, &audio)
}

fn write_audio(output: &str, audio: &AudioOut) -> Result<()> {
    let natural_ext = gemini::tts::extension_for_mime(&audio.mime);
    let (bytes, ext): (std::borrow::Cow<[u8]>, &str) =
        if audio.mime.starts_with("audio/L16") || audio.mime.starts_with("audio/pcm") {
            let sr = audio.sample_rate.unwrap_or(24000);
            (
                std::borrow::Cow::Owned(pcm16_to_wav(&audio.bytes, sr, 1)),
                natural_ext,
            )
        } else {
            (std::borrow::Cow::Borrowed(audio.bytes.as_slice()), natural_ext)
        };

    if output == "-" {
        let mut stdout = io::stdout().lock();
        stdout.write_all(&bytes)?;
        return Ok(());
    }
    let mut path = PathBuf::from(expand(output));
    let user_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase());
    if user_ext.is_none() {
        path.set_extension(ext);
    } else if let Some(u) = &user_ext
        && u != ext && ext != "bin" {
            eprintln!(
                "warning: writing {} content to .{} file ({} would match the data)",
                audio.mime, u, ext
            );
        }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    std::fs::write(&path, &*bytes)?;
    eprintln!("wrote {} ({})", path.display(), audio.mime);
    Ok(())
}

async fn run_one_shot_image(
    cfg: &config::Config,
    cli: &Cli,
    prompt: String,
    resolved: &ResolvedModel,
) -> Result<()> {
    let api_key = cfg.require_api_key()?;
    let output = cli
        .output
        .clone()
        .ok_or_else(|| anyhow::anyhow!("image generation requires -o <path> or -o -"))?;

    let client = Client::new(api_key.to_string(), cfg.api_base().to_string())?;

    let mut inputs = Vec::with_capacity(cli.file.len());
    for f in &cli.file {
        let p = std::path::PathBuf::from(expand(f));
        let att = session::attachment::load(&p)?;
        inputs.push(InputImage {
            mime: att.mime,
            bytes: att.bytes,
        });
    }

    let req = ImageRequest {
        model: resolved.id.clone(),
        prompt,
        input_images: inputs,
        aspect_ratio: None,
        count: None,
    };
    let images = client.generate_image(req).await?;
    write_images(&output, &images)?;
    Ok(())
}

fn write_images(output: &str, images: &[gemini::image::ImageOut]) -> Result<()> {
    if output == "-" {
        if images.len() > 1 {
            anyhow::bail!("multiple images: cannot write all to stdout");
        }
        let mut stdout = io::stdout().lock();
        stdout.write_all(&images[0].bytes)?;
        return Ok(());
    }
    if images.len() == 1 {
        let path = PathBuf::from(expand(output));
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        std::fs::write(&path, &images[0].bytes)?;
        eprintln!("wrote {}", path.display());
        return Ok(());
    }
    let base = PathBuf::from(expand(output));
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
            .unwrap_or_else(|| image_api::extension_for_mime(&img.mime).to_string());
        let path = dir.join(format!("{stem}-{i}.{ext}"));
        std::fs::write(&path, &img.bytes)?;
        eprintln!("wrote {}", path.display());
    }
    Ok(())
}

async fn run_repl(
    cfg: config::Config,
    session_name: Option<&str>,
    role_name: Option<&str>,
) -> Result<()> {
    let api_key = cfg.require_api_key()?;
    let client = Client::new(api_key.to_string(), cfg.api_base().to_string())?;
    let registry = models::Registry::load()?;
    let mut db = open_db()?;

    let role = match role_name {
        Some(name) => Some(role::load(name)?),
        None => None,
    };

    let chat_model_for_session = role
        .as_ref()
        .and_then(|r| r.model.as_deref())
        .unwrap_or_else(|| cfg.default_chat_model());

    let (active, history) = if let Some(name) = session_name {
        let s = db.get_or_create_session(name, Some(chat_model_for_session), None)?;
        let msgs = db.load_messages(s.id)?;
        let history = messages_to_contents(&msgs);
        (
            Some(ActiveSession {
                db_session: s,
                ephemeral: false,
            }),
            history,
        )
    } else {
        (None, Vec::new())
    };

    let state = repl::ReplState::new(cfg, client, registry, db, active, history, role);
    repl::run(state).await
}

async fn run_one_shot_chat(
    cfg: &config::Config,
    cli: &Cli,
    prompt: String,
    role: Option<&role::Role>,
    registry: &models::Registry,
) -> Result<()> {
    let api_key = cfg.require_api_key()?;

    let role_chat_capable = role
        .map(|r| role::is_chat_capable(r, registry))
        .unwrap_or(true);

    let mut db_opt = if cli.session.is_some() {
        Some(open_db()?)
    } else {
        None
    };

    let mut session_history: Vec<Content> = Vec::new();
    let mut active: Option<ActiveSession> = None;

    if let (Some(name), Some(db)) = (cli.session.as_deref(), db_opt.as_mut()) {
        let chat_model = cli
            .model
            .as_deref()
            .or_else(|| role.as_ref().and_then(|r| r.model.as_deref()))
            .filter(|_| role_chat_capable)
            .unwrap_or_else(|| cfg.default_chat_model());
        let s = db.get_or_create_session(name, Some(chat_model), None)?;
        session_history = messages_to_contents(&db.load_messages(s.id)?);
        active = Some(ActiveSession {
            db_session: s,
            ephemeral: false,
        });
    }

    let requested = cli
        .model
        .clone()
        .or_else(|| {
            role.as_ref()
                .filter(|_| role_chat_capable)
                .and_then(|r| r.model.clone())
        })
        .or_else(|| active.as_ref().and_then(|s| s.db_session.model.clone()))
        .unwrap_or_else(|| cfg.default_chat_model().to_string());
    let mut resolved = alias::resolve(cfg, &requested);
    if let Some(r) = &role
        && role_chat_capable {
            if let Some(t) = &r.thinking_level {
                resolved.thinking_level = Some(t.clone());
            }
            if let Some(t) = r.temperature {
                resolved.temperature = Some(t);
            }
        }
    models::validate(registry, &resolved.id, models::CAP_CHAT);

    let system_prompt = role
        .as_ref()
        .filter(|_| role_chat_capable)
        .and_then(|r| r.system_prompt.clone())
        .or_else(|| cfg.model.chat.system_prompt.clone());

    let enabled_tools = role
        .as_ref()
        .map(|r| r.tools.clone())
        .unwrap_or_default();
    tools::validate_all(registry, &resolved.id, &enabled_tools);

    let client = Client::new(api_key.to_string(), cfg.api_base().to_string())?;

    let files: Vec<PathBuf> = cli.file.iter().map(|s| PathBuf::from(expand(s))).collect();
    let (user_msg, attachments) = session::build_user_message(prompt, &files)?;
    let mut contents = session_history;
    contents.push(user_msg.clone());

    if tools::has_local(&enabled_tools) {
        return run_one_shot_with_tools(
            cli,
            cfg,
            registry,
            &client,
            resolved,
            system_prompt,
            enabled_tools,
            contents,
            user_msg,
            attachments,
            active,
            db_opt,
        )
        .await;
    }

    let req = ChatRequest {
        model: resolved.id.clone(),
        contents,
        system_instruction: system_prompt,
        generation_config: build_generation_config(cfg, &resolved),
        tools: tools::build_request_tools(&enabled_tools),
    };

    let mut stream = client.stream_chat(req).await?;

    let stdout = io::stdout();
    let tty = stdout.is_terminal() && !cli.no_stream;
    let style = render::pick_style(tty, cfg.repl.color, cfg.repl.markdown);
    let mut renderer = render::make_boxed(stdout, tty, style);
    let mut accumulated = String::new();

    let mut prompt_tok: Option<u32> = None;
    let mut output_tok: Option<u32> = None;
    let mut finish_reason: Option<String> = None;
    let mut finish_message: Option<String> = None;
    while let Some(ev) = stream.next().await {
        match ev? {
            ChatEvent::TextDelta(text) => {
                accumulated.push_str(&text);
                renderer.push(&text);
            }
            ChatEvent::Finish {
                prompt_tokens,
                output_tokens,
                reason,
                message,
            } => {
                prompt_tok = prompt_tokens;
                output_tok = output_tokens;
                finish_reason = reason;
                finish_message = message;
            }
        }
    }
    renderer.finish();
    if let Some(r) = finish_reason.as_deref()
        && r != "STOP"
    {
        if accumulated.is_empty() {
            eprintln!("(no response — finish_reason={r})");
        } else {
            eprintln!("(finish_reason={r})");
        }
        if let Some(m) = finish_message.as_deref()
            && !m.is_empty()
        {
            eprintln!("  {m}");
        }
    }
    drop(renderer);
    if let (Some(p), Some(o)) = (prompt_tok, output_tok) {
        let cost = registry
            .get(&resolved.id)
            .map(|m| {
                (p as f64) / 1_000_000.0 * m.input_price_per_1m
                    + (o as f64) / 1_000_000.0 * m.output_price_per_1m
            })
            .unwrap_or(0.0);
        if cost > 0.0 {
            eprintln!("(usage: prompt={p} output={o}, est. ${cost:.4})");
        } else {
            eprintln!("(usage: prompt={p} output={o})");
        }
    }

    if let (Some(s), Some(db)) = (active.as_ref(), db_opt.as_mut()) {
        let hashes = session::persist_attachments(db, &attachments)?;
        let assistant = Content {
            role: Some("model".to_string()),
            parts: vec![Part::Text { text: accumulated }],
        };
        db.commit_turn(s.id(), &user_msg, &assistant, Some(&resolved.id), &hashes)?;
    }

    let _ = io::stderr().flush();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_one_shot_with_tools(
    cli: &cli::Cli,
    cfg: &config::Config,
    registry: &models::Registry,
    client: &Client,
    resolved: ResolvedModel,
    system_prompt: Option<String>,
    enabled_tools: Vec<String>,
    contents: Vec<Content>,
    user_msg: Content,
    attachments: Vec<session::attachment::Attachment>,
    active: Option<session::ActiveSession>,
    db_opt: Option<session::db::Database>,
) -> Result<()> {
    let req = tools::runner::ToolLoopRequest {
        model: resolved.id.clone(),
        contents,
        system_instruction: system_prompt,
        generation_config: build_generation_config(cfg, &resolved),
        enabled_tools,
    };
    let mut ui = OneShotToolUi;
    let outcome = tools::runner::run(client, req, &mut ui).await?;

    let stdout = io::stdout();
    let tty = stdout.is_terminal() && !cli.no_stream;
    let style = render::pick_style(tty, cfg.repl.color, cfg.repl.markdown);
    let mut renderer = render::make_boxed(stdout, tty, style);
    renderer.push(&outcome.final_text);
    renderer.finish();
    drop(renderer);

    if let (Some(p), Some(o)) = (outcome.prompt_tokens, outcome.output_tokens) {
        let cost = registry
            .get(&resolved.id)
            .map(|m| {
                (p as f64) / 1_000_000.0 * m.input_price_per_1m
                    + (o as f64) / 1_000_000.0 * m.output_price_per_1m
            })
            .unwrap_or(0.0);
        if cost > 0.0 {
            eprintln!("(usage: prompt={p} output={o}, est. ${cost:.4})");
        } else {
            eprintln!("(usage: prompt={p} output={o})");
        }
    }

    if let (Some(s), Some(mut db)) = (active.as_ref(), db_opt) {
        let hashes = session::persist_attachments(&mut db, &attachments)?;
        db.commit_exchange(
            s.id(),
            &user_msg,
            &outcome.exchange,
            Some(&resolved.id),
            &hashes,
        )?;
    }
    let _ = io::stderr().flush();
    Ok(())
}

struct OneShotToolUi;

impl tools::runner::ToolUi for OneShotToolUi {
    fn announce_call(&mut self, name: &str, summary: &str) {
        eprintln!("[tool] {summary} ({name})");
    }
    fn announce_result(&mut self, _name: &str, ok: bool, preview: &str) {
        let tag = if ok { "ok" } else { "err" };
        eprintln!("[tool/{tag}] {preview}");
    }
    fn confirm(&mut self, _name: &str, summary: &str) -> tools::runner::Confirmation {
        use std::io::{BufRead, Write};
        let stdin = io::stdin();
        if !stdin.is_terminal() {
            eprintln!("[tool] {summary}: auto-denied (no TTY)");
            return tools::runner::Confirmation::Deny;
        }
        eprint!("[tool] run `{summary}`? [y/N] ");
        let _ = io::stderr().flush();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).is_err() {
            return tools::runner::Confirmation::Deny;
        }
        let answer = line.trim().to_ascii_lowercase();
        if matches!(answer.as_str(), "y" | "yes") {
            tools::runner::Confirmation::Allow
        } else {
            tools::runner::Confirmation::Deny
        }
    }
}

fn build_generation_config(
    cfg: &config::Config,
    resolved: &ResolvedModel,
) -> Option<GenerationConfig> {
    let temperature = resolved.temperature.or(cfg.model.chat.temperature);
    let max_output_tokens = cfg.model.chat.max_tokens;
    let thinking_config = resolved
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

fn cmd_models_list() -> Result<()> {
    let reg = models::Registry::load()?;
    let groups: [(&str, &str); 5] = [
        (models::CAP_CHAT, "Chat"),
        (models::CAP_IMAGE_OUT, "Image"),
        (models::CAP_TTS, "Text-to-Speech"),
        (models::CAP_MUSIC_OUT, "Music"),
        (models::CAP_EMBED, "Embedding"),
    ];
    for (cap, label) in groups {
        let items: Vec<_> = reg.iter_capability(cap).collect();
        if items.is_empty() {
            continue;
        }
        println!("\n{label}:");
        for m in items {
            let status = if m.status == "stable" {
                String::new()
            } else {
                format!(" [{}]", m.status)
            };
            println!("  {:<40}  {}{}", m.id, m.display_name, status);
        }
    }
    println!();
    Ok(())
}

fn cmd_sessions_list() -> Result<()> {
    let db = open_db()?;
    for s in db.list_sessions()? {
        println!("{}", session::format_summary(&s, None));
    }
    Ok(())
}

fn cmd_sessions_delete(name: &str) -> Result<()> {
    let mut db = open_db()?;
    if db.delete_session_ref(name)? {
        eprintln!("deleted session: {name}");
    } else {
        bail!("no session named {name}");
    }
    Ok(())
}

fn cmd_sessions_export(name: &str, output: Option<&str>) -> Result<()> {
    let db = open_db()?;
    let s = db
        .resolve_session_ref(name)?
        .ok_or_else(|| anyhow::anyhow!("no session named/id {name}"))?;
    match output {
        None | Some("-") => session::export_jsonl(&db, &s, io::stdout().lock())?,
        Some(path) => {
            let f = std::fs::File::create(path)?;
            session::export_jsonl(&db, &s, f)?;
        }
    }
    Ok(())
}

fn cmd_gc() -> Result<()> {
    let mut db = open_db()?;
    let n = session::gc_blobs(&mut db)?;
    eprintln!("removed {n} orphaned attachment(s)");
    Ok(())
}

fn expand(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    s.to_string()
}
