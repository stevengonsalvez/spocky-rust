//! `writer::Writer` against the transcript the pinned `PaseoRelay.Delivery.Writer` printed for the
//! operation script in `fixtures/relay-writer-ops.txt` (see `scripts/phase4/relay-writer-baseline.exs`
//! for the operation language and the block format). The replay compares raw text: what the
//! destination received, what each source got back, and the Writer's queue and metrics.

#![allow(clippy::too_many_lines)]

use spocky_relay::writer::{
    Call, Effect, Metric, Opcode, Pid, Reference, Reply, Token, Writer, WriterError,
};
use std::collections::{BTreeMap, BTreeSet};

/// The far deadline of the baseline: 1,000,000 ms after the call.
const FAR: i64 = 1_000_000;
const PAST: i64 = -1_000;

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(path).expect("a fixture")
}

fn baseline() -> String {
    std::env::var_os("SPOCKY_RELAY_WRITER_BASELINE").map_or_else(
        || fixture("relay-writer-baseline.txt"),
        |path| std::fs::read_to_string(path).expect("the baseline file"),
    )
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Kind {
    Reserve,
    Write,
    Control,
}

impl Kind {
    const fn name(self) -> &'static str {
        match self {
            Self::Reserve => "reserve",
            Self::Write => "write",
            Self::Control => "control",
        }
    }
}

struct Pending {
    source: String,
    kind: Kind,
    raw: bool,
}

struct Machine {
    writer: Writer,
    sources: BTreeMap<String, Pid>,
    dead: BTreeSet<Pid>,
    pending: BTreeMap<Call, Pending>,
    tokens: BTreeMap<String, Token>,
    token_names: BTreeMap<Token, String>,
    frames: BTreeMap<String, Reference>,
    frame_names: BTreeMap<Reference, String>,
    counters: [u64; 3],
    metrics: [u64; 4],
    /// Live monitors (the destination, and one per granted source) and the armed reservation
    /// timer, derived from the effects the port reports.
    monitors: BTreeMap<Pid, u32>,
    timers: BTreeSet<Token>,
    next_pid: u64,
    next_call: u64,
    /// The Writer's clock reading, as milliseconds since the far deadline was set (an input).
    now: i64,
    timer_events: Vec<(Token, i64)>,
    timeout_ms: i64,
    /// References of every frame sent; a control frame's reservation timer has the same value.
    frame_refs: BTreeSet<u64>,
    dest: Vec<String>,
    replies: Vec<String>,
}

impl Machine {
    fn new(timeout: i64, control_bytes: u64) -> Self {
        Self {
            writer: Writer::new(timeout, control_bytes),
            sources: BTreeMap::new(),
            dead: BTreeSet::new(),
            pending: BTreeMap::new(),
            tokens: BTreeMap::new(),
            token_names: BTreeMap::new(),
            frames: BTreeMap::new(),
            frame_names: BTreeMap::new(),
            counters: [0; 3],
            metrics: [0; 4],
            monitors: BTreeMap::new(),
            timers: BTreeSet::new(),
            next_pid: 0,
            next_call: 0,
            now: 0,
            timer_events: Vec::new(),
            timeout_ms: timeout,
            frame_refs: BTreeSet::new(),
            dest: Vec::new(),
            replies: Vec::new(),
        }
    }

    fn source(&mut self, name: &str) -> Pid {
        if let Some(pid) = self
            .sources
            .get(name)
            .copied()
            .filter(|p| !self.dead.contains(p))
        {
            return pid;
        }
        self.next_pid += 1;
        let pid = Pid(self.next_pid);
        self.sources.insert(name.to_owned(), pid);
        pid
    }

    /// The name a pid has in the harness's source table; a process replaced under its name
    /// shows as `?`.
    fn source_name(&self, pid: Pid) -> &str {
        self.sources
            .iter()
            .find_map(|(name, current)| (*current == pid).then_some(name.as_str()))
            .unwrap_or("?")
    }

