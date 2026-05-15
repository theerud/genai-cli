//! Tool-call policy: a single rule list that decides allow / deny /
//! prompt for any tool invocation. Replaces the per-tool flat allow /
//! deny lists we had before — every tool flows through the same matcher
//! so adding a new tool means adding a built-in default rule, not a new
//! wiring path.
//!
//! Rules are evaluated in descending `priority`; ties broken by config
//! order. The first matching rule wins. If no user rule matches, the
//! built-in floor (sensitive paths + private-network deny) is consulted
//! before falling back to the tool's `requires_confirmation()` default.

use serde_json::Value;
use std::sync::OnceLock;

use crate::config::{Decision, PolicyRule, ToolSelector};

/// Evaluation result. Anything beyond the rule's `Decision`: the rule
/// index for diagnostics / audit, plus a label suitable for surfacing
/// to the user.
#[derive(Debug, Clone)]
pub struct PolicyOutcome {
    pub decision: Decision,
    pub source: PolicySource,
}

#[derive(Debug, Clone)]
pub enum PolicySource {
    /// A user-defined rule from config.toml, identified by its index.
    User { index: usize, priority: i32 },
    /// The built-in floor (default deny for sensitive paths / private
    /// networks) — fired when no user rule matched.
    Builtin(&'static str),
    /// No user or built-in rule applied; tool's own default decides.
    Default,
}

impl PolicySource {
    pub fn label(&self) -> String {
        match self {
            PolicySource::User { index, priority } => format!("rule #{index} (priority {priority})"),
            PolicySource::Builtin(name) => format!("builtin:{name}"),
            PolicySource::Default => "default".to_string(),
        }
    }
}

/// Evaluate the policy against a tool call. `normalized_args` should
/// already have any per-tool fixups applied (path canonicalization, etc.)
/// — the policy itself doesn't know which args are paths.
pub fn evaluate(tool_name: &str, normalized_args: &Value) -> PolicyOutcome {
    let rules = sorted_rules();
    for (idx, rule) in rules.iter() {
        if matches_rule(rule, tool_name, normalized_args) {
            return PolicyOutcome {
                decision: rule.decision,
                source: PolicySource::User {
                    index: *idx,
                    priority: rule.priority,
                },
            };
        }
    }
    // Built-in floor: low-priority default-deny on sensitive paths /
    // private networks. User rules above can override.
    if let Some(label) = builtin_deny_match(tool_name, normalized_args) {
        return PolicyOutcome {
            decision: Decision::Deny,
            source: PolicySource::Builtin(label),
        };
    }
    PolicyOutcome {
        decision: Decision::Prompt, // tool's own requires_confirmation() interprets this
        source: PolicySource::Default,
    }
}

/// Sorted rule list, cached for the process lifetime. Index is the
/// position in the original config so error messages remain stable.
fn sorted_rules() -> &'static [(usize, PolicyRule)] {
    static CACHE: OnceLock<Vec<(usize, PolicyRule)>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let cfg = crate::config::load().unwrap_or_default();
        let mut rules: Vec<(usize, PolicyRule)> = cfg
            .security
            .rules
            .into_iter()
            .enumerate()
            .collect();
        // Stable sort by priority desc, preserving original order for ties.
        rules.sort_by_key(|(_, r)| std::cmp::Reverse(r.priority));
        rules
    })
}

fn matches_rule(rule: &PolicyRule, tool: &str, args: &Value) -> bool {
    if !tool_selector_matches(&rule.tool, tool) {
        return false;
    }
    let Some(arg_name) = &rule.arg else {
        // Tool-level rule with no arg pattern: tool match alone is enough.
        return rule.patterns.is_empty();
    };
    let Some(value) = args.get(arg_name).and_then(Value::as_str) else {
        return false;
    };
    if rule.patterns.is_empty() {
        // `arg = "x"` with no patterns means "any value of x".
        return true;
    }
    rule.patterns.iter().any(|p| glob_match(p, value))
}

fn tool_selector_matches(sel: &ToolSelector, tool: &str) -> bool {
    match sel {
        ToolSelector::Single(s) => glob_match(s, tool),
        ToolSelector::List(names) => names.iter().any(|n| glob_match(n, tool)),
    }
}

/// Shell-style `*` wildcard matcher. `*` matches any run of characters
/// (including empty). No `?`, no character classes. Anchored at both
/// ends — to match anywhere, surround with `*`.
pub fn glob_match(pattern: &str, value: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        // No wildcard: exact match.
        return pattern == value;
    }
    let mut cursor = value;
    // First piece must match from the start.
    let first = parts[0];
    if !cursor.starts_with(first) {
        return false;
    }
    cursor = &cursor[first.len()..];
    // Middle pieces must appear in order, anywhere.
    for piece in &parts[1..parts.len() - 1] {
        if piece.is_empty() {
            continue;
        }
        match cursor.find(piece) {
            Some(idx) => cursor = &cursor[idx + piece.len()..],
            None => return false,
        }
    }
    // Last piece must match the end (or the wildcard before it absorbs
    // the rest).
    let last = parts[parts.len() - 1];
    cursor.ends_with(last)
}

// ---------- built-in floor ----------

