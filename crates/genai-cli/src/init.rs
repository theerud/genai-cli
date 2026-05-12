use anyhow::{Context, Result, bail};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use crate::config;
use crate::models;

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
        if !confirm("Overwrite?", false)? {
            eprintln!("Aborted.");
            return Ok(());
        }
    }

    // API key
    eprintln!();
    eprintln!("Step 1/3 — API key");
    eprintln!("Get one at https://aistudio.google.com/apikey");
    let api_key = prompt_secret("API key (input hidden if your tty supports it)")?;
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
    let choice = prompt_default(
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

    // Starter role
    eprintln!();
    eprintln!("Step 3/3 — Starter role (optional)");
    let want_role = confirm("Create a starter 'coding' role?", true)?;

    // Write .env
    std::fs::write(&env_path, format!("GEMINI_API_KEY={api_key}\n"))
        .with_context(|| format!("writing {}", env_path.display()))?;
    set_user_only(&env_path)?;

    // Write config.toml
    let cfg_body = render_config_toml(&default_model);
    std::fs::write(&config_path, cfg_body)
        .with_context(|| format!("writing {}", config_path.display()))?;

    // Optional role
    if want_role {
        std::fs::create_dir_all(&roles_dir)?;
        let role_path = roles_dir.join("coding.toml");
        if !role_path.exists() || force {
            std::fs::write(&role_path, STARTER_CODING_ROLE)
                .with_context(|| format!("writing {}", role_path.display()))?;
        }
    }

    eprintln!();
    eprintln!("Done.");
    eprintln!("  .env       → {}", env_path.display());
    eprintln!("  config     → {}", config_path.display());
    if want_role {
        eprintln!("  role       → {}/coding.toml", roles_dir.display());
    }
    eprintln!();
    eprintln!("Try it: genai \"hello\"");
    Ok(())
}

fn render_config_toml(default_model: &str) -> String {
    format!(
        r#"# genai-cli config.
# API key is loaded from $GEMINI_API_KEY or ~/.config/genai/.env — keep it
# out of this file.

# api_key_env = "GEMINI_API_KEY"
# api_base = "https://generativelanguage.googleapis.com"

[model.chat]
default = "{default_model}"
# temperature = 0.7
# max_tokens = 8192
# system_prompt = ""

[model.image]
default = "gemini-2.5-flash-image"

[model.tts]
default = "gemini-2.5-flash-preview-tts"
voice = "Kore"

[model.embed]
default = "gemini-embedding-2"

[repl]
markdown = true
color = true

# Aliases: named bundles of model + per-model params.
# [aliases.pro-high]
# model = "gemini-2.5-pro"
# thinking_level = "high"
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

fn confirm(question: &str, default_yes: bool) -> Result<bool> {
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

fn prompt_default(label: &str, default: &str) -> Result<String> {
    eprint!("{label}: ");
    let _ = std::io::stderr().flush();
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    let trimmed = buf.trim();
    Ok(if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    })
}

#[cfg(unix)]
fn prompt_secret(label: &str) -> Result<String> {
    eprint!("{label}: ");
    let _ = std::io::stderr().flush();

    // Best-effort: disable echo via stty if available. If anything fails,
    // fall back to visible input.
    let echo_off = std::process::Command::new("stty")
        .arg("-echo")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let mut buf = String::new();
    let read_result = std::io::stdin().read_line(&mut buf);

    if echo_off {
        let _ = std::process::Command::new("stty").arg("echo").status();
        eprintln!();
    }
    read_result.context("reading API key")?;
    Ok(buf.trim().to_string())
}

#[cfg(not(unix))]
fn prompt_secret(label: &str) -> Result<String> {
    prompt_default(label, "")
}

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
