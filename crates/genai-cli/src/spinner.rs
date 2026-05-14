//! Animated stderr indicator for operations that would otherwise be
//! silent — non-streaming LLM requests, tool subprocesses, image/audio
//! generation. Writes to stderr only (so stdout pipe-streaming stays
//! clean) and clears its line on drop.
//!
//! Spawned on a dedicated OS thread so it works for both sync and async
//! callers; the per-process overhead is one thread per active spinner,
//! which only matters if we ever have many concurrently. Today we have
//! at most one.

use std::io::{IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

const FRAMES: &[&str] = &[
    "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏",
];
const FRAME_INTERVAL_MS: u64 = 100;

pub struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Spinner {
    /// Start a spinner with `label` on stderr. Returns `None` when
    /// stderr isn't a TTY (piped or redirected — silent fallback).
    pub fn start(label: &str) -> Option<Self> {
        if !std::io::stderr().is_terminal() {
            return None;
        }
        let label = label.to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let handle = thread::spawn(move || run(&label, s));
        Some(Self {
            stop,
            handle: Some(handle),
        })
    }

    fn shutdown(&mut self) {
        if !self.stop.swap(true, Ordering::Relaxed)
            && let Some(h) = self.handle.take()
        {
            let _ = h.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run(label: &str, stop: Arc<AtomicBool>) {
    let mut frame = 0usize;
    while !stop.load(Ordering::Relaxed) {
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "\r\x1b[K{} {label}", FRAMES[frame % FRAMES.len()]);
        let _ = err.flush();
        drop(err);
        thread::sleep(Duration::from_millis(FRAME_INTERVAL_MS));
        frame += 1;
    }
    // Clear the line on the way out.
    let mut err = std::io::stderr().lock();
    let _ = write!(err, "\r\x1b[K");
    let _ = err.flush();
}
