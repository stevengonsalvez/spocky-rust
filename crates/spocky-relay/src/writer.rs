//! `PaseoRelay.Delivery.Writer`: the per-destination process that serializes frames to one
//! socket, grants one reservation at a time, queues control frames, and sheds a destination that
//! does not acknowledge in time.
//!
//! The BEAM process is a `GenServer`; this is the same state machine with its inputs explicit.
//! Calls carry the caller (its process and the call it waits on), time is an argument, and what
//! the process does to the world (replies, messages to the destination, timers, monitors,
//! metrics, stopping) is queued as [`Effect`]s and drained with [`Writer::take_effects`].

use std::collections::VecDeque;

/// An Erlang process identifier.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Pid(pub u64);

/// A pending `GenServer.call`: the `from` a reply goes to.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Call(pub u64);

/// A reservation token (`reference()`).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Token(pub u64);

/// The reference of a frame sent to the destination.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Reference(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Opcode {
    Text,
    Binary,
}

/// The `{:error, reason}` of a reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriterError {
    Timeout,
    InvalidReservation,
    DestinationClosed,
    SourceClosed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reply {
    Reserved(Token),
    Ok,
    Error(WriterError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Metric {
    FramesForwarded,
    BytesForwarded(u64),
    DeliveryTimeouts,
    SlowConsumerDisconnects,
}

/// What the process does besides changing its state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    /// `GenServer.reply(from, reply)`, or the reply of the callback.
    Reply {
        call: Call,
        reply: Reply,
    },
    /// `Process.send_after(self(), {:reservation_timeout, token}, ms)`.
    StartTimer {
        token: Token,
        ms: i64,
    },
    /// `Process.cancel_timer(timer)`.
    CancelTimer(Token),
    /// `Process.monitor(source)`.
    Monitor(Pid),
    /// `Process.demonitor(ref, [:flush])`.
    Demonitor(Pid),
    /// `send(destination, {:relay_frame, self(), reference, opcode, payload})`, followed by
    /// `{:relay_write_barrier, self(), reference}`.
    Frame {
        reference: Reference,
        opcode: Opcode,
        payload: Vec<u8>,
    },
    /// `send(destination, {:relay_close, code, reason})`.
    Close {
        code: u16,
        reason: &'static str,
    },
    Metric(Metric),
    /// The process stops with reason `:normal`.
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Payload,
    Control,
}

#[derive(Clone, Debug)]
struct Active {
    kind: Kind,
    token: Token,
    source: Option<Pid>,
    write: Option<Call>,
    write_reference: Option<Reference>,
}

#[derive(Clone, Debug)]
enum Entry {
    Payload {
        from: (Pid, Call),
        bytes: u64,
        deadline: i64,
    },
    Control {
        payload: Vec<u8>,
        bytes: u64,
        deadline: i64,
    },
}

#[derive(Debug)]
pub struct Writer {
    delivery_timeout_ms: i64,
    control_queue_bytes: u64,
    active: Option<Active>,
    queued: VecDeque<Entry>,
    queued_control_bytes: u64,
    next_reference: u64,
    stopped: bool,
    effects: Vec<Effect>,
}

/// `Deadline.remaining/1`.
#[must_use]
pub fn remaining(deadline: i64, now: i64) -> i64 {
    (deadline - now).max(0)
}

impl Writer {
    /// `init/1`.
    #[must_use]
    pub const fn new(delivery_timeout_ms: i64, control_queue_bytes: u64) -> Self {
        Self {
            delivery_timeout_ms,
            control_queue_bytes,
            active: None,
            queued: VecDeque::new(),
            queued_control_bytes: 0,
            next_reference: 0,
            stopped: false,
            effects: Vec::new(),
        }
    }

    /// Whether the process has stopped; calls to it exit and the client reports
    /// `{:error, :destination_closed}`.
    #[must_use]
    pub const fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// The effects queued since the last call, oldest first.
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    fn make_ref(&mut self) -> u64 {
        self.next_reference += 1;
        self.next_reference
    }

    /// `handle_call({:reserve, byte_count, deadline}, from, state)`.
    pub fn reserve(&mut self, from: (Pid, Call), bytes: u64, deadline: i64, now: i64) {
        if self.stopped {
            return;
        }
        if self.active.is_none() {
            match remaining(deadline, now) {
                0 => self.reply(from.1, Reply::Error(WriterError::Timeout)),
                timeout => self.grant(from, timeout),
            }
        } else {
            self.queued.push_back(Entry::Payload {
                from,
                bytes,
                deadline,
            });
        }
    }

    /// `handle_call({:write, token, opcode, payload}, from, state)`.
    pub fn write(&mut self, from: Call, token: Token, opcode: Opcode, payload: Vec<u8>) {
        if self.stopped {
            return;
        }
        if self
            .active
            .as_ref()
            .is_none_or(|active| active.token != token)
        {
            self.reply(from, Reply::Error(WriterError::InvalidReservation));
            return;
        }
        let reference = Reference(self.make_ref());
        self.effects.push(Effect::Metric(Metric::FramesForwarded));
        self.effects
            .push(Effect::Metric(Metric::BytesForwarded(payload.len() as u64)));
        self.effects.push(Effect::Frame {
            reference,
            opcode,
            payload,
        });
        if let Some(active) = self.active.as_mut() {
            active.write = Some(from);
            active.write_reference = Some(reference);
        }
    }

    /// `handle_call({:control, payload}, _from, state)`.
    pub fn control(&mut self, from: Call, payload: Vec<u8>, now: i64) {
        if self.stopped {
            return;
        }
        let deadline = now + self.delivery_timeout_ms;
        let bytes = payload.len() as u64;
        if self.active.is_none() {
            if self.start_control(&payload, deadline, now) {
                self.reply(from, Reply::Ok);
            } else {
                self.reply(from, Reply::Error(WriterError::Timeout));
                self.stop();
            }
        } else if self.queued_control_bytes + bytes <= self.control_queue_bytes {
            self.queued.push_back(Entry::Control {
                payload,
                bytes,
                deadline,
            });
            self.queued_control_bytes += bytes;
            self.reply(from, Reply::Ok);
        } else {
            self.shed_slow_consumer();
            self.reject_all(Reply::Error(WriterError::Timeout));
            self.reply(from, Reply::Error(WriterError::Timeout));
            self.stop();
        }
    }

    /// `handle_cast({:close, code, reason}, state)`.
    pub fn close(&mut self, code: u16, reason: &'static str) {
        if self.stopped {
            return;
        }
        self.effects.push(Effect::Close { code, reason });
        self.reject_all(Reply::Error(WriterError::DestinationClosed));
        self.stop();
    }

    /// `handle_info({:written, reference}, state)`: the destination wrote a frame.
    pub fn written(&mut self, reference: Reference, now: i64) {
        if self.stopped {
            return;
        }
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.write_reference == Some(reference))
        {
            let expired = self.complete_active(Reply::Ok, now);
            if expired {
                self.stop();
            }
        }
    }

    /// `handle_info({:reservation_timeout, token}, state)`.
    pub fn reservation_timeout(&mut self, token: Token) {
        if self.stopped
            || self
                .active
                .as_ref()
                .is_none_or(|active| active.token != token)
        {
            return;
        }
        self.effects.push(Effect::Metric(Metric::DeliveryTimeouts));
        self.shed_slow_consumer();
        self.reject_all(Reply::Error(WriterError::Timeout));
        self.stop();
    }

    /// `handle_info({:DOWN, ...}, state)` for the destination.
    pub fn destination_down(&mut self) {
        if self.stopped {
            return;
        }
        self.reject_all(Reply::Error(WriterError::DestinationClosed));
        self.stop();
    }

    /// `handle_info({:DOWN, ...}, state)` for the source of the active reservation.
    pub fn source_down(&mut self, source: Pid, now: i64) {
        if self.stopped {
            return;
        }
        let Some(active) = self.active.as_ref() else {
            return;
        };
        if active.source != Some(source) {
            return;
        }
        if active.write_reference.is_some() {
            self.effects.push(Effect::Close {
                code: 1013,
                reason: "Delivery unavailable",
            });
            self.reject_all(Reply::Error(WriterError::SourceClosed));
            self.stop();
        } else if self.complete_active(Reply::Error(WriterError::SourceClosed), now) {
            self.stop();
        }
    }

    /// The active reservation's token, the queue of `(payload sources, control)` entries and the
    /// queued control bytes, for inspection.
    #[must_use]
    pub fn inspect(&self) -> Inspect {
        Inspect {
            active: self.active.as_ref().map(|active| match active.kind {
                Kind::Payload => ActiveKind::Payload(active.token),
                Kind::Control => ActiveKind::Control,
            }),
            queued: self
                .queued
                .iter()
                .map(|entry| match entry {
                    Entry::Payload { from, bytes, .. } => Queued::Payload(from.0, *bytes),
                    Entry::Control { bytes, .. } => Queued::Control(*bytes),
                })
                .collect(),
            queued_control_bytes: self.queued_control_bytes,
        }
    }

    fn reply(&mut self, call: Call, reply: Reply) {
        self.effects.push(Effect::Reply { call, reply });
    }

    fn stop(&mut self) {
        self.stopped = true;
        self.effects.push(Effect::Stopped);
    }

    fn shed_slow_consumer(&mut self) {
        self.effects
            .push(Effect::Metric(Metric::SlowConsumerDisconnects));
        self.effects.push(Effect::Close {
            code: 1013,
            reason: "Slow consumer",
        });
    }

    fn grant(&mut self, from: (Pid, Call), timeout: i64) {
        let token = Token(self.make_ref());
        self.effects.push(Effect::StartTimer { token, ms: timeout });
        self.effects.push(Effect::Monitor(from.0));
        self.active = Some(Active {
            kind: Kind::Payload,
            token,
            source: Some(from.0),
            write: None,
            write_reference: None,
        });
        self.reply(from.1, Reply::Reserved(token));
    }

    /// `complete_active/2` then `grant_next/1`. Returns whether the process must stop
    /// (`{:expired, state}`).
    fn complete_active(&mut self, result: Reply, now: i64) -> bool {
        if let Some(active) = self.active.take() {
            self.effects.push(Effect::CancelTimer(active.token));
            if let Some(source) = active.source {
                self.effects.push(Effect::Demonitor(source));
            }
            if let Some(write) = active.write {
                self.reply(write, result);
            }
        }
        self.grant_next(now)
    }

    fn grant_next(&mut self, now: i64) -> bool {
        loop {
            match self.queued.pop_front() {
                None => return false,
                Some(Entry::Payload { from, deadline, .. }) => match remaining(deadline, now) {
                    0 => self.reply(from.1, Reply::Error(WriterError::Timeout)),
                    timeout => {
                        self.grant(from, timeout);
                        return false;
                    }
                },
                Some(Entry::Control {
                    payload,
                    bytes,
                    deadline,
                }) => {
                    self.queued_control_bytes -= bytes;
                    return !self.start_control(&payload, deadline, now);
                }
            }
        }
    }

    /// `start_control/3`. Returns `false` when the deadline had passed: the destination was
    /// shed and everything rejected.
    fn start_control(&mut self, payload: &[u8], deadline: i64, now: i64) -> bool {
        match remaining(deadline, now) {
            0 => {
                self.effects.push(Effect::Metric(Metric::DeliveryTimeouts));
                self.shed_slow_consumer();
                self.reject_all(Reply::Error(WriterError::Timeout));
                false
            }
            timeout => {
                let reference = Reference(self.make_ref());
                let token = Token(reference.0);
                self.effects.push(Effect::StartTimer { token, ms: timeout });
                self.effects.push(Effect::Metric(Metric::FramesForwarded));
                self.effects
                    .push(Effect::Metric(Metric::BytesForwarded(payload.len() as u64)));
                self.effects.push(Effect::Frame {
                    reference,
                    opcode: Opcode::Text,
                    payload: payload.to_vec(),
                });
                self.active = Some(Active {
                    kind: Kind::Control,
                    token,
                    source: None,
                    write: None,
                    write_reference: Some(reference),
                });
                true
            }
        }
    }

    fn reject_all(&mut self, result: Reply) {
        if let Some(active) = self.active.take() {
            self.effects.push(Effect::CancelTimer(active.token));
            if let Some(source) = active.source {
                self.effects.push(Effect::Demonitor(source));
            }
            if let Some(write) = active.write {
                self.reply(write, result);
            }
        }
        for entry in std::mem::take(&mut self.queued) {
            if let Entry::Payload { from, .. } = entry {
                self.reply(from.1, result);
            }
        }
        self.queued_control_bytes = 0;
    }
}

/// The part of the state the differential prints.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Inspect {
    pub active: Option<ActiveKind>,
    pub queued: Vec<Queued>,
    pub queued_control_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveKind {
    Payload(Token),
    Control,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Queued {
    Payload(Pid, u64),
    Control(u64),
}
