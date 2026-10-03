//! Per-client terminal output streams with snapshot catch-up and
//! backpressure, following the stream half of pinned
//! `packages/server/src/terminal/terminal-session-controller.ts`
//! (`bindActiveStream`, `trySendSnapshot`, `sendSnapshot`, the snapshot
//! emitters, `replayTerminalOutputAfterSnapshot`, `detachStream`,
//! `completeStream`, `detachRegistration`, `allocateSlot`).
//!
//! The baseline is promise code; this is the same machine with the awaits
//! made explicit. Everything outside the machine (the terminal manager, the
//! owned subscription, timers, the microtask queue) is the [`StreamHost`]:
//!
//! - `request_snapshot` starts `terminalManager.getTerminalState`; the host
//!   answers with [`TerminalStreams::snapshot_result`] when it settles.
//! - `defer` is `await` on an already settled value (a live restore): the host
//!   calls [`TerminalStreams::resume`] on the next microtask.
//! - `schedule_timer` and `clear_timer` carry the coalescer's trailing timer;
//!   the host calls [`TerminalStreams::fire_timer`] when it elapses.
//! - `release` is `owner.release()`: the host stops delivering that owner's
//!   emits. A release that starts outside the machine (a disconnect) is
//!   reported with [`TerminalStreams::release_registration`].

use std::collections::BTreeMap;

use spocky_wire::{TerminalOpcode, encode_terminal_frame};

use crate::output_coalescer::{CoalescerFlush, Handled, TerminalOutputCoalescer};
use crate::restore::{
    MAX_CLIENT_BUFFERED_BYTES, MAX_TERMINAL_OUTPUT_FRAME_BYTES, RestoreOptions, RestoreSnapshot,
    SnapshotMode, SnapshotOptions, encode_legacy_snapshot_frame, encode_restore_frame,
    restore_after_output_overflow, restore_snapshot_options, subscription_snapshot_mode,
};
use crate::session::{ServerMessage, StateSnapshot};

/// `MAX_TERMINAL_STREAM_SLOTS`.
pub const MAX_TERMINAL_STREAM_SLOTS: usize = 256;

/// Everything a stream needs from outside.
pub trait StreamHost {
    /// `owner.emitBinary(frame)`.
    fn emit_binary(&mut self, slot: u8, frame: Vec<u8>);
    /// `owner.emit({ type: "terminal_stream_exit", payload })`.
    fn emit_stream_exit(&mut self, slot: u8, terminal_id: &str, error: Option<&str>);
    /// `owner.release()`: the owner stops delivering emits from now on. The
    /// stream's own cleanup (`detachRegistration`) runs right after, as the
    /// owner's `stop` does synchronously.
    fn release(&mut self, slot: u8);
    /// `terminalManager.getTerminalState(terminalId, options)`.
    fn request_snapshot(&mut self, slot: u8, terminal_id: &str, options: &SnapshotOptions);
    /// `terminal.getReplayPreamble()`.
    fn replay_preamble(&mut self, terminal_id: &str) -> String;
    /// `getClientBufferedAmount(owner.source)`; `None` is a transport with no
    /// backpressure signal.
    fn client_buffered_amount(&mut self, slot: u8) -> Option<u64>;
    /// `clientSupportsWrapReflow(owner.source)`.
    fn supports_wrap_reflow(&mut self, slot: u8) -> bool;
    /// `terminalManager.getTerminal(terminalId)` is defined.
    fn terminal_exists(&mut self, terminal_id: &str) -> bool;
    /// Arms the trailing timer of the stream's coalescer.
    fn schedule_timer(&mut self, slot: u8, token: u64, delay_ms: f64);
    /// `clearTimeout` of that timer.
    fn clear_timer(&mut self, slot: u8);
    /// Continue [`TerminalStreams::resume`] on the next microtask.
    fn defer(&mut self, slot: u8);
    /// `unsubscribe()` of the terminal subscription `bind` called for.
    fn terminal_unsubscribe(&mut self, slot: u8);
    /// The clock the coalescer reads, in milliseconds.
    fn now(&mut self) -> f64;
}

/// `bindActiveStream`'s result: the slot and the snapshot mode the caller
/// subscribes to the terminal with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bound {
    pub slot: u8,
    pub snapshot_mode: SnapshotMode,
}

#[derive(Debug)]
struct BufferedOutput {
    data: String,
    revision: Option<u64>,
}

/// What `emitLegacySnapshot` and `emitRestoreSnapshot` returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Emitted {
    /// `shouldContinue: false`.
    Stop,
    /// `shouldContinue: true` with its `replayRevision` (none for a live
    /// restore).
    Continue(Option<u64>),
}

