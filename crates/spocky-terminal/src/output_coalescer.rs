//! Leading and trailing throttle for terminal output, following pinned
//! `packages/server/src/terminal/terminal-output-coalescer.ts`.
//!
//! The first chunk after an idle window flushes at once; chunks that arrive
//! within `flush_delay_ms` of the last flush accumulate behind one trailing
//! timer. The caller owns the clock and the timer: [`TerminalOutputCoalescer::handle`]
//! returns the timer to arm, and the caller passes its token back to
//! [`TerminalOutputCoalescer::fire`] when it elapses. A token whose timer was
//! cleared by a flush or dispose is ignored, as `clearTimeout` guarantees in
//! the baseline.
//!
//! `handle` takes a Rust `&str`, so it cannot carry a JavaScript lone
//! surrogate, which the baseline's `Buffer.from(data, "utf8")` would encode as
//! `EF BF BD` while counting one char. Output reaches the coalescer through
//! the utf8 decode of node-pty's `setEncoding("utf8")` (here
//! [`crate::utf8_decoder::Utf8Decoder`]), which never yields one, so that
//! input is unreachable.

use spocky_contracts::js_value::js_text_utf16;

/// `DEFAULT_FLUSH_DELAY_MS` in the baseline.
pub const DEFAULT_FLUSH_DELAY_MS: f64 = 5.0;

/// One flushed batch: the concatenated UTF-8 bytes, their byte count, and the
/// JavaScript string length (UTF-16 code units) of the chunks that formed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoalescerFlush {
    pub payload: Vec<u8>,
    pub chars: usize,
    pub bytes: usize,
}

/// A trailing timer the caller must arm for `delay_ms` and then pass back to
/// [`TerminalOutputCoalescer::fire`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimerRequest {
    pub token: u64,
    pub delay_ms: f64,
}

/// What [`TerminalOutputCoalescer::handle`] did with a chunk.
#[derive(Debug, Clone, PartialEq)]
pub enum Handled {
    /// Empty input, ignored.
    Ignored,
    /// Leading edge: flushed synchronously.
    Flushed(CoalescerFlush),
    /// Buffered behind a newly scheduled trailing timer.
    Scheduled(TimerRequest),
    /// Buffered behind the trailing timer that is already pending.
    Buffered,
}

#[derive(Debug)]
pub struct TerminalOutputCoalescer {
    flush_delay_ms: f64,
    chunks: Vec<u8>,
    bytes: usize,
    chars: usize,
    timer: Option<u64>,
    next_token: u64,
    last_flush_at: Option<f64>,
}

impl Default for TerminalOutputCoalescer {
    fn default() -> Self {
        Self::new(DEFAULT_FLUSH_DELAY_MS)
    }
}

impl TerminalOutputCoalescer {
    #[must_use]
    pub fn new(flush_delay_ms: f64) -> Self {
        Self {
            flush_delay_ms,
            chunks: Vec::new(),
            bytes: 0,
            chars: 0,
            timer: None,
            next_token: 0,
            last_flush_at: None,
        }
    }

    /// `handle(data)` at clock `now` (milliseconds).
    pub fn handle(&mut self, data: &str, now: f64) -> Handled {
        if data.is_empty() {
            return Handled::Ignored;
        }
        self.chunks.extend_from_slice(data.as_bytes());
        self.bytes += data.len();
        self.chars += js_text_utf16(data).count();

        if self.timer.is_some() {
            return Handled::Buffered;
        }
        let elapsed = self.last_flush_at.map_or(f64::INFINITY, |at| now - at);
        if elapsed >= self.flush_delay_ms {
            return match self.flush(now) {
                Some(flush) => Handled::Flushed(flush),
                None => Handled::Buffered,
            };
        }
        self.next_token += 1;
        self.timer = Some(self.next_token);
        Handled::Scheduled(TimerRequest {
            token: self.next_token,
            delay_ms: self.flush_delay_ms,
        })
    }

    /// The trailing timer `token` elapsed at `now`. A stale token is ignored.
    pub fn fire(&mut self, token: u64, now: f64) -> Option<CoalescerFlush> {
        if self.timer != Some(token) {
            return None;
        }
        self.timer = None;
        self.flush(now)
    }

    /// `flush()`: clears the trailing timer and drains pending output.
    pub fn flush(&mut self, now: f64) -> Option<CoalescerFlush> {
        self.timer = None;
        if self.chunks.is_empty() {
            return None;
        }
        let flush = CoalescerFlush {
            payload: std::mem::take(&mut self.chunks),
            chars: self.chars,
            bytes: self.bytes,
        };
        self.chars = 0;
        self.bytes = 0;
        self.last_flush_at = Some(now);
        Some(flush)
    }

    /// `markFlushed()`: a frame went out of band at `now`, so the next chunk
    /// takes the trailing path.
    pub fn mark_flushed(&mut self, now: f64) {
        self.last_flush_at = Some(now);
    }

    /// `dispose()`: clears the timer and drops pending output.
    pub fn dispose(&mut self) {
        self.timer = None;
        self.chunks.clear();
        self.bytes = 0;
        self.chars = 0;
    }

    /// Whether a trailing timer is armed (the baseline's `flushTimer`).
    #[must_use]
    pub fn timer_pending(&self) -> bool {
        self.timer.is_some()
    }
}
