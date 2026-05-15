use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Role {
    #[serde(skip)]
    pub name: String,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub thinking_level: Option<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    /// `"chat"` (default) or `"loop"`. In `"loop"` mode, the role can run
    /// multiple tool-call iterations under a single user prompt, and only
    /// the user prompt and final answer are kept in session history.
    pub mode: Option<String>,
    /// Cap on tool-loop iterations. Applies to both chat and loop modes;
    /// in interactive loop mode the user is prompted to extend the budget
    /// when the cap is hit.
    pub max_iterations: Option<u32>,
}

pub const DEFAULT_MAX_ITERATIONS: u32 = 8;

impl Role {
    pub fn is_loop_mode(&self) -> bool {
        matches!(self.mode.as_deref(), Some("loop"))
    }
}

/// Resolve the iteration budget: explicit CLI override wins, then the
/// role's `max_iterations`, then the global default.
pub fn iter_budget(cli_override: Option<u32>, role: Option<&Role>) -> u32 {
    cli_override
        .or_else(|| role.and_then(|r| r.max_iterations))
        .unwrap_or(DEFAULT_MAX_ITERATIONS)
        .max(1)
}

pub fn roles_dir() -> Result<PathBuf> {
    let p = crate::config::paths()?;
    Ok(p.config_dir.join("roles"))
}

pub fn load(name: &str) -> Result<Role> {
    let path = roles_dir()?.join(format!("{name}.toml"));
    load_from_path(&path, name)
}

pub fn load_from_path(path: &Path, name: &str) -> Result<Role> {
    let s = std::fs::read_to_string(path)
        .with_context(|| format!("reading role file {}", path.display()))?;
    let mut role: Role = toml::from_str(&s)
        .with_context(|| format!("parsing role file {}", path.display()))?;
    role.name = name.to_string();
    Ok(role)
}

pub fn list_available() -> Result<Vec<String>> {
    let dir = roles_dir()?;
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("toml")
            && let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                out.push(stem.to_string());
            }
    }
    out.sort();
    Ok(out)
}

/// True when the role's configured model is a chat-capable model in the registry.
/// Unknown models default to chat-capable (no false-negatives blocking the user).
pub fn is_chat_capable(role: &Role, registry: &crate::models::Registry) -> bool {
    let Some(model_id) = role.model.as_deref() else {
        return true;
    };
    match registry.get(model_id) {
        Some(entry) => entry.has(crate::models::CAP_CHAT),
        None => true,
    }
}
