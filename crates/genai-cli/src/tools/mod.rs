pub mod builtin;
pub mod local;
pub mod runner;

use crate::gemini::types::Tool;
use crate::models::Registry;

pub use builtin::{builtin_names, parse_builtin, validate_enabled_tool};
pub use local::{LocalTool, local_names, lookup_local};

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

/// Returns true if any of the enabled tools is a local (client-side) tool.
pub fn has_local(names: &[String]) -> bool {
    names.iter().any(|n| lookup_local(n).is_some())
}

/// Build the `tools` field of a request from the enabled-name list.
/// Combines server-side built-ins and a `FunctionDeclarations` aggregate for
/// any local tools.
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

/// Validate every enabled tool against the model and known tool names.
/// Emits warnings only — never blocks.
pub fn validate_all(registry: &Registry, model_id: &str, names: &[String]) {
    for n in names {
        match classify(n) {
            ToolKind::Builtin(_) => validate_enabled_tool(registry, model_id, n),
            ToolKind::Local(_) => {}
            ToolKind::Unknown => eprintln!("warning: unknown tool '{n}'"),
        }
    }
}
