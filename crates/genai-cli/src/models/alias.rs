use crate::config::{AliasEntry, Config};
use crate::gemini::types::ThinkingConfig;

#[derive(Debug, Clone, Default)]
pub struct ResolvedModel {
    pub id: String,
    pub temperature: Option<f32>,
    pub thinking_level: Option<String>,
}

impl ResolvedModel {
    pub fn plain(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ..Default::default()
        }
    }
}

/// Resolve a model name through aliases. If `name` is an alias, return its
/// target model id and overlay params. Otherwise pass through unchanged.
pub fn resolve(cfg: &Config, name: &str) -> ResolvedModel {
    match cfg.aliases.get(name) {
        Some(alias) => from_alias(alias),
        None => ResolvedModel::plain(name),
    }
}

pub fn thinking_for(level: &str) -> Option<ThinkingConfig> {
    let budget = match level {
        "off" | "none" => Some(0),
        "low" => Some(1024),
        "medium" => Some(8192),
        "high" => Some(24576),
        "dynamic" | "auto" => Some(-1),
        _ => None,
    };
    budget.map(|b| ThinkingConfig {
        thinking_budget: Some(b),
    })
}

fn from_alias(a: &AliasEntry) -> ResolvedModel {
    ResolvedModel {
        id: a.model.clone(),
        temperature: a.temperature,
        thinking_level: a.thinking_level.clone(),
    }
}