const BUILTIN_DENY_PATHS: &[&str] = &[
    "~/.ssh/*",
    "~/.aws/*",
    "~/.gnupg/*",
    "~/.netrc",
];

/// Hostnames / host substrings we refuse even with no user rules. IPv4
/// RFC1918 / link-local detection lives in `is_private_ipv4`.
const BUILTIN_DENY_HOSTS: &[&str] = &["localhost", "::1"];

fn builtin_deny_match(tool: &str, args: &Value) -> Option<&'static str> {
    if matches!(tool, "read_file" | "list_dir" | "write_file")
        && let Some(path) = args.get("path").and_then(Value::as_str)
    {
        let expanded = crate::output::expand_path(path);
        for p in BUILTIN_DENY_PATHS {
            let exp = crate::output::expand_path(p);
            if glob_match(&exp, &expanded) {
                return Some("sensitive-path");
            }
        }
        if let Ok(paths) = crate::config::paths()
            && let Some(env_path) = paths.config_dir.join(".env").to_str()
            && expanded == env_path
        {
            return Some("config-env");
        }
    }
    if tool == "fetch_url"
        && let Some(url) = args.get("url").and_then(Value::as_str)
    {
        let host = url_host(url).unwrap_or("").to_ascii_lowercase();
        if host.is_empty() {
            return None;
        }
        if BUILTIN_DENY_HOSTS.iter().any(|h| host == *h) {
            return Some("private-host");
        }
        if is_private_ipv4(&host) {
            return Some("private-ipv4");
        }
        // 169.254.169.254 cloud metadata is caught by is_private_ipv4
        // (169.254.0.0/16). 0.0.0.0/8 and 127.0.0.0/8 also caught there.
    }
    None
}

// URL host extraction + private-IPv4 detection. Lifted from the old
// safety.rs verbatim — same logic, same edge cases. Tests live in this
// module too so behavior stays anchored.

fn url_host(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let host_port = rest.split(['/', '?', '#']).next()?;
    let host_port = host_port.rsplit_once('@').map(|(_, h)| h).unwrap_or(host_port);
    if let Some(stripped) = host_port.strip_prefix('[') {
        return stripped.split(']').next();
    }
    Some(host_port.split_once(':').map(|(h, _)| h).unwrap_or(host_port))
}

fn is_private_ipv4(addr: &str) -> bool {
    let octets: Vec<&str> = addr.split('.').collect();
    if octets.len() != 4 {
        return false;
    }
    let parsed: Option<Vec<u8>> = octets.iter().map(|s| s.parse::<u8>().ok()).collect();
    let Some(o) = parsed else { return false };
    match o.as_slice() {
        [127, _, _, _] => true,
        [10, _, _, _] => true,
        [192, 168, _, _] => true,
        [172, b, _, _] if (16..=31).contains(b) => true,
        [169, 254, _, _] => true,
        [0, _, _, _] => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn glob_exact_no_wildcard() {
        assert!(glob_match("git status", "git status"));
        assert!(!glob_match("git status", "git stat"));
    }

    #[test]
    fn glob_prefix() {
        assert!(glob_match("git*", "git diff"));
        assert!(glob_match("git*", "git"));
        assert!(!glob_match("git*", "magit"));
    }

    #[test]
    fn glob_suffix() {
        assert!(glob_match("*push", "git push"));
        assert!(!glob_match("*push", "pushed"));
    }

    #[test]
    fn glob_anywhere() {
        assert!(glob_match("*sudo*", "rm -rf / && sudo reboot"));
        assert!(glob_match("*sudo*", "sudo ls"));
        assert!(!glob_match("*sudo*", "su do something"));
    }

    #[test]
    fn glob_multi_wildcard() {
        assert!(glob_match("curl * | sh*", "curl https://x.io/install.sh | sh"));
        assert!(!glob_match("curl * | sh*", "curl x.io"));
    }

    #[test]
    fn tool_selector_single_vs_list() {
        let s = ToolSelector::Single("exec".into());
        assert!(tool_selector_matches(&s, "exec"));
        assert!(!tool_selector_matches(&s, "read_file"));

        let s = ToolSelector::Single("*".into());
        assert!(tool_selector_matches(&s, "anything"));

        let s = ToolSelector::List(vec!["read_file".into(), "list_dir".into()]);
        assert!(tool_selector_matches(&s, "read_file"));
        assert!(tool_selector_matches(&s, "list_dir"));
        assert!(!tool_selector_matches(&s, "exec"));
    }

    #[test]
    fn matches_rule_args() {
        let rule = PolicyRule {
            tool: ToolSelector::Single("exec".into()),
            arg: Some("command".into()),
            patterns: vec!["git diff*".into(), "ls*".into()],
            decision: Decision::Allow,
            priority: 0,
        };
        assert!(matches_rule(&rule, "exec", &json!({"command": "git diff HEAD"})));
        assert!(matches_rule(&rule, "exec", &json!({"command": "ls /tmp"})));
        assert!(!matches_rule(&rule, "exec", &json!({"command": "rm -rf /"})));
        assert!(!matches_rule(&rule, "read_file", &json!({"command": "ls"})));
    }
}
