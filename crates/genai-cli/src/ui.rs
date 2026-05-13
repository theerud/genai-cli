//! User-facing prompt helpers. Everything reads from stdin and writes the
//! prompt to stderr so output remains pipeable. None of these functions
//! detect TTY — callers that need a TTY-only path should gate the call.

use anyhow::{Result, bail};
use std::io::{self, Write};

/// Yes/no prompt with a configurable default (returned when the user just
/// hits Enter or enters anything other than y/yes/n/no).
pub fn confirm(question: &str, default_yes: bool) -> Result<bool> {
    let suffix = if default_yes { "[Y/n]" } else { "[y/N]" };
    eprint!("{question} {suffix} ");
    let _ = io::stderr().flush();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    Ok(match buf.trim().to_ascii_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        _ => false,
    })
}

/// Read a single line, trim it, return whatever the user typed (possibly
/// empty). Use this for optional input.
pub fn read_line(label: &str) -> Result<String> {
    eprint!("{label}: ");
    let _ = io::stderr().flush();
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    Ok(buf.trim().to_string())
}

/// Like `read_line` but bails with a clear error if the user enters nothing.
pub fn read_required(label: &str) -> Result<String> {
    let v = read_line(label)?;
    if v.is_empty() {
        bail!("input is required");
    }
    Ok(v)
}

/// Read a line; if empty, fall back to `default`.
pub fn read_with_default(label: &str, default: &str) -> Result<String> {
    let v = read_line(label)?;
    Ok(if v.is_empty() { default.to_string() } else { v })
}

/// Read a secret (API key, password). Best-effort echo suppression via
/// `stty -echo` on Unix; silently visible on other platforms.
#[cfg(unix)]
pub fn read_secret(label: &str) -> Result<String> {
    use anyhow::Context;
    eprint!("{label}: ");
    let _ = io::stderr().flush();
    let echo_off = std::process::Command::new("stty")
        .arg("-echo")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let mut buf = String::new();
    let read_result = io::stdin().read_line(&mut buf);
    if echo_off {
        let _ = std::process::Command::new("stty").arg("echo").status();
        eprintln!();
    }
    read_result.context("reading secret")?;
    Ok(buf.trim().to_string())
}

#[cfg(not(unix))]
pub fn read_secret(label: &str) -> Result<String> {
    read_line(label)
}