    fn busy(&self, name: &str) -> bool {
        self.pending.values().any(|call| call.source == name)
    }

    fn token(&self, name: &str) -> Token {
        self.tokens.get(name).copied().unwrap_or(Token(0))
    }

    fn call(&mut self) -> Call {
        self.next_call += 1;
        Call(self.next_call)
    }

    /// Runs the effects, including the exit message of a source that was already dead when it
    /// was monitored.
    fn drain(&mut self) {
        loop {
            let effects = self.writer.take_effects();
            if effects.is_empty() {
                break;
            }
            let mut downs = Vec::new();
            for effect in effects {
                match effect {
                    Effect::Reply { call, reply } => self.deliver(call, reply),
                    Effect::Frame {
                        reference,
                        opcode,
                        payload,
                    } => {
                        self.counters[1] += 1;
                        let name = format!("w{}", self.counters[1]);
                        self.frame_refs.insert(reference.0);
                        self.frames.insert(name.clone(), reference);
                        self.frame_names.insert(reference, name.clone());
                        let opcode = match opcode {
                            Opcode::Text => "text",
                            Opcode::Binary => "binary",
                        };
                        self.dest.push(format!(
                            "frame {name} {opcode} {} {}",
                            payload.len(),
                            hex(&payload)
                        ));
                        self.dest.push(format!("barrier {name}"));
                    }
                    Effect::Close { code, reason } => {
                        self.dest.push(format!("close {code} {reason}"));
                    }
                    Effect::Metric(Metric::FramesForwarded) => self.metrics[0] += 1,
                    Effect::Metric(Metric::BytesForwarded(bytes)) => self.metrics[1] += bytes,
                    Effect::Metric(Metric::DeliveryTimeouts) => self.metrics[2] += 1,
                    Effect::Metric(Metric::SlowConsumerDisconnects) => self.metrics[3] += 1,
                    Effect::Monitor(pid) if self.dead.contains(&pid) => {
                        // Monitoring a dead process delivers its exit message at once.
                        downs.push(pid);
                    }
                    Effect::StartTimer { token, ms } => {
                        self.timer_events.push((token, ms));
                        self.timers.insert(token);
                    }
                    Effect::CancelTimer(token) => {
                        self.timers.remove(&token);
                    }
                    Effect::Monitor(pid) => *self.monitors.entry(pid).or_insert(0) += 1,
                    Effect::Demonitor(pid) => {
                        if let Some(count) = self.monitors.get_mut(&pid) {
                            *count = count.saturating_sub(1);
                        }
                    }
                    Effect::Stopped => {}
                }
            }
            for pid in downs {
                if let Some(count) = self.monitors.get_mut(&pid) {
                    *count = count.saturating_sub(1);
                }
                self.writer.source_down(pid, self.now);
            }
        }
    }

    fn deliver(&mut self, call: Call, reply: Reply) {
        let Some(pending) = self.pending.remove(&call) else {
            return;
        };
        let kind = pending.kind.name();
        let text = match reply {
            Reply::Reserved(token) => {
                self.counters[0] += 1;
                let name = format!("t{}", self.counters[0]);
                self.tokens.insert(name.clone(), token);
                self.token_names.insert(token, name.clone());
                format!("{} {kind} ok {name}", pending.source)
            }
            Reply::Ok => format!("{} {kind} ok", pending.source),
            Reply::Error(error) => {
                format!("{} {kind} error {}", pending.source, error_name(error))
            }
        };
        self.replies.push(text);
    }

    /// After an operation: callers whose call can never be answered because the Writer is gone.
    fn finish(&mut self) {
        if self.writer.is_stopped() {
            // Callers that get an exit instead of a reply follow the replies, by name.
            let mut waiting: Vec<Pending> =
                std::mem::take(&mut self.pending).into_values().collect();
            waiting.sort_by(|a, b| a.source.cmp(&b.source));
            for pending in waiting {
                let kind = pending.kind.name();
                self.replies.push(if pending.raw {
                    format!("{} {kind} exit", pending.source)
                } else {
                    format!("{} {kind} error destination_closed", pending.source)
                });
            }
        }
    }

