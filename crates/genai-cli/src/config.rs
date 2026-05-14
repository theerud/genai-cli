use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const QUALIFIER: &str = "";
const ORG: &str = "";
const APP: &str = "genai";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub api_base: Option<String>,
    #[serde(default)]
    pub model: ModelDefaults,
    #[serde(default)]
    pub output: OutputPaths,
    #[serde(default)]
    pub repl: ReplConfig,
    #[serde(default)]
    pub aliases: BTreeMap<String, AliasEntry>,
    #[serde(default)]
    pub security: SecurityConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelDefaults {
    #[serde(default)]
    pub chat: ModelChat,
    #[serde(default)]
    pub image: ModelImage,
    #[serde(default)]
    pub tts: ModelTts,
    #[serde(default)]
    pub embed: ModelEmbed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelChat {
    pub default: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub system_prompt: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelImage {
    pub default: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelTts {
    pub default: Option<String>,
    pub voice: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelEmbed {
    pub default: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutputPaths {
    pub image_dir: Option<String>,
    pub audio_dir: Option<String>,
    /// "auto" (default), "kitty", "iterm2", or "off". Controls in-terminal
    /// image preview after `.image` / one-off image generation.
    pub image_preview: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReplConfig {
    pub color: Option<bool>,
    pub markdown: Option<bool>,
    pub history_size: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AliasEntry {
    pub model: String,
    pub temperature: Option<f32>,
    pub thinking_level: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// Extra paths to refuse in `read_file` / `list_dir`. Merged onto the
    /// built-in deny list (`~/.ssh/`, `~/.aws/`, `~/.gnupg/`, `~/.netrc`,
    /// `<config_dir>/.env`). Prefix match after tilde expansion + symlink
    /// resolution.
    #[serde(default)]
    pub read_paths_deny: Vec<String>,
    /// Paths to permit even if they match a deny pattern. Allow wins.
    #[serde(default)]
    pub read_paths_allow: Vec<String>,
    /// Extra hostnames or string prefixes to refuse in `fetch_url`. Merged
    /// onto the built-in private-range list.
    #[serde(default)]
    pub fetch_hosts_deny: Vec<String>,
    /// Hostnames to permit even when they match a deny pattern.
    #[serde(default)]
    pub fetch_hosts_allow: Vec<String>,
    #[serde(default)]
    pub audit: AuditConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditConfig {
    /// Append one JSON line per tool call to `<data_dir>/tool-log.jsonl`.
    #[serde(default = "default_audit_enabled")]
    pub enabled: bool,
    /// Soft cap on the audit log. When exceeded by 10%, the file is
    /// trimmed in place back to this many lines (oldest dropped).
    #[serde(default = "default_audit_max_lines")]
    pub max_lines: usize,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            enabled: default_audit_enabled(),
            max_lines: default_audit_max_lines(),
        }
    }
}

fn default_audit_enabled() -> bool {
    true
}
fn default_audit_max_lines() -> usize {
    5000
}

pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

pub fn paths() -> Result<Paths> {
    if let Ok(home) = std::env::var("GENAI_HOME")
        && !home.is_empty()
    {
        let root = PathBuf::from(home);
        return Ok(Paths {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
        });
    }
    let pd = ProjectDirs::from(QUALIFIER, ORG, APP)
        .context("could not determine XDG directories")?;
    Ok(Paths {
        config_dir: pd.config_dir().to_path_buf(),
        data_dir: pd.data_dir().to_path_buf(),
        cache_dir: pd.cache_dir().to_path_buf(),
    })
}

pub fn load() -> Result<Config> {
    let p = paths()?;
    let cfg_path = p.config_dir.join("config.toml");
    let mut cfg: Config = if cfg_path.exists() {
        let s = std::fs::read_to_string(&cfg_path)
            .with_context(|| format!("reading {}", cfg_path.display()))?;
        toml::from_str(&s).with_context(|| format!("parsing {}", cfg_path.display()))?
    } else {
        Config::default()
    };

    if cfg.api_key.is_none() {
        let env_var_name = cfg.api_key_env.as_deref().unwrap_or("GEMINI_API_KEY");
        if let Some(value) = resolve_env_value(env_var_name, &p.config_dir)?
            && !value.is_empty() {
                cfg.api_key = Some(value);
            }
    }

    Ok(cfg)
}

/// Look up an env var name in this order: process env, CWD/.env, user-config-dir/.env.
fn resolve_env_value(name: &str, config_dir: &Path) -> Result<Option<String>> {
    if let Ok(v) = std::env::var(name)
        && !v.is_empty() {
            return Ok(Some(v));
        }
    let cwd_env = std::env::current_dir().ok().map(|d| d.join(".env"));
    let user_env = config_dir.join(".env");
    for candidate in [cwd_env.as_deref(), Some(user_env.as_path())]
        .into_iter()
        .flatten()
    {
        if !candidate.exists() {
            continue;
        }
        if let Some(v) = lookup_dotenv(candidate, name)? {
            return Ok(Some(v));
        }
    }
    Ok(None)
}

fn lookup_dotenv(path: &Path, name: &str) -> Result<Option<String>> {
    let s = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let stripped = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = stripped.split_once('=') else {
            continue;
        };
        if k.trim() != name {
            continue;
        }
        return Ok(Some(unquote(v.trim()).to_string()));
    }
    Ok(None)
}

fn unquote(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &s[1..s.len() - 1];
        }
    }
    s
}

impl Config {
    pub fn require_api_key(&self) -> Result<&str> {
        self.api_key
            .as_deref()
            .filter(|k| !k.is_empty())
            .context("no API key set — set GEMINI_API_KEY or write api_key in config.toml")
    }

    pub fn api_base(&self) -> &str {
        self.api_base
            .as_deref()
            .unwrap_or("https://generativelanguage.googleapis.com")
    }

    pub fn default_chat_model(&self) -> &str {
        self.model
            .chat
            .default
            .as_deref()
            .unwrap_or("gemini-2.5-flash")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn dotenv_picks_named_key() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join(".env");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, "# comment").unwrap();
        writeln!(f, "OTHER=ignored").unwrap();
        writeln!(f, "GEMINI_API_KEY=secret123").unwrap();
        drop(f);
        let v = lookup_dotenv(&p, "GEMINI_API_KEY").unwrap();
        assert_eq!(v.as_deref(), Some("secret123"));
    }

    #[test]
    fn dotenv_handles_quotes_and_export() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join(".env");
        let mut f = std::fs::File::create(&p).unwrap();
        writeln!(f, "export FOO=\"quoted value\"").unwrap();
        writeln!(f, "BAR='single quoted'").unwrap();
        drop(f);
        assert_eq!(
            lookup_dotenv(&p, "FOO").unwrap().as_deref(),
            Some("quoted value")
        );
        assert_eq!(
            lookup_dotenv(&p, "BAR").unwrap().as_deref(),
            Some("single quoted")
        );
    }

    #[test]
    fn dotenv_missing_key_returns_none() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join(".env");
        std::fs::write(&p, "OTHER=ok\n").unwrap();
        assert!(lookup_dotenv(&p, "MISSING").unwrap().is_none());
    }
}
