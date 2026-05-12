pub mod alias;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

const BUNDLED: &str = include_str!("data.toml");

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub models: Vec<ModelEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub context_window: u32,
    #[serde(default)]
    pub max_output_tokens: u32,
    #[serde(default)]
    pub input_price_per_1m: f64,
    #[serde(default)]
    pub output_price_per_1m: f64,
    #[serde(default)]
    pub supports_thinking: bool,
    #[serde(default)]
    pub thinking_levels: Vec<String>,
    #[serde(default = "default_status")]
    pub status: String,
}

fn default_status() -> String {
    "stable".to_string()
}

pub const CAP_CHAT: &str = "chat";
pub const CAP_IMAGE_OUT: &str = "image_out";
pub const CAP_TTS: &str = "tts";
pub const CAP_MUSIC_OUT: &str = "music_out";
pub const CAP_EMBED: &str = "embed";

impl ModelEntry {
    pub fn has(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }
}

impl Registry {
    pub fn load() -> Result<Self> {
        let mut reg: Registry = toml::from_str(BUNDLED).context("parsing bundled models data")?;

        if let Ok(paths) = crate::config::paths() {
            let overlay = paths.config_dir.join("models.toml");
            if overlay.exists() {
                merge_overlay(&mut reg, &overlay)?;
            }
        }
        Ok(reg)
    }

    pub fn get(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id == id)
    }

    pub fn iter_capability<'a>(&'a self, cap: &'a str) -> impl Iterator<Item = &'a ModelEntry> {
        self.models.iter().filter(move |m| m.has(cap))
    }

    /// Return up to 3 ids closest to `query` by Levenshtein distance.
    pub fn suggest(&self, query: &str) -> Vec<String> {
        let mut scored: Vec<(usize, &str)> = self
            .models
            .iter()
            .map(|m| (levenshtein(query, &m.id), m.id.as_str()))
            .collect();
        scored.sort_by_key(|(d, _)| *d);
        scored
            .into_iter()
            .take(3)
            .filter(|(d, _)| *d <= query.len().max(4) / 2 + 4)
            .map(|(_, id)| id.to_string())
            .collect()
    }
}

fn merge_overlay(reg: &mut Registry, path: &Path) -> Result<()> {
    let s = std::fs::read_to_string(path)
        .with_context(|| format!("reading overlay {}", path.display()))?;
    let overlay: Registry =
        toml::from_str(&s).with_context(|| format!("parsing overlay {}", path.display()))?;
    for m in overlay.models {
        if let Some(existing) = reg.models.iter_mut().find(|e| e.id == m.id) {
            *existing = m;
        } else {
            reg.models.push(m);
        }
    }
    Ok(())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (curr[j - 1] + 1)
                .min(prev[j] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

pub fn validate(reg: &Registry, id: &str, required_cap: &str) {
    match reg.get(id) {
        Some(entry) => {
            if !entry.has(required_cap) {
                let caps = entry.capabilities.join(", ");
                eprintln!(
                    "warning: model {id} does not advertise capability '{required_cap}' (has: {caps})"
                );
            }
        }
        None => {
            let suggestions = reg.suggest(id);
            if suggestions.is_empty() {
                eprintln!("warning: model {id} is not in the local registry");
            } else {
                eprintln!(
                    "warning: model {id} is not in the local registry. Did you mean: {}",
                    suggestions.join(", ")
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_parses() {
        let reg = Registry::load().expect("registry loads");
        assert!(!reg.models.is_empty(), "bundled registry should not be empty");
    }

    #[test]
    fn lookup_works() {
        let reg = Registry::load().unwrap();
        assert!(reg.get("gemini-2.5-flash").is_some());
        assert!(reg.get("not-a-real-model").is_none());
    }

    #[test]
    fn suggest_close_matches() {
        let reg = Registry::load().unwrap();
        let s = reg.suggest("gemini-2.5-flsh");
        assert!(s.iter().any(|id| id == "gemini-2.5-flash"), "got {:?}", s);
    }
}