/// What a suspended snapshot task is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Idle,
    /// `getTerminalState` for a legacy snapshot.
    Legacy,
    /// `getTerminalState` for a restore snapshot.
    Restore,
    /// A live restore: no read, one microtask.
    Live,
}

/// One flag per baseline `ActiveTerminalStream` field.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
struct ActiveStream {
    terminal_id: String,
    snapshot_output: Option<Vec<u8>>,
    exiting: bool,
    needs_snapshot: bool,
    snapshot_in_flight: bool,
    pending: Pending,
    /// `completeStream` is awaiting the snapshot task.
    complete_waiting: bool,
    retry_snapshot_errors: bool,
    ready_revision: Option<u64>,
    restore: Option<RestoreOptions>,
    buffered_outputs: Vec<BufferedOutput>,
    output_bytes_since_snapshot: usize,
    coalescer: TerminalOutputCoalescer,
    timer_token: Option<u64>,
}

/// The active streams of one session.
#[derive(Debug, Default)]
pub struct TerminalStreams {
    streams: BTreeMap<u8, ActiveStream>,
    next_slot: usize,
}

/// `Buffer.toString("utf8")` of bytes that came from a string.
fn utf8_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

impl TerminalStreams {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of active streams (`activeStreams.size`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.streams.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }

    /// Whether `slot` is an active stream.
    #[must_use]
    pub fn contains(&self, slot: u8) -> bool {
        self.streams.contains_key(&slot)
    }

    /// The terminal a slot streams.
    #[must_use]
    pub fn terminal_of(&self, slot: u8) -> Option<&str> {
        self.streams
            .get(&slot)
            .map(|stream| stream.terminal_id.as_str())
    }

    /// `allocateSlot`.
    fn allocate_slot(&mut self) -> Option<u8> {
        for attempt in 0..MAX_TERMINAL_STREAM_SLOTS {
            let slot = (self.next_slot + attempt) % MAX_TERMINAL_STREAM_SLOTS;
            let slot = u8::try_from(slot).ok()?;
            if self.streams.contains_key(&slot) {
                continue;
            }
            self.next_slot = (usize::from(slot) + 1) % MAX_TERMINAL_STREAM_SLOTS;
            return Some(slot);
        }
        None
    }

    /// `bindActiveStream` after its `hasBinaryChannel` check: a new stream
    /// for `terminal_id`, or `None` when all 256 slots are taken. The caller
    /// subscribes to the terminal with [`Bound::snapshot_mode`] and feeds its
    /// messages to [`Self::terminal_message`].
    pub fn bind(
        &mut self,
        terminal_id: &str,
        restore: Option<RestoreOptions>,
        retry_snapshot_errors: bool,
    ) -> Option<Bound> {
        let slot = self.allocate_slot()?;
        self.streams.insert(
            slot,
            ActiveStream {
                terminal_id: terminal_id.to_owned(),
                snapshot_output: None,
                exiting: false,
                needs_snapshot: true,
                snapshot_in_flight: false,
                pending: Pending::Idle,
                complete_waiting: false,
                retry_snapshot_errors,
                ready_revision: None,
                restore,
                buffered_outputs: Vec::new(),
                output_bytes_since_snapshot: 0,
                coalescer: TerminalOutputCoalescer::default(),
                timer_token: None,
            },
        );
        Some(Bound {
            slot,
            snapshot_mode: subscription_snapshot_mode(restore.as_ref()),
        })
    }

    /// The terminal subscription callback of `bindActiveStream`.
    pub fn terminal_message(
        &mut self,
        host: &mut dyn StreamHost,
        slot: u8,
        message: ServerMessage,
    ) {
        let Some(stream) = self.streams.get_mut(&slot) else {
            return;
        };
        match message {
            ServerMessage::Snapshot { revision, .. }
            | ServerMessage::SnapshotReady { revision, .. } => {
                stream.ready_revision = Some(revision);
                self.flush_coalescer(host, slot);
                if let Some(stream) = self.streams.get_mut(&slot) {
                    stream.needs_snapshot = true;
                }
                self.try_send_snapshot(host, slot);
            }
            ServerMessage::TitleChange { .. } => {}
            ServerMessage::Output { data, revision } => {
                if data.is_empty() {
                    return;
                }
                if stream.needs_snapshot || stream.snapshot_in_flight {
                    stream.buffered_outputs.push(BufferedOutput {
                        data,
                        revision: Some(revision),
                    });
                    return;
                }
                self.coalescer_handle(host, slot, &data);
            }
        }
    }

