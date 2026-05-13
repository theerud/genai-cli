use anyhow::{Context, Result, bail};
use std::io::IsTerminal;
use std::path::PathBuf;

use crate::config;
use crate::models;
use crate::ui;

pub fn run(force: bool) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!("genai init needs an interactive terminal");
    }

    let paths = config::paths()?;
    std::fs::create_dir_all(&paths.config_dir)
        .with_context(|| format!("creating {}", paths.config_dir.display()))?;

    let config_path = paths.config_dir.join("config.toml");
    let env_path = paths.config_dir.join(".env");
    let roles_dir = paths.config_dir.join("roles");

    eprintln!("genai init — first-run setup");
    eprintln!("Config dir: {}", paths.config_dir.display());
    eprintln!();

    if (config_path.exists() || env_path.exists()) && !force {
        eprintln!("Existing files detected:");
        if config_path.exists() {
            eprintln!("  {}", config_path.display());
        }
        if env_path.exists() {
            eprintln!("  {}", env_path.display());
        }
        if !ui::confirm("Overwrite?", false)? {
            eprintln!("Aborted.");
            return Ok(());
        }
    }

    // API key
    eprintln!();
    eprintln!("Step 1/3 — API key");
    eprintln!("Get one at https://aistudio.google.com/apikey");
    let api_key = ui::read_secret("API key (input hidden if your tty supports it)")?;
    if api_key.trim().is_empty() {
        bail!("API key is required");
    }

    // Default chat model
    eprintln!();
    eprintln!("Step 2/3 — Default chat model");
    let reg = models::Registry::load()?;
    let chat_models: Vec<&str> = reg
        .iter_capability(models::CAP_CHAT)
        .filter(|m| m.status == "stable")
        .map(|m| m.id.as_str())
        .collect();
    for (i, m) in chat_models.iter().enumerate() {
        eprintln!("  [{}] {}", i + 1, m);
    }
    eprintln!();
    let default_idx = chat_models
        .iter()
        .position(|m| *m == "gemini-2.5-flash")
        .unwrap_or(0);
    let choice = ui::read_with_default(
        &format!("Pick by number [default: {}]", default_idx + 1),
        &(default_idx + 1).to_string(),
    )?;
    let idx: usize = choice
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|n| *n >= 1 && *n <= chat_models.len())
        .unwrap_or(default_idx + 1);
    let default_model = chat_models[idx - 1].to_string();

    // Starter roles
    eprintln!();
    eprintln!("Step 3/3 — Starter roles (optional)");
    eprintln!("  coding    — senior-engineer system prompt, gemini-2.5-pro");
    eprintln!("  research  — citation-first assistant with google_search enabled");
    let want_roles = ui::confirm("Install both?", true)?;

    // Write .env
    std::fs::write(&env_path, format!("GEMINI_API_KEY={api_key}\n"))
        .with_context(|| format!("writing {}", env_path.display()))?;
    set_user_only(&env_path)?;

    // Write config.toml
    let cfg_body = render_config_toml(&default_model);
    std::fs::write(&config_path, cfg_body)
        .with_context(|| format!("writing {}", config_path.display()))?;

    // Optional roles
    let mut installed_roles: Vec<&str> = Vec::new();
    if want_roles {
        std::fs::create_dir_all(&roles_dir)?;
        for (name, body) in [
            ("coding", STARTER_CODING_ROLE),
            ("research", STARTER_RESEARCH_ROLE),
        ] {
            let role_path = roles_dir.join(format!("{name}.toml"));
            if !role_path.exists() || force {
                std::fs::write(&role_path, body)
                    .with_context(|| format!("writing {}", role_path.display()))?;
                installed_roles.push(name);
            }
        }
    }

    eprintln!();
    eprintln!("Done.");
    eprintln!("  .env       → {}", env_path.display());
    eprintln!("  config     → {}", config_path.display());
    for name in &installed_roles {
        eprintln!("  role       → {}/{name}.toml", roles_dir.display());
    }
    eprintln!();
    eprintln!("Try it: genai \"hello\"");
    if installed_roles.contains(&"research") {
        eprintln!("    or: genai -r research \"what's new in <topic> this week?\"");
    }
    eprintln!();
    eprintln!("Heads-up on dangerous roles:");
    eprintln!("  Tools like `exec` and `fetch_url` give the model real reach into your");
    eprintln!("  machine. The init wizard deliberately does NOT install a `sysadmin`-style");
    eprintln!("  role. If you want one, see DESIGN.md (`### Tools`) for the schema and the");
    eprintln!("  confirmation-prompt model. Treat such roles like any sudo-adjacent tool.");
    Ok(())
}

fn render_config_toml(default_model: &str) -> String {
    format!(
        r#"# genai-cli config. See DESIGN.md for the full reference.
#
# The API key is loaded from $GEMINI_API_KEY, ./.env, or this directory's
# .env — keep it out of this file. To override the env-var name:
#
# api_key_env = "GEMINI_PERSONAL_KEY"
#
# To point at a non-default Gemini endpoint:
# api_base = "https://generativelanguage.googleapis.com"

[model.chat]
default = "{default_model}"
# temperature = 0.7        # 0.0 = deterministic, 1.0 = creative
# max_tokens = 8192        # cap response length
# system_prompt = ""       # baseline system instruction for every chat

[model.image]
default = "gemini-2.5-flash-image"   # 'nano-banana'; supports image-in editing

[model.tts]
default = "gemini-2.5-flash-preview-tts"
voice = "Kore"

[model.embed]
default = "gemini-embedding-2"

[repl]
markdown = true            # render ANSI-colored markdown to a TTY
color = true               # syntax-highlight fenced code blocks

# In-terminal image preview after `.image` / image generation. Default
# "auto" probes the terminal; "iterm2" or "kitty" force a protocol;
# "off" disables. Force a protocol if your terminal advertises Kitty
# support but rendering doesn't actually work for you.
#
# [output]
# image_preview = "auto"

# Aliases are named bundles of (model, per-model params). Usable anywhere a
# model id is expected: `genai -m pro-high "…"`, `.model pro-high` in the REPL,
# etc. The thinking_level maps to one of: off, low, medium, high, dynamic.
#
# [aliases.pro-high]
# model = "gemini-2.5-pro"
# thinking_level = "high"
#
# [aliases.fast]
# model = "gemini-2.5-flash-lite"
# temperature = 0.3
"#
    )
}

const STARTER_CODING_ROLE: &str = r#"# Starter role. Edit to taste.
model = "gemini-2.5-pro"
system_prompt = """
You are a senior software engineer. Be precise. Answer with code where
it helps. Skip pleasantries; assume the reader is fluent.
"""
temperature = 0.4
thinking_level = "high"
"#;

const STARTER_RESEARCH_ROLE: &str = r#"# Web-grounded research assistant. Uses google_search server-side.
model = "gemini-2.5-pro"
system_prompt = """
You are a research assistant. For anything time-sensitive or factual,
call google_search first and cite the URLs you used inline. If the search
returns nothing useful, say so rather than guessing.
"""
tools = ["google_search"]
"#;

#[cfg(unix)]
fn set_user_only(path: &PathBuf) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let f = std::fs::metadata(path)?;
    let mut perms = f.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_user_only(_path: &PathBuf) -> Result<()> {
    Ok(())
}