    fn state_line(&self) -> String {
        let (alive, active, queued, control, live) = if self.writer.is_stopped() {
            (false, "-".to_owned(), "-".to_owned(), 0, "-".to_owned())
        } else {
            let inspect = self.writer.inspect();
            let active = match inspect.active {
                None => "none".to_owned(),
                Some(spocky_relay::writer::ActiveKind::Control) => "control".to_owned(),
                Some(spocky_relay::writer::ActiveKind::Payload(token)) => {
                    format!(
                        "payload:{}",
                        self.token_names.get(&token).map_or("?", String::as_str)
                    )
                }
            };
            let queued = inspect
                .queued
                .iter()
                .map(|entry| match entry {
                    spocky_relay::writer::Queued::Payload(pid, bytes) => {
                        format!("p:{}:{bytes}", self.source_name(*pid))
                    }
                    spocky_relay::writer::Queued::Control(bytes) => format!("c:{bytes}"),
                })
                .collect::<Vec<_>>()
                .join(",");
            let monitors: u32 = 1 + self.monitors.values().sum::<u32>();
            let live = format!("{monitors},{}", self.timers.len());
            (true, active, queued, inspect.queued_control_bytes, live)
        };
        format!(
            "state alive={alive} active={active} queued={queued} control={control} live={live} \
             metrics={},{},{},{}",
            self.metrics[0], self.metrics[1], self.metrics[2], self.metrics[3]
        )
    }

    fn block(&mut self, op: &str) -> String {
        let mut lines = vec![format!("> {op}")];
        // Reservation timers of payload grants, by token name. A control frame's timer depends on
        // two clock reads inside the Writer and is not printed.
        let named: Vec<(String, i64)> = self
            .timer_events
            .iter()
            .filter_map(|(token, ms)| Some((self.token_names.get(token)?.clone(), *ms)))
            .collect();
        // A control frame's timer is `delivery_timeout_ms` minus the time between two clock reads
        // in the Writer, which the capture cannot reproduce: it must lie in the last 100 ms.
        for (token, ms) in &self.timer_events {
            if self.frame_refs.contains(&token.0) {
                assert!(
                    *ms > self.timeout_ms - 100 && *ms <= self.timeout_ms,
                    "control timer {ms} outside ({} - 100, {}]",
                    self.timeout_ms,
                    self.timeout_ms
                );
            }
        }
        self.timer_events.clear();
        if let Some((_, ms)) = named.first() {
            lines.push(format!("~ elapsed={}", FAR - ms));
        }
        if !self.dest.is_empty() {
            lines.push(format!("d {}", self.dest.join(" | ")));
        }
        if !self.replies.is_empty() {
            lines.push(format!("r {}", self.replies.join(" | ")));
        }
        lines.extend(named.iter().map(|(name, ms)| format!("t {name} {ms}")));
        self.dest.clear();
        self.replies.clear();
        lines.push(self.state_line());
        lines.join("\n")
    }
}

fn error_name(error: WriterError) -> &'static str {
    match error {
        WriterError::Timeout => "timeout",
        WriterError::InvalidReservation => "invalid_reservation",
        WriterError::DestinationClosed => "destination_closed",
        WriterError::SourceClosed => "source_closed",
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut out, byte| {
        write!(out, "{byte:02x}").unwrap();
        out
    })
}

/// Byte `i` of a payload is `(seed * 131 + i * 37 + 11) mod 256`, so contents differ by seed.
fn payload(seed: &str, len: usize) -> Vec<u8> {
    let seed: u64 = seed.parse().unwrap();
    (0..len as u64)
        .map(|i| u8::try_from((seed * 131 + i * 37 + 11) % 256).unwrap())
        .collect()
}

fn deadline(word: &str) -> i64 {
    match word {
        "far" => FAR,
        "past" => PAST,
        other => panic!("deadline {other}"),
    }
}

