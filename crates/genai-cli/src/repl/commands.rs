#[derive(Debug, Clone)]
pub enum DotCmd {
    Help,
    Exit,
    Info,
    Clear,
    Model(Option<String>),
    Set { key: String, value: String },
    File(Vec<String>),
    Edit,
    Role(Option<String>),
    Session(SessionCmd),
    Image(ActionArgs),
    Tts(ActionArgs),
    Music(ActionArgs),
    Tools(Option<String>),
    Preview(String),
    Audit(Option<usize>),
    Trust(TrustCmd),
    Undo,
    Retry,
    Unknown(String),
}

#[derive(Debug, Clone)]
pub enum TrustCmd {
    List,
    Clear,
    Drop(String),
}

#[derive(Debug, Clone)]
pub enum SessionCmd {
    Show,
    Start,
    Save { name: String },
    Switch { name: String },
    Rename { name: String },
    List,
    Drop,
    Delete { name: String },
    Export { name: String },
}

#[derive(Debug, Clone, Default)]
pub struct ActionArgs {
    pub model: Option<String>,
    pub output: Option<String>,
    pub files: Vec<String>,
    pub voice: Option<String>,
    pub prompt: String,
}

pub fn parse(line: &str) -> Option<DotCmd> {
    let line = line.trim();
    let rest = line.strip_prefix('.')?;
    let tokens = split_shellish(rest);
    let mut parts = tokens.iter().map(String::as_str);
    let head = parts.next().unwrap_or("");
    let tail: Vec<&str> = parts.collect();
    let cmd = match head {
        "help" | "h" | "?" => DotCmd::Help,
        "exit" | "quit" | "q" => DotCmd::Exit,
        "info" => DotCmd::Info,
        "clear" => DotCmd::Clear,
        "model" => DotCmd::Model(opt_first(&tail)),
        "set" => {
            if tail.len() < 2 {
                DotCmd::Unknown(".set requires <key> <value>".to_string())
            } else {
                DotCmd::Set {
                    key: tail[0].to_string(),
                    value: tail[1..].join(" "),
                }
            }
        }
        "file" => DotCmd::File(tail.iter().map(|s| s.to_string()).collect()),
        "edit" => DotCmd::Edit,
        "role" => DotCmd::Role(opt_first(&tail)),
        "session" => match parse_session_cmd(&tail) {
            Ok(s) => DotCmd::Session(s),
            Err(e) => DotCmd::Unknown(format!(".session: {e}")),
        },
        "undo" => DotCmd::Undo,
        "retry" => DotCmd::Retry,
        "image" => match parse_action_args(&tail) {
            Ok(a) => DotCmd::Image(a),
            Err(e) => DotCmd::Unknown(format!(".image: {e}")),
        },
        "tts" => match parse_action_args(&tail) {
            Ok(a) => DotCmd::Tts(a),
            Err(e) => DotCmd::Unknown(format!(".tts: {e}")),
        },
        "music" => match parse_action_args(&tail) {
            Ok(a) => DotCmd::Music(a),
            Err(e) => DotCmd::Unknown(format!(".music: {e}")),
        },
        "tools" => DotCmd::Tools(opt_first(&tail)),
        "preview" => match opt_first(&tail) {
            Some(path) => DotCmd::Preview(path),
            None => DotCmd::Unknown(".preview requires <path>".to_string()),
        },
        "audit" => {
            let n = tail.first().and_then(|s| s.parse::<usize>().ok());
            DotCmd::Audit(n)
        }
        "trust" => match tail.as_slice() {
            [] | ["list"] => DotCmd::Trust(TrustCmd::List),
            ["clear"] => DotCmd::Trust(TrustCmd::Clear),
            ["drop", name] => DotCmd::Trust(TrustCmd::Drop((*name).to_string())),
            _ => DotCmd::Unknown(".trust: expected list / clear / drop <name>".to_string()),
        },
        _ => DotCmd::Unknown(format!("unknown command: .{head}")),
    };
    Some(cmd)
}

