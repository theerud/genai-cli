pub mod builtin;
pub mod cli_ui;
pub mod local;
pub mod process;
pub mod runner;
pub mod user;

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::gemini::types::Tool;
use crate::models::Registry;

pub use builtin::{builtin_names, parse_builtin, validate_enabled_tool};
pub use local::LocalTool;

/// Categorization of a named tool. `Unknown` is returned for names that match
/// neither the Gemini built-ins nor any registered local tool.
pub enum ToolKind {
    Builtin(Tool),
    Local(&'static dyn LocalTool),
    Unknown,
}

pub fn classify(name: &str) -> ToolKind {
    if let Some(t) = parse_builtin(name) {
        return ToolKind::Builtin(t);
    }
    if let Some(t) = lookup_local(name) {
        return ToolKind::Local(t);
    }
    ToolKind::Unknown
}

pub fn has_local(names: &[String]) -> bool {
    names.iter().any(|n| lookup_local(n).is_some())
}

pub fn build_request_tools(names: &[String]) -> Option<Vec<Tool>> {
    let mut out: Vec<Tool> = Vec::new();
    let mut decls = Vec::new();
    for name in names {
        match classify(name) {
            ToolKind::Builtin(t) => out.push(t),
            ToolKind::Local(t) => decls.push(t.declaration()),
            ToolKind::Unknown => {}
        }
    }
    if !decls.is_empty() {
        out.push(Tool::FunctionDeclarations(decls));
    }
    if out.is_empty() { None } else { Some(out) }
}

pub fn validate_all(registry: &Registry, model_id: &str, names: &[String]) {
    for n in names {
        match classify(n) {
            ToolKind::Builtin(_) => validate_enabled_tool(registry, model_id, n),
            ToolKind::Local(_) => {}
            ToolKind::Unknown => eprintln!("warning: unknown tool '{n}'"),
        }
    }
}

// ---------- Local-tool registry ----------

static LOCAL_REGISTRY: OnceLock<HashMap<String, Box<dyn LocalTool>>> = OnceLock::new();

fn registry() -> &'static HashMap<String, Box<dyn LocalTool>> {
    LOCAL_REGISTRY.get_or_init(build_registry)
}

fn build_registry() -> HashMap<String, Box<dyn LocalTool>> {
    let mut map: HashMap<String, Box<dyn LocalTool>> = HashMap::new();
    for tool in local::builtin_locals() {
        map.insert(tool.name().to_string(), tool);
    }
    let builtin_count = map.len();
    if let Ok(paths) = crate::config::paths() {
        let tools_dir = paths.config_dir.join("tools");
        let bin_dir = tools_dir.join("bin");
        for tool in user::load_dir(&tools_dir, bin_dir) {
            let name = tool.name().to_string();
            if map.contains_key(&name) {
                eprintln!(
                    "warning: user tool '{name}' shadows a built-in name; ignoring user definition"
                );
                continue;
            }
            map.insert(name, Box::new(tool));
        }
    }
    tracing::debug!(
        builtin = builtin_count,
        user = map.len() - builtin_count,
        "local tool registry loaded"
    );
    map
}

pub fn lookup_local(name: &str) -> Option<&'static dyn LocalTool> {
    registry().get(name).map(|b| &**b)
}

pub fn local_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = registry().keys().map(|s| s.as_str()).collect();
    names.sort();
    names
}