fn opcode(word: &str) -> Opcode {
    match word {
        "text" => Opcode::Text,
        "binary" => Opcode::Binary,
        other => panic!("opcode {other}"),
    }
}

/// The client functions of the module (`reserve/3`, `write/5`): an expired deadline answers
/// `timeout` without a message, and a stopped Writer answers `destination_closed`.
fn client_call(
    machine: &mut Machine,
    name: &str,
    kind: Kind,
    raw: bool,
    deadline: i64,
) -> Option<Call> {
    let pid = machine.source(name);
    let _ = pid;
    let reply = |machine: &mut Machine, text: &str| {
        let line = format!("{name} {} {text}", kind.name());
        machine.replies.push(line);
    };
    if !raw && deadline <= 0 {
        reply(machine, "error timeout");
        return None;
    }
    if machine.writer.is_stopped() {
        reply(
            machine,
            if raw {
                "exit"
            } else {
                "error destination_closed"
            },
        );
        return None;
    }
    let call = machine.call();
    machine.pending.insert(
        call,
        Pending {
            source: name.to_owned(),
            kind,
            raw,
        },
    );
    Some(call)
}

fn render(baseline: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut machine: Option<Machine> = None;
    let mut heading: Option<&str> = None;
    let all: Vec<&str> = baseline.lines().collect();
    for (index, line) in all.iter().copied().enumerate() {
        if let Some(name) = line.strip_prefix("# ") {
            heading = Some(name);
            continue;
        }
        let Some(op) = line.strip_prefix("> ") else {
            continue;
        };
        let fields: Vec<&str> = op.split(' ').collect();
        if let ["scenario", _, timeout, control_bytes] = fields[..] {
            let fresh = Machine::new(timeout.parse().unwrap(), control_bytes.parse().unwrap());
            out.push(format!(
                "# {}\n> {op}\n{}",
                heading.take().expect("a scenario heading"),
                fresh.state_line()
            ));
            machine = Some(fresh);
            continue;
        }
        let machine = machine.as_mut().expect("a scenario first");
        // The clock reading the Writer had when it granted the reservation: an input.
        machine.now = all[index + 1..]
            .iter()
            .take_while(|next| !next.starts_with("> ") && !next.starts_with("# "))
            .find_map(|next| next.strip_prefix("~ elapsed="))
            .map_or(0, |value| value.parse().unwrap());
        let skipped = step(machine, &fields);
        machine.drain();
        machine.finish();
        if skipped {
            machine.timer_events.clear();
            out.push(format!("> {op}\n= skip\n{}", machine.state_line()));
        } else {
            out.push(machine.block(op));
        }
    }
    out.join("\n")
}