fn split_shellish(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut escape = false;
    for ch in s.chars() {
        if escape {
            cur.push(ch);
            escape = false;
            continue;
        }
        match ch {
            '\\' => escape = true,
            '"' | '\'' => {
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(ch);
                } else {
                    cur.push(ch);
                }
            }
            c if c.is_whitespace() && quote.is_none() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn parse_session_cmd(tail: &[&str]) -> Result<SessionCmd, String> {
    match tail {
        [] => Ok(SessionCmd::Show),
        ["start"] => Ok(SessionCmd::Start),
        ["save", name] => Ok(SessionCmd::Save {
            name: (*name).to_string(),
        }),
        ["switch", name] => Ok(SessionCmd::Switch {
            name: (*name).to_string(),
        }),
        ["rename", name] => Ok(SessionCmd::Rename {
            name: (*name).to_string(),
        }),
        ["list"] => Ok(SessionCmd::List),
        ["drop"] => Ok(SessionCmd::Drop),
        ["delete", name] => Ok(SessionCmd::Delete {
            name: (*name).to_string(),
        }),
        ["export", name] => Ok(SessionCmd::Export {
            name: (*name).to_string(),
        }),
        _ => Err(
            "expected one of: start, save <name>, switch <name>, rename <name>, list, drop, delete <name>, export <name>"
                .to_string(),
        ),
    }
}

fn parse_action_args(tail: &[&str]) -> Result<ActionArgs, String> {
    let mut a = ActionArgs::default();
    let mut i = 0;
    let mut prompt_tokens: Vec<&str> = Vec::new();
    while i < tail.len() {
        match tail[i] {
            "-m" | "--model" => {
                i += 1;
                a.model = Some(tail.get(i).ok_or("missing value for -m")?.to_string());
            }
            "-o" | "--output" => {
                i += 1;
                a.output = Some(tail.get(i).ok_or("missing value for -o")?.to_string());
            }
            "-f" | "--file" => {
                i += 1;
                a.files
                    .push(tail.get(i).ok_or("missing value for -f")?.to_string());
            }
            "-v" | "--voice" => {
                i += 1;
                a.voice = Some(tail.get(i).ok_or("missing value for -v")?.to_string());
            }
            other => prompt_tokens.push(other),
        }
        i += 1;
    }
    a.prompt = strip_quotes(&prompt_tokens.join(" "));
    if a.prompt.is_empty() {
        return Err("prompt is required".to_string());
    }
    Ok(a)
}

fn strip_quotes(s: &str) -> String {
    let trimmed = s.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

fn opt_first(tail: &[&str]) -> Option<String> {
    tail.first().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_a_dot_command() {
        assert!(parse("hello world").is_none());
    }

    #[test]
    fn help_variants() {
        assert!(matches!(parse(".help"), Some(DotCmd::Help)));
        assert!(matches!(parse(".h"), Some(DotCmd::Help)));
        assert!(matches!(parse(".?"), Some(DotCmd::Help)));
    }

    #[test]
    fn model_with_arg() {
        assert!(matches!(parse(".model foo"), Some(DotCmd::Model(Some(_)))));
        assert!(matches!(parse(".model"), Some(DotCmd::Model(None))));
    }

    #[test]
    fn session_subcommands_parse() {
        assert!(matches!(parse(".session"), Some(DotCmd::Session(SessionCmd::Show))));
        assert!(matches!(parse(".session start"), Some(DotCmd::Session(SessionCmd::Start))));
        assert!(matches!(parse(".session save foo"), Some(DotCmd::Session(SessionCmd::Save { .. }))));
        assert!(matches!(parse(".session switch foo"), Some(DotCmd::Session(SessionCmd::Switch { .. }))));
        assert!(matches!(parse(".session rename foo"), Some(DotCmd::Session(SessionCmd::Rename { .. }))));
        assert!(matches!(parse(".session list"), Some(DotCmd::Session(SessionCmd::List))));
        assert!(matches!(parse(".session drop"), Some(DotCmd::Session(SessionCmd::Drop))));
    }

    #[test]
    fn preview_parses_path() {
        match parse(".preview /tmp/foo.png") {
            Some(DotCmd::Preview(p)) => assert_eq!(p, "/tmp/foo.png"),
            other => panic!("unexpected: {other:?}"),
        }
        assert!(matches!(parse(".preview"), Some(DotCmd::Unknown(_))));
    }

    #[test]
    fn tools_parse() {
        assert!(matches!(parse(".tools"), Some(DotCmd::Tools(None))));
        assert!(matches!(parse(".tools google_search"), Some(DotCmd::Tools(Some(_)))));
    }

    #[test]
    fn session_subcommand_errors() {
        assert!(matches!(parse(".session foo"), Some(DotCmd::Unknown(_))));
        assert!(matches!(parse(".session save"), Some(DotCmd::Unknown(_))));
    }

    #[test]
    fn supports_quoted_session_name() {
        match parse(".session save \"forty two\"") {
            Some(DotCmd::Session(SessionCmd::Save { name })) => assert_eq!(name, "forty two"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn set_requires_value() {
        match parse(".set temperature 0.7") {
            Some(DotCmd::Set { key, value }) => {
                assert_eq!(key, "temperature");
                assert_eq!(value, "0.7");
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(matches!(parse(".set foo"), Some(DotCmd::Unknown(_))));
    }

    #[test]
    fn image_parses_args_and_prompt() {
        match parse(".image -m imagen-4 -o out.png a cat on the moon") {
            Some(DotCmd::Image(a)) => {
                assert_eq!(a.model.as_deref(), Some("imagen-4"));
                assert_eq!(a.output.as_deref(), Some("out.png"));
                assert_eq!(a.prompt, "a cat on the moon");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn image_strips_outer_quotes() {
        match parse(r#".image "a quoted prompt""#) {
            Some(DotCmd::Image(a)) => assert_eq!(a.prompt, "a quoted prompt"),
            other => panic!("unexpected: {other:?}"),
        }
    }

}
