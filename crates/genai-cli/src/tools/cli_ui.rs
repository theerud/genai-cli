//! Single `ToolUi` impl shared between the REPL and one-off paths. Both
//! print announcements to stderr and prompt y/N on confirmable tools, with
//! a non-TTY fallback that auto-denies.

use std::io::{self, BufRead, IsTerminal, Write};

use super::runner::{Confirmation, ToolUi};

pub struct CliToolUi;

impl ToolUi for CliToolUi {
    fn announce_call(&mut self, name: &str, summary: &str) {
        eprintln!("[tool] {summary} ({name})");
    }

    fn announce_result(&mut self, _name: &str, ok: bool, preview: &str) {
        let tag = if ok { "ok" } else { "err" };
        eprintln!("[tool/{tag}] {preview}");
    }

    fn confirm(&mut self, _name: &str, summary: &str) -> Confirmation {
        let stdin = io::stdin();
        if !stdin.is_terminal() {
            eprintln!("[tool] {summary}: auto-denied (no TTY)");
            return Confirmation::Deny;
        }
        eprint!("[tool] run `{summary}`? [y/N] ");
        let _ = io::stderr().flush();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).is_err() {
            return Confirmation::Deny;
        }
        let answer = line.trim().to_ascii_lowercase();
        if matches!(answer.as_str(), "y" | "yes") {
            Confirmation::Allow
        } else {
            Confirmation::Deny
        }
    }
}