/// Returns whether the operation was skipped (the source is waiting for a reply).
fn step(machine: &mut Machine, fields: &[&str]) -> bool {
    match fields {
        ["reserve" | "reserve_raw", source, bytes, dl] => {
            if machine.busy(source) {
                return true;
            }
            let raw = fields[0] == "reserve_raw";
            let deadline = deadline(dl);
            let pid = machine.source(source);
            if let Some(call) = client_call(machine, source, Kind::Reserve, raw, deadline) {
                let now = machine.now;
                machine
                    .writer
                    .reserve((pid, call), bytes.parse().unwrap(), deadline, now);
            }
        }
        ["write" | "write_raw", source, tvar, op, len, dl, seed] => {
            if machine.busy(source) {
                return true;
            }
            let raw = fields[0] == "write_raw";
            let deadline = deadline(dl);
            let token = machine.token(tvar);
            if let Some(call) = client_call(machine, source, Kind::Write, raw, deadline) {
                machine
                    .writer
                    .write(call, token, opcode(op), payload(seed, len.parse().unwrap()));
            }
        }
        ["control", len, seed] => {
            machine.counters[2] += 1;
            let name = format!("c{}", machine.counters[2]);
            machine.source(&name);
            if machine.writer.is_stopped() {
                machine
                    .replies
                    .push(format!("{name} control error destination_closed"));
            } else {
                let call = machine.call();
                machine.pending.insert(
                    call,
                    Pending {
                        source: name,
                        kind: Kind::Control,
                        raw: false,
                    },
                );
                machine
                    .writer
                    .control(call, payload(seed, len.parse().unwrap()), 0);
            }
        }
        ["ack", wvar] => {
            let reference = machine
                .frames
                .get(*wvar)
                .copied()
                .unwrap_or(Reference(u64::MAX));
            machine.writer.written(reference, machine.now);
        }
        ["timeout", tvar] => {
            let token = machine.token(tvar);
            machine.writer.reservation_timeout(token);
        }
        ["close", code] => machine.writer.close(code.parse().unwrap(), "bye"),
        ["kill", source] => {
            let alive = machine
                .sources
                .get(*source)
                .is_some_and(|pid| !machine.dead.contains(pid));
            if !alive {
                return true;
            }
            let pid = machine.source(source);
            machine.dead.insert(pid);
            // Replies to a dead process go nowhere.
            machine.pending.retain(|_, call| call.source != *source);
            // The exit message consumes the monitor on it.
            if let Some(count) = machine.monitors.get_mut(&pid) {
                *count = count.saturating_sub(1);
            }
            machine.writer.source_down(pid, machine.now);
        }
        ["kill_dest"] => machine.writer.destination_down(),
        other => panic!("unknown operation {other:?}"),
    }
    false
}

#[test]
fn the_baseline_ran_the_committed_operations() {
    let committed = fixture("relay-writer-ops.txt");
    let ops: Vec<&str> = committed
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    let baseline = baseline();
    let ran: Vec<&str> = baseline
        .lines()
        .filter_map(|line| line.strip_prefix("> "))
        .collect();
    assert_eq!(ran, ops);
}

#[test]
fn writer_matches_the_pinned_relay() {
    let baseline = baseline();
    let rendered = render(&baseline);
    if rendered != baseline {
        let first = rendered
            .lines()
            .zip(baseline.lines())
            .enumerate()
            .find(|(_, (rust, pinned))| rust != pinned);
        panic!("first difference (line, rust, pinned): {first:?}");
    }
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % bound as u64).unwrap()
    }
}

const HAND_SCENARIOS: &str = "\
# one reservation, write and acknowledgement
scenario basic 30000 1000
reserve S1 10 far
write S1 t1 text 10 far 10
ack w1
reserve S1 5 far
write S1 t2 binary 5 far 17
ack w2
# a second source queues behind the first
scenario queue 30000 1000
reserve S1 10 far
reserve S2 10 far
reserve S3 10 far
write S1 t1 text 3 far 24
ack w1
write S2 t2 text 4 far 31
kill S3
ack w2
reserve S3 1 far
# invalid reservations and expired deadlines
scenario invalid 30000 1000
write S1 t9 text 1 far 38
reserve S1 10 past
reserve_raw S1 10 past
reserve S1 10 far
write S2 t1 text 1 far 45
write S1 t1 text 1 past 52
write_raw S1 t1 text 1 past 59
reserve_raw S2 10 past
reserve_raw S3 10 far
write S1 t1 text 2 far 66
write S1 t1 text 2 far 73
ack w1
ack w2
ack w3
# control frames queue up to the byte bound
scenario control 30000 50
control 10 164
control 20 171
control 20 178
ack w1
ack w2
control 30 185
control 30 192
control 30 199
# a control frame waits behind a reservation
scenario control_behind 30000 100
reserve S1 10 far
control 10 206
control 10 213
write S1 t1 text 1 far 80
ack w1
ack w2
ack w3
# control with an expired deadline
scenario control_expired 0 100
control 10 220
control 10 227
# reservation timeout sheds the destination
scenario timeout 30000 100
reserve S1 10 far
reserve S2 10 far
control 10 234
timeout t1
reserve S1 1 far
control 5 241
# reservation timeout while a write is in flight
scenario timeout_write 30000 100
reserve S1 10 far
write S1 t1 text 10 far 87
reserve S2 10 far
timeout t1
# the source exits
scenario source_exit 30000 100
reserve S1 10 far
reserve S2 10 far
kill S1
reserve S3 10 far
write S2 t2 text 2 far 94
kill S2
write S3 t3 text 2 far 101
kill S3
control 3 248
# a dead source in the queue
scenario source_exit_queued 30000 100
reserve S1 10 far
reserve S2 10 far
reserve S3 10 far
kill S2
ack w1
write S1 t1 text 1 far 108
ack w1
write S3 t3 text 1 far 115
ack w2
# the destination exits or the Writer is closed
scenario destination_exit 30000 100
reserve S1 10 far
reserve S2 10 far
write S1 t1 text 4 far 122
kill_dest
reserve S3 10 far
control 1 255
write S3 t3 text 1 far 129
scenario closed 30000 100
reserve S1 10 far
reserve S2 10 far
write S1 t1 text 4 far 136
close 1000
reserve S3 10 far
reserve_raw S3 1 far
control 1 262
# a second write on one reservation replaces the first caller
scenario double_write 30000 100
reserve S1 10 far
write_raw S1 t1 text 1 far 143
reserve S2 10 far
ack w1
ack w1
# queued raw reserve with an expired deadline
scenario queued_expired 30000 100
reserve S1 10 far
reserve_raw S2 10 past
reserve_raw S3 10 far
write S1 t1 text 1 far 150
ack w1
write S3 t2 text 1 far 157
";