    /// The trailing timer armed by `schedule_timer` elapsed.
    pub fn fire_timer(&mut self, host: &mut dyn StreamHost, slot: u8, token: u64) {
        let now = host.now();
        let Some(stream) = self.streams.get_mut(&slot) else {
            return;
        };
        if stream.timer_token == Some(token) {
            stream.timer_token = None;
        }
        let flush = stream.coalescer.fire(token, now);
        if let Some(flush) = flush {
            self.on_flush(host, slot, flush);
        }
    }

    fn coalescer_handle(&mut self, host: &mut dyn StreamHost, slot: u8, data: &str) {
        let now = host.now();
        let Some(stream) = self.streams.get_mut(&slot) else {
            return;
        };
        match stream.coalescer.handle(data, now) {
            Handled::Flushed(flush) => self.on_flush(host, slot, flush),
            Handled::Scheduled(timer) => {
                stream.timer_token = Some(timer.token);
                host.schedule_timer(slot, timer.token, timer.delay_ms);
            }
            Handled::Buffered | Handled::Ignored => {}
        }
    }

    /// `outputCoalescer.flush()`.
    fn flush_coalescer(&mut self, host: &mut dyn StreamHost, slot: u8) {
        let now = host.now();
        let Some(stream) = self.streams.get_mut(&slot) else {
            return;
        };
        let (flush, cleared) = stream.coalescer.flush_clearing(now);
        if cleared {
            stream.timer_token = None;
            host.clear_timer(slot);
        }
        if let Some(flush) = flush {
            self.on_flush(host, slot, flush);
        }
    }

    /// The `onFlush` of the stream's coalescer: output frame, or the snapshot
    /// fallback when the client is far behind and backed up.
    fn on_flush(&mut self, host: &mut dyn StreamHost, slot: u8, flush: CoalescerFlush) {
        let Some(stream) = self.streams.get_mut(&slot) else {
            return;
        };
        stream.output_bytes_since_snapshot += flush.payload.len();
        // Catch up via a snapshot only when the client is BOTH far behind in
        // produced output AND actually backed up on the wire. A null reading
        // means the transport exposes no backpressure signal, so fall back at
        // the byte threshold.
        let buffered = host.client_buffered_amount(slot);
        let Some(stream) = self.streams.get_mut(&slot) else {
            return;
        };
        if !stream.exiting
            && stream.output_bytes_since_snapshot > MAX_TERMINAL_OUTPUT_FRAME_BYTES
            && buffered.is_none_or(|amount| amount > MAX_CLIENT_BUFFERED_BYTES as u64)
        {
            // The snapshot replaces this batch only after it succeeds.
            stream.snapshot_output = Some(flush.payload);
            stream.restore = restore_after_output_overflow(stream.restore);
            stream.needs_snapshot = true;
            self.try_send_snapshot(host, slot);
            return;
        }
        host.emit_binary(
            slot,
            encode_terminal_frame(TerminalOpcode::Output, slot, &flush.payload),
        );
    }

    /// `trySendSnapshot`.
    pub fn try_send_snapshot(&mut self, host: &mut dyn StreamHost, slot: u8) {
        if self
            .streams
            .get(&slot)
            .is_some_and(|stream| stream.snapshot_in_flight)
        {
            return;
        }
        self.send_snapshot(host, slot);
    }

    /// The synchronous part of `sendSnapshot`, up to its first `await`.
    fn send_snapshot(&mut self, host: &mut dyn StreamHost, slot: u8) {
        let Some(stream) = self.streams.get(&slot) else {
            return;
        };
        if !stream.needs_snapshot || stream.snapshot_in_flight {
            return;
        }
        let terminal_id = stream.terminal_id.clone();
        if !host.terminal_exists(&terminal_id) {
            self.detach_stream(host, &terminal_id, true);
            return;
        }
        let Some(stream) = self.streams.get(&slot) else {
            return;
        };
        if stream.restore.is_some() && stream.ready_revision.is_none() {
            return;
        }
        self.flush_coalescer(host, slot);
        let Some(stream) = self.streams.get_mut(&slot) else {
            return;
        };
        stream.snapshot_in_flight = true;
        let wrap = host.supports_wrap_reflow(slot);
        let Some(stream) = self.streams.get_mut(&slot) else {
            return;
        };
        let options = |scrollback_lines| SnapshotOptions {
            scrollback_lines,
            include_wrap_flags: wrap,
        };
        if let Some(restore) = stream.restore {
            match restore_snapshot_options(&restore) {
                // Live restore: nothing to read, but the await still yields.
                RestoreSnapshot::None => {
                    stream.pending = Pending::Live;
                    host.defer(slot);
                }
                RestoreSnapshot::Full => {
                    stream.pending = Pending::Restore;
                    host.request_snapshot(slot, &terminal_id, &options(None));
                }
                RestoreSnapshot::Options(bounded) => {
                    stream.pending = Pending::Restore;
                    host.request_snapshot(slot, &terminal_id, &options(bounded.scrollback_lines));
                }
            }
        } else {
            stream.pending = Pending::Legacy;
            host.request_snapshot(slot, &terminal_id, &options(None));
        }
    }

