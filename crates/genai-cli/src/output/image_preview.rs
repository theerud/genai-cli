//! In-terminal image preview via the Kitty graphics or iTerm2 inline-image
//! protocol. Detection uses a query/response handshake over `/dev/tty` so
//! tmux + ssh wrappers can pass it through (with `allow-passthrough on`)
//! instead of relying on env vars that get stripped at multiplexer or
//! session boundaries.
//!
//! Failure mode: silent. If we can't probe, can't detect, or the terminal
//! doesn't support either protocol, we print nothing and the caller's flow
//! continues normally.

use anyhow::Result;
use base64::Engine;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::sync::OnceLock;
use std::time::Duration;

/// Maximum bytes of base64 payload per Kitty graphics chunk. The protocol
/// spec recommends staying under 4096; we use 4000 to leave headroom for
/// the control prefix.
const KITTY_CHUNK_BYTES: usize = 4000;

/// How long to wait for a terminal to answer a capability probe. Local
/// terminals respond within milliseconds; ssh + tmux add some headroom.
const PROBE_TIMEOUT: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Kitty,
    ITerm2,
    None,
}

/// User-facing config-side preference. `Auto` runs the live probe;
/// `Kitty` / `ITerm2` force a protocol without probing; `Off` disables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preference {
    Auto,
    Kitty,
    ITerm2,
    Off,
}

impl Preference {
    pub fn from_config(s: Option<&str>) -> Self {
        match s.unwrap_or("auto").to_ascii_lowercase().as_str() {
            "auto" | "" => Preference::Auto,
            "kitty" => Preference::Kitty,
            "iterm2" | "iterm" => Preference::ITerm2,
            "off" | "false" | "no" => Preference::Off,
            other => {
                eprintln!("warning: unknown image_preview value '{other}'; treating as 'auto'");
                Preference::Auto
            }
        }
    }
}

static DETECTED: OnceLock<Protocol> = OnceLock::new();

/// Detect once, cache for the rest of the process lifetime. Subsequent
/// calls are pointer-cheap.
pub fn detect(pref: Preference) -> Protocol {
    *DETECTED.get_or_init(|| match pref {
        Preference::Off => Protocol::None,
        Preference::Kitty => Protocol::Kitty,
        Preference::ITerm2 => Protocol::ITerm2,
        Preference::Auto => probe(),
    })
}

/// Emit `bytes` as an inline image using the detected protocol. No-op if
/// the terminal doesn't support either protocol. Errors are logged but not
/// surfaced — this is best-effort UX, not a load-bearing path.
pub fn show(pref: Preference, bytes: &[u8]) -> Result<()> {
    let proto = detect(pref);
    if proto == Protocol::None {
        return Ok(());
    }
    let cols = usize::min(50, term_cols().saturating_sub(4).max(20));
    let payload = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut stdout = std::io::stdout().lock();
    match proto {
        Protocol::Kitty => emit_kitty(&mut stdout, &payload, cols)?,
        Protocol::ITerm2 => emit_iterm2(&mut stdout, &payload, cols)?,
        Protocol::None => {}
    }
    let _ = stdout.flush();
    Ok(())
}

// ---------------- emit ----------------

fn emit_kitty<W: Write>(out: &mut W, b64: &str, cols: usize) -> Result<()> {
    let bytes = b64.as_bytes();
    let total = bytes.len();
    let mut offset = 0;
    let mut first = true;
    while offset < total {
        let end = usize::min(offset + KITTY_CHUNK_BYTES, total);
        let more = if end < total { 1 } else { 0 };
        if first {
            // First chunk carries the metadata. `q=2` tells the terminal
            // to suppress both success and error responses so they don't
            // leak onto stdin after the program exits.
            write!(out, "\x1b_Ga=T,f=100,t=d,c={cols},q=2,m={more};")?;
            first = false;
        } else {
            write!(out, "\x1b_Gm={more};")?;
        }
        out.write_all(&bytes[offset..end])?;
        write!(out, "\x1b\\")?;
        offset = end;
    }
    writeln!(out)?;
    Ok(())
}

fn emit_iterm2<W: Write>(out: &mut W, b64: &str, cols: usize) -> Result<()> {
    // OSC 1337 File= argument list, then a colon, then the base64 payload,
    // then BEL. The `:` separator matters — `;` produces silent no-op.
    write!(
        out,
        "\x1b]1337;File=inline=1;preserveAspectRatio=1;width={cols};size={}:",
        b64.len()
    )?;
    out.write_all(b64.as_bytes())?;
    writeln!(out, "\x07")?;
    Ok(())
}

// ---------------- probe ----------------

fn probe() -> Protocol {
    // /dev/tty is the controlling terminal — works even if stdout is
    // piped to a file. If we can't open it, we're definitely non-interactive.
    let Ok(tty) = OpenOptions::new().read(true).write(true).open("/dev/tty") else {
        tracing::debug!("image_preview: /dev/tty unavailable, no preview");
        return Protocol::None;
    };
    let fd = tty.as_raw_fd();
    let Some(_guard) = RawModeGuard::enter(fd) else {
        tracing::debug!("image_preview: could not enter raw mode, no preview");
        return Protocol::None;
    };

    // Probe iTerm2 first: terminals that speak both protocols (notably
    // recent iTerm2 builds with Kitty-graphics support) tend to be more
    // reliable on their own native protocol than on Kitty. Falling back
    // to Kitty afterwards covers Kitty, Ghostty, WezTerm, foot, etc.
    if probe_iterm2(fd) {
        tracing::debug!("image_preview: iterm2 protocol detected");
        return Protocol::ITerm2;
    }
    if probe_kitty(fd) {
        tracing::debug!("image_preview: kitty protocol detected");
        return Protocol::Kitty;
    }
    tracing::debug!("image_preview: no protocol detected");
    Protocol::None
}

