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
}