    /// A live restore's microtask: the task continues without a read.
    pub fn resume(&mut self, host: &mut dyn StreamHost, slot: u8) {
        if self
            .streams
            .get(&slot)
            .is_none_or(|stream| stream.pending != Pending::Live)
        {
            return;
        }
        self.finish_snapshot(host, slot, Ok(None), true);
    }

    /// `getTerminalState` settled: a snapshot, `None` for a terminal that is
    /// gone, or the error it rejected with.
    pub fn snapshot_result(
        &mut self,
        host: &mut dyn StreamHost,
        slot: u8,
        result: Result<Option<StateSnapshot>, String>,
    ) {
        if self
            .streams
            .get(&slot)
            .is_none_or(|stream| !matches!(stream.pending, Pending::Legacy | Pending::Restore))
        {
            return;
        }
        self.finish_snapshot(host, slot, result, false);
    }

    /// The part of `sendSnapshot` after its `await`.
    fn finish_snapshot(
        &mut self,
        host: &mut dyn StreamHost,
        slot: u8,
        result: Result<Option<StateSnapshot>, String>,
        live: bool,
    ) {
        let Some(stream) = self.streams.get_mut(&slot) else {
            return;
        };
        let pending = std::mem::replace(&mut stream.pending, Pending::Idle);
        let outcome = match result {
            Ok(snapshot) => self.emit_snapshot(host, slot, pending, snapshot, live),
            Err(message) => Err(message),
        };
        match outcome {
            Ok(Emitted::Continue(replay_revision)) => {
                self.complete_snapshot(host, slot, replay_revision);
            }
            Ok(Emitted::Stop) => {}
            Err(message) => self.snapshot_failed(host, slot, &message),
        }
        self.snapshot_task_settled(host, slot);
    }

    /// `emitLegacySnapshot` and `emitRestoreSnapshot` after the read.
    fn emit_snapshot(
        &mut self,
        host: &mut dyn StreamHost,
        slot: u8,
        pending: Pending,
        snapshot: Option<StateSnapshot>,
        live: bool,
    ) -> Result<Emitted, String> {
        if live {
            return Ok(Emitted::Continue(None));
        }
        let Some(stream) = self.streams.get(&slot) else {
            return Ok(Emitted::Stop);
        };
        let terminal_id = stream.terminal_id.clone();
        let Some(snapshot) = snapshot else {
            self.detach_stream(host, &terminal_id, true);
            return Ok(Emitted::Stop);
        };
        let frame = if pending == Pending::Restore {
            encode_restore_frame(slot, &snapshot.state)
        } else {
            encode_legacy_snapshot_frame(slot, &snapshot.state)
                .map_err(|error| error.to_string())?
        };
        host.emit_binary(slot, frame);
        // The frame went out-of-band; keep the replay that follows on the
        // coalescer's trailing path so it doesn't flush back-to-back with it.
        let now = host.now();
        if let Some(stream) = self.streams.get_mut(&slot) {
            stream.coalescer.mark_flushed(now);
        }
        Ok(Emitted::Continue(Some(snapshot.revision)))
    }

    /// The success tail of `sendSnapshot`.
    fn complete_snapshot(
        &mut self,
        host: &mut dyn StreamHost,
        slot: u8,
        replay_revision: Option<u64>,
    ) {
        let Some(stream) = self.streams.get_mut(&slot) else {
            return;
        };
        stream.snapshot_output = None;
        let terminal_id = stream.terminal_id.clone();
        self.replay_after_snapshot(host, slot, &terminal_id, replay_revision);
        if let Some(stream) = self.streams.get_mut(&slot) {
            stream.needs_snapshot = false;
            stream.output_bytes_since_snapshot = 0;
        }
    }

    /// `replayTerminalOutputAfterSnapshot`.
    fn replay_after_snapshot(
        &mut self,
        host: &mut dyn StreamHost,
        slot: u8,
        terminal_id: &str,
        replay_revision: Option<u64>,
    ) {
        let preamble = host.replay_preamble(terminal_id);
        if !preamble.is_empty() {
            self.coalescer_handle(host, slot, &preamble);
        }
        let Some(stream) = self.streams.get_mut(&slot) else {
            return;
        };
        let buffered = std::mem::take(&mut stream.buffered_outputs);
        for output in buffered {
            if let (Some(replay), Some(revision)) = (replay_revision, output.revision)
                && revision <= replay
            {
                continue;
            }
            self.coalescer_handle(host, slot, &output.data);
        }
    }

