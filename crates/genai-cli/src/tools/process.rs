//! Spawn a child process, poll until it exits or times out, capture both
//! streams with a cap. Shared by the built-in `exec` tool and every
//! user-defined tool.
//!
//! stdout and stderr are drained on dedicated reader threads while the
//! child runs. Without that, a child writing more than the OS pipe
//! buffer (~64 KB on Linux) would block on write and the parent would
//! sit in `try_wait` until the timeout — turning normal commands into
//! apparent hangs.

use anyhow::Result;
use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
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

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_handle = stdout.map(|s| spawn_reader(s, max_output));
    let stderr_handle = stderr.map(|s| spawn_reader(s, max_output));

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

    let (stdout_buf, stdout_trunc) = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_else(|| (Vec::new(), false));
    let (stderr_buf, stderr_trunc) = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_else(|| (Vec::new(), false));

    let timed_out = exit_status.is_none();
    let exit_code = exit_status.and_then(|s| s.code());
    Ok(CapturedOutput {
        exit_code,
        timed_out,
        stdout: format_output(stdout_buf, stdout_trunc),
        stderr: format_output(stderr_buf, stderr_trunc),
    })
}

/// Drain `stream` to EOF on a dedicated thread, retaining the first
/// `cap` bytes and discarding the rest. Always reads through EOF so
/// the child's pipe stays unblocked even after we've stopped keeping
/// the bytes.
fn spawn_reader<R>(mut stream: R, cap: usize) -> thread::JoinHandle<(Vec<u8>, bool)>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buf: Vec<u8> = Vec::with_capacity(cap.min(8192));
        let mut chunk = [0u8; 8192];
        let mut truncated = false;
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buf.len() < cap {
                        let room = cap - buf.len();
                        if n <= room {
                            buf.extend_from_slice(&chunk[..n]);
                        } else {
                            buf.extend_from_slice(&chunk[..room]);
                            truncated = true;
                        }
                    } else {
                        truncated = true;
                    }
                }
                Err(_) => break,
            }
        }
        (buf, truncated)
    })
}

fn format_output(buf: Vec<u8>, truncated: bool) -> String {
    let mut out = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        out.push_str("\n…[truncated]");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_output_passes_through() {
        let out = run_with_caps(
            std::process::Command::new("sh").arg("-c").arg("printf 'hello'"),
            Duration::from_secs(5),
            1024,
        )
        .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.timed_out);
        assert_eq!(out.stdout, "hello");
    }

    #[test]
    fn large_output_truncates_without_deadlock() {
        // 256 KB of output, far past the typical 64 KB pipe buffer. The
        // old implementation would deadlock here; the reader-thread
        // version drains the pipe to EOF and keeps the first `cap`
        // bytes.
        let out = run_with_caps(
            std::process::Command::new("sh")
                .arg("-c")
                .arg("yes hello | head -c 262144"),
            Duration::from_secs(5),
            128,
        )
        .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.timed_out);
        assert!(out.stdout.ends_with("…[truncated]"));
        // 128 bytes of payload + trailer.
        let kept = out.stdout.trim_end_matches("\n…[truncated]");
        assert_eq!(kept.len(), 128);
    }

    #[test]
    fn stderr_drained_independently() {
        let out = run_with_caps(
            std::process::Command::new("sh")
                .arg("-c")
                .arg("printf 'out' ; printf 'err' >&2"),
            Duration::from_secs(5),
            1024,
        )
        .unwrap();
        assert_eq!(out.stdout, "out");
        assert_eq!(out.stderr, "err");
    }
}
