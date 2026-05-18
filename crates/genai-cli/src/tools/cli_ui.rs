//! Single `ToolUi` impl shared between the REPL and one-off paths. Both
//! print announcements to stderr and prompt y/N/A on confirmable tools,
//! with a non-TTY fallback that auto-denies. The `A` answer trusts that
//! tool for the rest of the process — useful in long REPL sessions to
//! avoid confirmation fatigue, harmless in one-offs where it's effectively
//! identical to `y`.

use std::collections::HashSet;
use std::io::{self, BufRead, IsTerminal, Write};

use super::runner::{Confirmation, ToolUi};

#[derive(Default)]
pub struct CliToolUi {
    trusted: HashSet<String>,
}

impl CliToolUi {
    pub fn new() -> Self {
        Self::default()
    }

    /// Tools trusted for the rest of this process. Sorted for stable display.
    pub fn trusted_tools(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.trusted.iter().map(|s| s.as_str()).collect();
        names.sort();
        names
    }

    /// Forget all session trust. Subsequent confirmable-tool calls will
    /// prompt again.
    pub fn clear_trust(&mut self) {
        self.trusted.clear();
    }

    /// Revoke trust for a single tool. Returns true if the tool was trusted.
    pub fn revoke_trust(&mut self, name: &str) -> bool {
        self.trusted.remove(name)
    }
}

impl ToolUi for CliToolUi {
    fn announce_call(&mut self, name: &str, summary: &str) {
        eprintln!("[tool] {summary} ({name})");
    }

    fn announce_result(&mut self, _name: &str, ok: bool, preview: &str) {
        let tag = if ok { "ok" } else { "err" };
        eprintln!("[tool/{tag}] {preview}");
    }

    fn confirm(&mut self, name: &str, summary: &str) -> Confirmation {
        if self.trusted.contains(name) {
            return Confirmation::Allow;
        }
        let stdin = io::stdin();
        if !stdin.is_terminal() {
            eprintln!("[tool] {summary}: auto-denied (no TTY)");
            return Confirmation::Deny;
        }
        // Surface any matching warning patterns above the prompt so the
        // user has a visible cue before answering. Loads config inline —
        // confirm() is interactive and not in a hot path.
        let cfg = crate::config::load().unwrap_or_else(|e| {
            tracing::warn!(error = %e, "confirm: config load failed");
            crate::config::Config::default()
        });
        let warnings = super::warn::matches(&cfg, name, summary);
        if !warnings.is_empty() {
            eprintln!("[tool] ⚠ matched warning pattern(s): {}", warnings.join(", "));
        }
        eprint!("[tool] run `{summary}`? [y/N/A] ");
        let _ = io::stderr().flush();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).is_err() {
            return Confirmation::Deny;
        }
        match line.trim().to_ascii_lowercase().as_str() {
            "y" | "yes" => Confirmation::Allow,
            "a" | "always" => {
                self.trusted.insert(name.to_string());
                eprintln!("[tool] '{name}' trusted for this session");
                Confirmation::Allow
            }
            _ => Confirmation::Deny,
        }
    }

    fn continue_loop(&mut self, used: u32, max: u32) -> u32 {
        let stdin = io::stdin();
        if !stdin.is_terminal() {
            // Non-interactive: clean stop at cap.
            return 0;
        }
        eprint!(
            "[loop] reached {used}/{max} iterations. continue? \
             [c=+{max} more / N=N more / Enter=stop] "
        );
        let _ = io::stderr().flush();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).is_err() {
            return 0;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return 0;
        }
        if trimmed.eq_ignore_ascii_case("c") {
            return max;
        }
        match trimmed.parse::<u32>() {
            Ok(n) if n > 0 => n,
            _ => 0,
        }
    }
}
