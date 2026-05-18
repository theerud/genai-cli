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
    /// Per-role overrides for media generation models. Each field, when
    /// set, wins over the global `[media]` table in config.toml for
    /// `generate_media` calls made under this role.
    #[serde(default)]
    pub media: crate::config::MediaConfig,
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

/// Resolve the effective media model for `kind`, applying the
/// role-overrides-cfg precedence:
///
/// 1. `role.media.<kind>` if the role sets it
/// 2. `cfg.media.<kind>` from config.toml
/// 3. Legacy `[model.image]` / `[model.tts]` (warned once)
/// 4. Hardcoded fallback
pub fn effective_media(
    role: Option<&Role>,
    cfg: &crate::config::Config,
    kind: crate::config::MediaKind,
) -> String {
    if let Some(r) = role
        && let Some(v) = media_field(&r.media, kind)
    {
        return v.to_string();
    }
    cfg.media_default(kind)
}

fn media_field(m: &crate::config::MediaConfig, kind: crate::config::MediaKind) -> Option<&str> {
    match kind {
        crate::config::MediaKind::Image => m.image.as_deref(),
        crate::config::MediaKind::Speech => m.speech.as_deref(),
        crate::config::MediaKind::Music => m.music.as_deref(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, MediaConfig, MediaKind, ModelDefaults, ModelImage, ModelTts};

    fn role_with_media(image: Option<&str>, speech: Option<&str>, music: Option<&str>) -> Role {
        Role {
            media: MediaConfig {
                image: image.map(String::from),
                speech: speech.map(String::from),
                music: music.map(String::from),
            },
            ..Default::default()
        }
    }

    fn cfg_with_new_media(image: Option<&str>, speech: Option<&str>, music: Option<&str>) -> Config {
        Config {
            media: MediaConfig {
                image: image.map(String::from),
                speech: speech.map(String::from),
                music: music.map(String::from),
            },
            ..Default::default()
        }
    }

    fn cfg_with_legacy(image: Option<&str>, tts: Option<&str>) -> Config {
        Config {
            model: ModelDefaults {
                image: ModelImage { default: image.map(String::from) },
                tts: ModelTts { default: tts.map(String::from), voice: None },
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn role_media_overrides_cfg_media() {
        let cfg = cfg_with_new_media(Some("cfg-img"), Some("cfg-tts"), Some("cfg-mus"));
        let role = role_with_media(Some("role-img"), None, None);
        assert_eq!(effective_media(Some(&role), &cfg, MediaKind::Image), "role-img");
        assert_eq!(effective_media(Some(&role), &cfg, MediaKind::Speech), "cfg-tts");
        assert_eq!(effective_media(Some(&role), &cfg, MediaKind::Music), "cfg-mus");
    }

    #[test]
    fn cfg_media_used_when_role_absent() {
        let cfg = cfg_with_new_media(Some("cfg-img"), None, None);
        assert_eq!(effective_media(None, &cfg, MediaKind::Image), "cfg-img");
    }

    #[test]
    fn legacy_model_image_falls_back() {
        let cfg = cfg_with_legacy(Some("legacy-img"), Some("legacy-tts"));
        assert_eq!(effective_media(None, &cfg, MediaKind::Image), "legacy-img");
        assert_eq!(effective_media(None, &cfg, MediaKind::Speech), "legacy-tts");
    }

    #[test]
    fn hardcoded_fallback_when_nothing_set() {
        let cfg = Config::default();
        let img = effective_media(None, &cfg, MediaKind::Image);
        assert!(img.starts_with("imagen-"));
        let music = effective_media(None, &cfg, MediaKind::Music);
        assert!(music.starts_with("lyria-"));
    }

    #[test]
    fn cfg_media_wins_over_legacy() {
        let mut cfg = cfg_with_new_media(Some("new"), None, None);
        cfg.model.image.default = Some("legacy".into());
        assert_eq!(effective_media(None, &cfg, MediaKind::Image), "new");
    }
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
