mod audit;
mod cli;
mod config;
mod gemini;
mod init;
mod models;
mod output;
mod repl;
mod role;
mod session;
mod spinner;
mod tools;
mod ui;

use anyhow::{Result, bail};
use clap::Parser;
use futures_util::StreamExt;
use std::io::{self, IsTerminal, Write};

use cli::{AuditCmd, Cli, Command, ModelsCmd, SessionsCmd};
use gemini::Client;
use gemini::chat::{ChatEvent, ChatRequest};
use gemini::image::ImageRequest;
use gemini::tts::{MusicRequest, TtsRequest};
use gemini::types::{Content, FinishReason, GenerationConfig, Part};
use models::alias::{self, ResolvedModel};
use output::{expand_path, write_audio, write_images};
use repl::render::{self};
use session::{ActiveSession, messages_to_contents, open_db};
use std::path::PathBuf;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    if let Err(err) = real_main().await {
        print_error_chain(&err);
        std::process::exit(1);
    }
}

async fn real_main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let cfg = config::load()?;

    if let Some(cmd) = &cli.command {
        return match cmd {
            Command::Models { sub } => match sub {
                ModelsCmd::List => cmd_models_list(),
                ModelsCmd::Sync { dry_run } => {
                    models::sync::run(&cfg, models::sync::SyncOptions { dry_run: *dry_run })
                        .await
                }
            },
            Command::Sessions { sub } => match sub {
                SessionsCmd::List => cmd_sessions_list(),
                SessionsCmd::Delete { name } => cmd_sessions_delete(name),
                SessionsCmd::Export { name, output } => cmd_sessions_export(name, output.as_deref()),
            },
            Command::Gc => cmd_gc(),
            Command::Init { force } => init::run(*force),
            Command::Audit { sub } => match sub {
                AuditCmd::Tail { count, json } => cmd_audit_tail(*count, *json),
            },
        };
    }

    match cli.prompt_text() {
        Some(prompt) => run_one_shot(&cfg, &cli, prompt).await,
        None => run_repl(cfg, cli.session.as_deref(), cli.role.as_deref()).await,
    }
}

/// Install a stderr `tracing` subscriber filtered by the `GENAI_LOG`
/// environment variable (falls back to `RUST_LOG`). With neither set, no
/// trace output is produced — user-visible messages stay on the usual
/// stderr/stdout paths. Examples: `GENAI_LOG=genai=debug`,
/// `GENAI_LOG=info,genai::gemini=trace`.
fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = std::env::var("GENAI_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .ok()
        .and_then(|s| EnvFilter::try_new(s).ok())
        .unwrap_or_else(|| EnvFilter::new("off"));
    let _ = fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .with_target(true)
        .without_time()
        .try_init();
}

/// Print an anyhow error and its cause chain in a stable format suitable for
/// the CLI's stderr. Top line is `error: <message>`; each subsequent cause is
/// indented under `caused by:`.
fn print_error_chain(err: &anyhow::Error) {
    eprintln!("error: {err}");
    for cause in err.chain().skip(1) {
        eprintln!("  caused by: {cause}");
    }
}

/// Resolve `-o` for media subcommands.
///
/// - `Some(path)` passes through unchanged.
/// - None + TTY stdout → auto-generate a path under `<data_dir>/generated/`
///   (or the user-configured override).
/// - None + non-TTY stdout → error, because silently writing files when
///   scripts expected stdout binary would be a surprise; the user almost
///   certainly meant `-o -`.
fn resolve_one_shot_output(
    cli: &Cli,
    cfg: &config::Config,
    kind: output::GeneratedKind,
    prompt: &str,
) -> Result<String> {
    if let Some(s) = cli.output.clone() {
        return Ok(s);
    }
    if !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "no output path; pass -o <path> or -o - when stdout is not a TTY"
        );
    }
    let path = output::default_generated_path(cfg, kind, prompt)?;
    Ok(path.display().to_string())
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
    let output = resolve_one_shot_output(cli, cfg, output::GeneratedKind::Tts, &text)?;
    let client = Client::new(api_key.to_string(), cfg.api_base().to_string())?;
    let audio = {
        let _s = spinner::Spinner::start("synthesizing speech...");
        client
            .synthesize_speech(TtsRequest {
                model: resolved.id.clone(),
                text,
                voice: cfg.model.tts.voice.clone(),
            })
            .await?
    };
    write_audio(&output, &audio)
}

async fn run_one_shot_music(
    cfg: &config::Config,
    cli: &Cli,
    prompt: String,
    resolved: &ResolvedModel,
) -> Result<()> {
    let api_key = cfg.require_api_key()?;
    let output = resolve_one_shot_output(cli, cfg, output::GeneratedKind::Music, &prompt)?;
    let client = Client::new(api_key.to_string(), cfg.api_base().to_string())?;
    let audio = {
        let _s = spinner::Spinner::start("generating music...");
        client
            .generate_music(MusicRequest {
                model: resolved.id.clone(),
                prompt,
            })
            .await?
    };
    write_audio(&output, &audio)
}

