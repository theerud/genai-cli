//! Tab completion for the REPL.
//!
//! Activates only for dot-commands; bare chat input gets no completions. The
//! helper carries snapshots of dynamic data (sessions, roles, models, tools)
//! that the host code refreshes before each `readline` call.

use rustyline::Context;
use rustyline::Helper;
use rustyline::completion::{Completer, FilenameCompleter, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;

const DOT_COMMANDS: &[&str] = &[
    "help", "exit", "quit", "info", "clear", "model", "set", "file", "edit", "role", "session",
    "image", "tts", "music", "tools", "preview", "undo", "retry",
];

const SESSION_SUBCOMMANDS: &[&str] = &[
    "start", "save", "switch", "rename", "list", "drop", "delete", "export",
];

pub struct ReplHelper {
    files: FilenameCompleter,
    pub role_names: Vec<String>,
    pub session_labels: Vec<String>, // names + numeric IDs
    pub model_names: Vec<String>,
    pub tool_names: Vec<String>,
}

impl ReplHelper {
    pub fn new() -> Self {
        Self {
            files: FilenameCompleter::new(),
            role_names: Vec::new(),
            session_labels: Vec::new(),
            model_names: Vec::new(),
            tool_names: Vec::new(),
        }
    }
}

impl Helper for ReplHelper {}
impl Hinter for ReplHelper {
    type Hint = String;
}
impl Highlighter for ReplHelper {}
impl Validator for ReplHelper {}

impl Completer for ReplHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        // Only complete inside dot-commands; bare chat text gets no completions.
        let trimmed = line.trim_start();
        let lead = line.len() - trimmed.len();
        if !trimmed.starts_with('.') {
            return Ok((pos, Vec::new()));
        }
        // pos is over the full line; convert to an offset inside the dot body.
        if pos < lead + 1 {
            // Cursor is in the leading whitespace before the '.'.
            return Ok((pos, Vec::new()));
        }
        let body_start = lead + 1; // after the '.'
        let body_pos = pos - body_start;
        let body = &trimmed[1..];
        let head_end = body.find(char::is_whitespace).unwrap_or(body.len());

        // Completing the command head itself.
        if body_pos <= head_end {
            let prefix = &body[..body_pos];
            let candidates = filter_prefix(DOT_COMMANDS.iter().copied(), prefix);
            return Ok((body_start, pairs(candidates)));
        }

        let cmd = &body[..head_end];
        let tail_start = head_end
            + body[head_end..]
                .chars()
                .take_while(|c| c.is_whitespace())
                .map(char::len_utf8)
                .sum::<usize>();
        let tail = &body[tail_start..];
        let tail_cursor = body_pos.saturating_sub(tail_start);
        let token = current_token(tail, tail_cursor);
        let candidate_start = body_start + tail_start + token.start;

        let candidates: Vec<String> = match cmd {
            "session" => self.complete_session(tail, tail_cursor, &token.text),
            "set" => filter_prefix(
                ["temperature", "max-tokens", "thinking"],
                &token.text,
            ),
            "role" => filter_prefix(
                self.role_names
                    .iter()
                    .map(String::as_str)
                    .chain(std::iter::once("-")),
                &token.text,
            ),
            "model" => filter_prefix(
                self.model_names
                    .iter()
                    .map(String::as_str)
                    .chain(std::iter::once("-")),
                &token.text,
            ),
            "tools" => filter_prefix(
                self.tool_names
                    .iter()
                    .map(String::as_str)
                    .chain(std::iter::once("list")),
                &token.text,
            ),
            "file" | "image" | "tts" | "music" | "preview" => {
                // Defer to the filesystem completer on the current token.
                return self.files.complete(line, pos, _ctx);
            }
            _ => return Ok((pos, Vec::new())),
        };

        Ok((candidate_start, pairs(candidates)))
    }
}

impl ReplHelper {
    fn complete_session(&self, tail: &str, cursor: usize, token: &str) -> Vec<String> {
        // First argument is the subcommand; subsequent args may be a session
        // label (for switch/delete/export).
        let first_end = tail.find(char::is_whitespace).unwrap_or(tail.len());
        if cursor <= first_end {
            return filter_prefix(SESSION_SUBCOMMANDS.iter().copied(), token);
        }
        let sub = &tail[..first_end];
        match sub {
            "switch" | "delete" | "export" => {
                filter_prefix(self.session_labels.iter().map(String::as_str), token)
            }
            _ => Vec::new(),
        }
    }
}

struct Token<'a> {
    start: usize,
    text: std::borrow::Cow<'a, str>,
}

/// Find the token under the cursor inside `s` (no quoting handling — quoted
/// args don't get completion in v0).
fn current_token(s: &str, cursor: usize) -> Token<'_> {
    let cursor = cursor.min(s.len());
    let before = &s[..cursor];
    let start = before
        .rfind(char::is_whitespace)
        .map(|i| i + 1)
        .unwrap_or(0);
    Token {
        start,
        text: std::borrow::Cow::Borrowed(&s[start..cursor]),
    }
}

fn filter_prefix<'a, I>(names: I, prefix: &str) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut out: Vec<String> = names
        .into_iter()
        .filter(|n| n.starts_with(prefix))
        .map(String::from)
        .collect();
    out.sort();
    out.dedup();
    out
}

fn pairs(candidates: Vec<String>) -> Vec<Pair> {
    candidates
        .into_iter()
        .map(|c| Pair {
            display: c.clone(),
            replacement: c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn helper() -> ReplHelper {
        let mut h = ReplHelper::new();
        h.role_names = vec!["coding".into(), "research".into(), "sysadmin".into()];
        h.session_labels = vec!["notes".into(), "rust-work".into(), "#7".into()];
        h.model_names = vec![
            "gemini-2.5-flash".into(),
            "gemini-2.5-pro".into(),
            "pro-high".into(),
        ];
        h.tool_names = vec![
            "google_search".into(),
            "read_file".into(),
            "list_dir".into(),
        ];
        h
    }

    fn ctx() -> rustyline::history::DefaultHistory {
        rustyline::history::DefaultHistory::new()
    }

    fn complete(h: &ReplHelper, line: &str) -> (usize, Vec<String>) {
        let history = ctx();
        let ctx = Context::new(&history);
        let (pos, pairs) = h.complete(line, line.len(), &ctx).unwrap();
        (pos, pairs.into_iter().map(|p| p.replacement).collect())
    }

    #[test]
    fn completes_dot_command_head() {
        let h = helper();
        let (_, names) = complete(&h, ".se");
        assert!(names.contains(&"session".to_string()));
        assert!(names.contains(&"set".to_string()));
    }

    #[test]
    fn completes_session_subcommand() {
        let h = helper();
        let (_, names) = complete(&h, ".session sw");
        assert_eq!(names, vec!["switch".to_string()]);
    }

    #[test]
    fn completes_session_label_after_switch() {
        let h = helper();
        let (_, names) = complete(&h, ".session switch ");
        assert!(names.contains(&"notes".to_string()));
        assert!(names.contains(&"rust-work".to_string()));
    }

    #[test]
    fn completes_role_name() {
        let h = helper();
        let (_, names) = complete(&h, ".role re");
        assert_eq!(names, vec!["research".to_string()]);
    }

    #[test]
    fn completes_tool_name() {
        let h = helper();
        let (_, names) = complete(&h, ".tools read");
        assert_eq!(names, vec!["read_file".to_string()]);
    }

    #[test]
    fn no_completions_for_bare_chat() {
        let h = helper();
        let (_, names) = complete(&h, "hello world");
        assert!(names.is_empty());
    }
}
