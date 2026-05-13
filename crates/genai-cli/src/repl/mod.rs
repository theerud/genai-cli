pub mod chat;
pub mod commands;
pub mod complete;
pub mod dispatch;
pub mod media;
pub mod prompt;
pub mod render;
pub mod sessions;

use anyhow::{Context, Result};
use rustyline::Editor;
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use std::path::PathBuf;

use crate::config::Config;
use crate::gemini::Client;
use crate::gemini::types::{Content, GenerationConfig};
use crate::models::Registry;
use crate::models::alias::{self, ResolvedModel};
use crate::role::{self, Role};
use crate::session::{ActiveSession, db::Database};
use crate::tools;
use commands::{DotCmd, parse as parse_cmd};
use prompt::PromptState;

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
    pub active_tools: Vec<String>,
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
        let active_tools = role
            .as_ref()
            .map(|r| r.tools.clone())
            .unwrap_or_default();
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
            active_tools,
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
    let mut rl: Editor<complete::ReplHelper, DefaultHistory> = Editor::new()?;
    rl.set_helper(Some(complete::ReplHelper::new()));
    let history_path = history_file_path()?;
    let _ = rl.load_history(&history_path);

    print_banner(&state);

    loop {
        refresh_completer(&mut rl, &state);
        let prompt_str = state.prompt();
        let line = tokio::task::block_in_place(|| rl.readline(&prompt_str));
        match line {
            Ok(line) => {
                let _ = rl.add_history_entry(line.as_str());
                if let Some(cmd) = parse_cmd(&line) {
                    if matches!(cmd, DotCmd::Exit) {
                        break;
                    }
                    if let Err(e) = dispatch::handle_command(&mut state, cmd).await {
                        eprintln!("error: {e}");
                    }
                    continue;
                }
                if line.trim().is_empty() {
                    continue;
                }
                if let Err(e) = chat::chat_turn(&mut state, line).await {
                    eprintln!("error: {e}");
                }
            }
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => {
                if sessions::maybe_prompt_save_on_exit(&mut state)? {
                    continue;
                }
                break;
            },
            Err(e) => {
                eprintln!("error: readline: {e}");
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

/// Refresh the completer's snapshots from current REPL state. Cheap enough
/// to run before each readline.
fn refresh_completer(rl: &mut Editor<complete::ReplHelper, DefaultHistory>, state: &ReplState) {
    let Some(helper) = rl.helper_mut() else {
        return;
    };
    helper.role_names = role::list_available().unwrap_or_default();
    helper.session_labels = state
        .db
        .list_sessions()
        .unwrap_or_default()
        .into_iter()
        .flat_map(|s| [s.name, format!("#{}", s.id)])
        .collect();
    helper.model_names = state
        .registry
        .models
        .iter()
        .filter(|m| m.has(crate::models::CAP_CHAT))
        .map(|m| m.id.clone())
        .chain(state.cfg.aliases.keys().cloned())
        .collect();
    let mut tools: Vec<String> = tools::builtin_names()
        .iter()
        .map(|s| s.to_string())
        .collect();
    tools.extend(tools::local_names().iter().map(|s| s.to_string()));
    helper.tool_names = tools;
}

