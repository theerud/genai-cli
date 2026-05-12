use crate::gemini::types::Tool;
use crate::models::{
    CAP_TOOL_CODE_EXECUTION, CAP_TOOL_GOOGLE_SEARCH, CAP_TOOL_URL_CONTEXT, ModelEntry, Registry,
};

pub const TOOL_GOOGLE_SEARCH: &str = "google_search";
pub const TOOL_URL_CONTEXT: &str = "url_context";
pub const TOOL_CODE_EXECUTION: &str = "code_execution";

pub fn parse_builtin(name: &str) -> Option<Tool> {
    match name {
        TOOL_GOOGLE_SEARCH => Some(Tool::GoogleSearch {}),
        TOOL_URL_CONTEXT => Some(Tool::UrlContext {}),
        TOOL_CODE_EXECUTION => Some(Tool::CodeExecution {}),
        _ => None,
    }
}

pub fn builtin_names() -> &'static [&'static str] {
    &[TOOL_GOOGLE_SEARCH, TOOL_URL_CONTEXT, TOOL_CODE_EXECUTION]
}

pub fn validate_enabled_tool(registry: &Registry, model_id: &str, tool_name: &str) {
    let Some(model) = registry.get(model_id) else {
        eprintln!("warning: cannot validate tool '{tool_name}' for unknown model {model_id}");
        return;
    };
    let required = match tool_name {
        TOOL_GOOGLE_SEARCH => CAP_TOOL_GOOGLE_SEARCH,
        TOOL_URL_CONTEXT => CAP_TOOL_URL_CONTEXT,
        TOOL_CODE_EXECUTION => CAP_TOOL_CODE_EXECUTION,
        _ => return,
    };
    if !supports(model, required) {
        eprintln!(
            "warning: model {} does not advertise support for tool '{}'",
            model.id, tool_name
        );
    }
}

fn supports(model: &ModelEntry, capability: &str) -> bool {
    model.has(capability)
}
