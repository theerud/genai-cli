//! Confirmation-prompt warning patterns. The confirm UI matches the
//! current tool's `describe_call` summary against this tool's pattern
//! list and prepends a `⚠` line listing the matched patterns, so the
//! user has a visible cue before answering `y`.
//!
//! User patterns under `[security.warn.<tool>]` override the built-in
//! defaults entirely for that tool. An empty list disables warnings for
//! that tool. Absent entry → built-in defaults apply.

use crate::tools::policy::glob_match;

/// Built-in defaults. Strings are globs (same `*`-only semantics as
/// policy rules). Each pattern is matched against the tool's
/// `describe_call` summary, which already contains the relevant args
/// in a human-readable form (e.g. `exec(rm -rf /tmp)`, `write_file(/x, 5 B, overwrite)`).
const DEFAULT_PATTERNS: &[(&str, &[&str])] = &[
    (
        "exec",
        &[
            "*~/.ssh*",
            "*~/.aws*",
            "*~/.gnupg*",
            "*~/.netrc*",
            "*/.env*",
            "*rm -rf /*",
            "*curl*|*sh*",
            "*wget*|*sh*",
            "*eval $(*",
            "*dd if=*",
            "*chmod 777*",
            "*sudo*",
        ],
    ),
    (
        "write_file",
        &[
            "*~/.ssh*",
            "*~/.aws*",
            "*~/.gnupg*",
            "*~/.netrc*",
            "*authorized_keys*",
        ],
    ),
];

/// Resolve the active pattern list for `tool`: user config wins if any
/// entry exists, otherwise built-in defaults.
pub fn patterns_for<'a>(cfg: &'a crate::config::Config, tool: &str) -> Vec<&'a str> {
    if let Some(custom) = cfg.security.warn.get(tool) {
        return custom.iter().map(String::as_str).collect();
    }
    DEFAULT_PATTERNS
        .iter()
        .find(|(t, _)| *t == tool)
        .map(|(_, p)| p.to_vec())
        .unwrap_or_default()
}

/// Return the matched patterns (if any) for the given `summary`.
/// Callers display the result above the confirmation prompt.
pub fn matches(cfg: &crate::config::Config, tool: &str, summary: &str) -> Vec<String> {
    patterns_for(cfg, tool)
        .into_iter()
        .filter(|p| glob_match(p, summary))
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn cfg_with_warn(tool: &str, patterns: Vec<&str>) -> Config {
        let mut cfg = Config::default();
        cfg.security.warn.insert(
            tool.to_string(),
            patterns.into_iter().map(String::from).collect(),
        );
        cfg
    }

    #[test]
    fn defaults_apply_when_absent() {
        let cfg = Config::default();
        let m = matches(&cfg, "exec", "exec(rm -rf /tmp/x)");
        assert!(!m.is_empty(), "default rm -rf pattern should fire");
    }

    #[test]
    fn user_overrides_defaults() {
        // Empty user list = no warnings for this tool.
        let cfg = cfg_with_warn("exec", vec![]);
        let m = matches(&cfg, "exec", "exec(rm -rf /tmp/x)");
        assert!(m.is_empty());
    }

    #[test]
    fn user_custom_pattern_fires() {
        let cfg = cfg_with_warn("exec", vec!["*git push --force*"]);
        let m = matches(&cfg, "exec", "exec(git push --force origin main)");
        assert_eq!(m.len(), 1);
        assert!(m[0].contains("git push --force"));
    }

    #[test]
    fn unknown_tool_has_no_defaults() {
        let cfg = Config::default();
        let m = matches(&cfg, "made_up", "anything goes");
        assert!(m.is_empty());
    }

    #[test]
    fn write_file_default_catches_ssh_path() {
        let cfg = Config::default();
        let m = matches(&cfg, "write_file", "write_file(~/.ssh/authorized_keys, 100 B, append)");
        assert!(!m.is_empty());
    }
}
