//! Spawn a child process, poll until it exits or times out, capture both
//! streams with a cap. Shared by the built-in `exec` tool and every
//! user-defined tool.

use anyhow::Result;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct CapturedOutput {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Spawn `cmd` with stdout+stderr piped, wait up to `timeout`, return the
/// captured output truncated to `max_output` bytes per stream. On timeout,
/// the child is killed and `timed_out` is set.
pub fn run_with_caps(
    cmd: &mut Command,
    timeout: Duration,
    max_output: usize,
) -> Result<CapturedOutput> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    let deadline = Instant::now() + timeout;
    let exit_status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    let timed_out = exit_status.is_none();
    let exit_code = exit_status.and_then(|s| s.code());
    Ok(CapturedOutput {
        exit_code,
        timed_out,
        stdout: drain_capped(&mut child, true, max_output),
        stderr: drain_capped(&mut child, false, max_output),
    })
}

fn drain_capped(child: &mut Child, stdout: bool, cap: usize) -> String {
    use std::io::Read;
    let Some(mut stream): Option<Box<dyn Read>> = (if stdout {
        child.stdout.take().map(|s| Box::new(s) as Box<dyn Read>)
    } else {
        child.stderr.take().map(|s| Box::new(s) as Box<dyn Read>)
    }) else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf);
    let truncated = buf.len() > cap;
    if truncated {
        buf.truncate(cap);
    }
    let mut out = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        out.push_str("\n…[truncated]");
    }
    out
}