#[test]
#[ignore = "writes the operation script: SPOCKY_RELAY_OPS_OUT=path"]
fn write_ops() {
    use std::fmt::Write;
    let mut out = String::from(HAND_SCENARIOS);
    let mut random = Random(0x2545_f491_4f6c_dd1d);
    for index in 0..150 {
        let timeout = [0, 30_000, 30_000, 30_000, 30_000, 30_000, 30_000, 30_000][random.below(8)];
        let control_bytes = [10, 50, 1_000][random.below(3)];
        writeln!(out, "# generated {index}").unwrap();
        writeln!(out, "scenario generated{index} {timeout} {control_bytes}").unwrap();
        let (mut tokens, mut frames) = (0, 0);
        for _ in 0..(15 + random.below(30)) {
            let source = format!("S{}", 1 + random.below(4));
            let token = format!("t{}", 1 + random.below(tokens.min(3) + 1));
            let frame = format!("w{}", 1 + random.below(frames.min(3) + 1));
            let deadline = ["far", "far", "far", "past"][random.below(4)];
            let opcode = ["text", "binary"][random.below(2)];
            match random.below(26) {
                0..=4 => {
                    let raw = ["reserve", "reserve", "reserve_raw"][random.below(3)];
                    writeln!(out, "{raw} {source} {} {deadline}", 1 + random.below(50)).unwrap();
                    tokens += 1;
                }
                5..=9 => {
                    let raw = ["write", "write", "write_raw"][random.below(3)];
                    writeln!(
                        out,
                        "{raw} {source} {token} {opcode} {} {deadline} {}",
                        random.below(30),
                        random.below(1000)
                    )
                    .unwrap();
                    frames += 1;
                }
                10..=12 => {
                    writeln!(
                        out,
                        "control {} {}",
                        1 + random.below(30),
                        random.below(1000)
                    )
                    .unwrap();
                    frames += 1;
                }
                18 if random.below(3) == 0 => writeln!(out, "timeout {token}").unwrap(),
                19..=21 => writeln!(out, "kill {source}").unwrap(),
                22 if random.below(12) == 0 => writeln!(out, "kill_dest").unwrap(),
                23 if random.below(16) == 0 => writeln!(out, "close 1000").unwrap(),
                _ => writeln!(out, "ack {frame}").unwrap(),
            }
        }
    }
    std::fs::write(
        std::env::var_os("SPOCKY_RELAY_OPS_OUT").expect("output path"),
        out,
    )
    .unwrap();
}