    /// The `catch` of `sendSnapshot`.
    fn snapshot_failed(&mut self, host: &mut dyn StreamHost, slot: u8, message: &str) {
        let Some(stream) = self.streams.get_mut(&slot) else {
            return;
        };
        // Natural completion owns the buffered final bytes and waits for this
        // read.
        if stream.exiting {
            return;
        }
        // COMPAT(terminalSnapshotErrors): old clients read every stream exit
        // as a PTY exit, so keep their stream attached and retry on the next
        // snapshot notification.
        if stream.retry_snapshot_errors {
            stream.needs_snapshot = true;
            return;
        }
        let terminal_id = stream.terminal_id.clone();
        let error = if message.is_empty() {
            "Unable to read terminal snapshot"
        } else {
            message
        };
        host.emit_stream_exit(slot, &terminal_id, Some(error));
        // Cleanup removes this slot synchronously; awaiting release here
        // would make the snapshot task wait for itself.
        self.release_owner(host, slot);
    }

    /// The `finally` of `sendSnapshot`, and whoever awaited the task.
    fn snapshot_task_settled(&mut self, host: &mut dyn StreamHost, slot: u8) {
        let Some(stream) = self.streams.get_mut(&slot) else {
            return;
        };
        stream.snapshot_in_flight = false;
        if std::mem::take(&mut stream.complete_waiting) {
            self.complete_stream_tail(host, slot);
        }
    }

    /// `detachStream(terminalId, { emitExit })`; returns whether any stream
    /// was detached.
    pub fn detach_stream(
        &mut self,
        host: &mut dyn StreamHost,
        terminal_id: &str,
        emit_exit: bool,
    ) -> bool {
        let mut detached = false;
        let slots: Vec<u8> = self
            .streams
            .iter()
            .filter(|(_, stream)| stream.terminal_id == terminal_id)
            .map(|(slot, _)| *slot)
            .collect();
        for slot in slots {
            let Some(stream) = self.streams.get_mut(&slot) else {
                continue;
            };
            detached = true;
            if stream.exiting {
                continue;
            }
            stream.exiting = true;
            if emit_exit {
                self.complete_stream(host, slot);
            } else {
                self.release_owner(host, slot);
            }
        }
        detached
    }

    /// `completeStream`: wait for the snapshot task, then drain, emit the
    /// stream exit, and release.
    fn complete_stream(&mut self, host: &mut dyn StreamHost, slot: u8) {
        let Some(stream) = self.streams.get_mut(&slot) else {
            return;
        };
        if stream.snapshot_in_flight {
            stream.complete_waiting = true;
            return;
        }
        self.complete_stream_tail(host, slot);
    }

    fn complete_stream_tail(&mut self, host: &mut dyn StreamHost, slot: u8) {
        let Some(stream) = self.streams.get_mut(&slot) else {
            return;
        };
        let terminal_id = stream.terminal_id.clone();
        if let Some(output) = stream.snapshot_output.take() {
            self.coalescer_handle(host, slot, &utf8_text(&output));
        }
        let buffered = self
            .streams
            .get_mut(&slot)
            .map(|stream| std::mem::take(&mut stream.buffered_outputs))
            .unwrap_or_default();
        for output in buffered {
            self.coalescer_handle(host, slot, &output.data);
        }
        // Completion must not turn this final flush into another backpressure
        // read: `exiting` is set.
        self.flush_coalescer(host, slot);
        host.emit_stream_exit(slot, &terminal_id, None);
        self.release_owner(host, slot);
    }

    /// `owner.release()`: the owner stops accepting emits, then its cleanup
    /// runs, which is `detachRegistration`.
    fn release_owner(&mut self, host: &mut dyn StreamHost, slot: u8) {
        host.release(slot);
        self.release_registration(host, slot);
    }

    /// `detachRegistration`: the owner's cleanup ran.
    pub fn release_registration(&mut self, host: &mut dyn StreamHost, slot: u8) {
        let Some(mut stream) = self.streams.remove(&slot) else {
            return;
        };
        if stream.coalescer.dispose() {
            host.clear_timer(slot);
        }
        stream.snapshot_output = None;
        stream.buffered_outputs.clear();
        host.terminal_unsubscribe(slot);
    }
}