/// Send Kitty's capability query and watch for the `\e_G…OK…\e\\` reply.
/// The probe is a 1×1 zero-data image with `q=0` (verbose), `a=q` (query
/// only, don't render). Terminals that don't speak the protocol stay silent.
fn probe_kitty(fd: i32) -> bool {
    let probe = wrap_passthrough("\x1b_Gi=31,a=q,s=1,v=1,q=0,t=d,f=24;AAAA\x1b\\");
    let response = match tty_exchange(fd, probe.as_bytes(), b"\x1b\\") {
        Some(r) => r,
        None => return false,
    };
    tracing::debug!(
        bytes = response.len(),
        sample = %String::from_utf8_lossy(&response[..response.len().min(80)]).escape_default(),
        "image_preview: kitty probe response"
    );
    // Look for a reply that specifically references our probe id (i=31)
    // and an OK. Generic `\e_G...;OK` matches can be triggered by leaked
    // emit acks from prior sessions or by terminals that ack without
    // actually rendering, so we require the id match.
    contains_subslice(&response, b"i=31") && contains_subslice(&response, b";OK")
}

/// iTerm2 doesn't have a graphics-specific probe, but it answers several
/// `OSC 1337` queries that no other terminal recognizes. ReportCellSize is
/// the canonical pick — its presence in the reply is a strong positive.
fn probe_iterm2(fd: i32) -> bool {
    let probe = wrap_passthrough("\x1b]1337;ReportCellSize\x07");
    let response = tty_exchange(fd, probe.as_bytes(), b"\x07");
    if let Some(r) = response.as_ref() {
        tracing::debug!(
            bytes = r.len(),
            sample = %String::from_utf8_lossy(&r[..r.len().min(80)]).escape_default(),
            "image_preview: iterm2 probe response"
        );
        if contains_subslice(r, b"CellSize=") {
            return true;
        }
    }
    // Fall back to the conventional env-var even if the probe didn't get
    // through — some setups (older tmux without passthrough) strip the
    // response but leave TERM_PROGRAM alone.
    let term_program = std::env::var("TERM_PROGRAM").ok();
    tracing::debug!(?term_program, "image_preview: iterm2 env fallback");
    term_program.as_deref() == Some("iTerm.app")
}

/// Wrap a control sequence with tmux/screen passthrough escapes so the
/// host multiplexer forwards it to the inner terminal. Inside tmux this
/// requires `set -g allow-passthrough on`; without that, the inner
/// terminal never sees the probe and we time out (which is the safe
/// fallback).
fn wrap_passthrough(seq: &str) -> String {
    if std::env::var_os("TMUX").is_some() {
        format!("\x1bPtmux;\x1b{seq}\x1b\\")
    } else if std::env::var_os("STY").is_some() {
        // screen
        format!("\x1bP{seq}\x1b\\")
    } else {
        seq.to_string()
    }
}

/// Write `query` to `fd`, then read until `terminator` is found or
/// PROBE_TIMEOUT elapses, returning whatever arrived.
fn tty_exchange(fd: i32, query: &[u8], terminator: &[u8]) -> Option<Vec<u8>> {
    write_all_fd(fd, query).ok()?;
    let mut buf = Vec::new();
    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    let mut chunk = [0u8; 256];
    loop {
        let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
        let ready = poll_fd(fd, remaining)?;
        if !ready {
            break;
        }
        // Safety: read(2) with fd in raw mode and VMIN=0/VTIME=0 returns
        // however many bytes are immediately available, or 0 if none.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n <= 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
        if contains_subslice(&buf, terminator) {
            break;
        }
    }
    Some(buf)
}

fn write_all_fd(fd: i32, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if n <= 0 {
            anyhow::bail!("write to tty failed");
        }
        bytes = &bytes[n as usize..];
    }
    Ok(())
}

/// `poll(2)` wrapper: returns `Some(true)` if there's data to read,
/// `Some(false)` on timeout, `None` on syscall error.
fn poll_fd(fd: i32, timeout: Duration) -> Option<bool> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    let r = unsafe { libc::poll(&mut pfd, 1, ms) };
    match r {
        -1 => None,
        0 => Some(false),
        _ => Some((pfd.revents & libc::POLLIN) != 0),
    }
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ---------------- raw mode guard ----------------

/// RAII wrapper around `tcsetattr`. Putting the tty into raw mode is fine;
/// *leaving* it that way after panic isn't. Restoration happens in `Drop`
/// even on unwind.
struct RawModeGuard {
    fd: i32,
    original: libc::termios,
}

impl RawModeGuard {
    fn enter(fd: i32) -> Option<Self> {
        // Safety: termios is a C struct of POD fields; zero-init is valid.
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            return None;
        }
        let mut raw = original;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return None;
        }
        Some(Self { fd, original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
        }
    }
}

// ---------------- helpers ----------------

fn term_cols() -> usize {
    // Use the controlling terminal's window size; fall back to 80.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let Ok(tty) = OpenOptions::new().read(true).write(true).open("/dev/tty") else {
        return 80;
    };
    let fd = tty.as_raw_fd();
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
        ws.ws_col as usize
    } else {
        80
    }
}