async fn run_one_shot_image(
    cfg: &config::Config,
    cli: &Cli,
    prompt: String,
    resolved: &ResolvedModel,
) -> Result<()> {
    let api_key = cfg.require_api_key()?;
    let output = resolve_one_shot_output(cli, cfg, output::GeneratedKind::Image, &prompt)?;

    let client = Client::new(api_key.to_string(), cfg.api_base().to_string())?;

    let inputs = output::load_input_images(&cli.file)?;

    let req = ImageRequest {
        model: resolved.id.clone(),
        prompt,
        input_images: inputs,
        aspect_ratio: None,
        count: None,
    };
    let images = {
        let _s = spinner::Spinner::start("generating image...");
        client.generate_image(req).await?
    };
    let preview = output::image_preview::Preference::from_config(cfg.output.image_preview.as_deref());
    write_images(&output, &images, preview)?;
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
        (Some(ActiveSession { db_session: s }), history)
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
        active = Some(ActiveSession { db_session: s });
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

    let files: Vec<PathBuf> = cli.file.iter().map(|s| PathBuf::from(expand_path(s))).collect();
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

    let gen_cfg = build_generation_config(cfg, &resolved);
    let tools_list = tools::build_request_tools(&enabled_tools);
    let req = ChatRequest {
        model: &resolved.id,
        contents: &contents,
        system_instruction: system_prompt.as_deref(),
        generation_config: gen_cfg.as_ref(),
        tools: tools_list.as_deref(),
    };

    let mut spinner = spinner::Spinner::start("thinking...");
    let mut stream = client.stream_chat(req).await?;

    let stdout = io::stdout();
    let tty = stdout.is_terminal() && !cli.no_stream;
    let style = render::pick_style(tty, cfg.repl.color, cfg.repl.markdown);
    let mut renderer = render::make_boxed(stdout, tty, style);
    let mut accumulated = String::new();

    let mut prompt_tok: Option<u32> = None;
    let mut output_tok: Option<u32> = None;
    let mut finish_reason: Option<FinishReason> = None;
    let mut finish_message: Option<String> = None;
    while let Some(ev) = stream.next().await {
        match ev? {
            ChatEvent::TextDelta(text) => {
                spinner.take();
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
    if let Some(r) = finish_reason.as_ref()
        && !r.is_normal()
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
        let assistant = Content {
            role: Some("model".to_string()),
            parts: vec![Part::Text { text: accumulated }],
        };
        if let Err(e) = persist_one_shot_turn(db, s.id(), &user_msg, &assistant, &resolved.id, &attachments)
        {
            eprintln!("warning: failed to persist turn: {e}");
        }
    }

    let _ = io::stderr().flush();
    Ok(())
}

fn persist_one_shot_turn(
    db: &mut session::db::Database,
    session_id: i64,
    user: &Content,
    assistant: &Content,
    model_id: &str,
    attachments: &[session::attachment::Attachment],
) -> Result<()> {
    let hashes = session::persist_attachments(db, attachments)?;
    db.commit_turn(session_id, user, assistant, Some(model_id), &hashes)
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
    let mut ui = tools::cli_ui::CliToolUi::new();
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

    if let (Some(s), Some(mut db)) = (active.as_ref(), db_opt)
        && let Err(e) = persist_one_shot_exchange(
            &mut db,
            s.id(),
            &user_msg,
            &outcome.exchange,
            &resolved.id,
            &attachments,
        )
    {
        eprintln!("warning: failed to persist turn: {e}");
    }
    let _ = io::stderr().flush();
    Ok(())
}

fn persist_one_shot_exchange(
    db: &mut session::db::Database,
    session_id: i64,
    user: &Content,
    chain: &[Content],
    model_id: &str,
    attachments: &[session::attachment::Attachment],
) -> Result<()> {
    let hashes = session::persist_attachments(db, attachments)?;
    db.commit_exchange(session_id, user, chain, Some(model_id), &hashes)
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

fn cmd_audit_tail(count: usize, json: bool) -> Result<()> {
    let lines = audit::tail(count);
    if lines.is_empty() {
        let path = audit::log_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(no log path)".to_string());
        eprintln!("(audit log empty or missing: {path})");
        return Ok(());
    }
    if json {
        for line in &lines {
            println!("{line}");
        }
    } else {
        for line in &lines {
            println!("{}", format_audit_line(line));
        }
    }
    Ok(())
}

fn format_audit_line(line: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return line.to_string();
    };
    let ts = v.get("ts").and_then(|t| t.as_str()).unwrap_or("?");
    let tool = v.get("tool").and_then(|t| t.as_str()).unwrap_or("?");
    let result = v.get("result").and_then(|t| t.as_str()).unwrap_or("?");
    let preview = v.get("preview").and_then(|t| t.as_str()).unwrap_or("");
    format!("{ts}  {result:<6}  {tool:<14}  {preview}")
}

