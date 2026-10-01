//! Terminal output coalescing: the pinned `terminal-output-coalescer.test.ts`
//! cases, and a differential against the pinned `TerminalOutputCoalescer`
//! driven by the same operations on a fake clock and fake timers. Timer
//! schedules, clears, and flush payloads must match in order.

mod support;

use spocky_contracts::js_value::{JsValue, parse, stringify};
use spocky_terminal::output_coalescer::{
    CoalescerFlush, DEFAULT_FLUSH_DELAY_MS, Handled, TerminalOutputCoalescer,
};

/// Fake timers like the pinned test harness: `run_scheduled` fires every armed
/// timer, `advance` moves the clock.
struct Harness {
    coalescer: TerminalOutputCoalescer,
    now: f64,
    scheduled: Vec<(u64, f64)>,
    flushes: Vec<(String, usize, usize)>,
}

impl Harness {
    fn new() -> Self {
        Self {
            coalescer: TerminalOutputCoalescer::default(),
            now: 1000.0,
            scheduled: Vec::new(),
            flushes: Vec::new(),
        }
    }

    fn record(&mut self, flush: Option<CoalescerFlush>) {
        if let Some(flush) = flush {
            self.flushes.push((
                String::from_utf8(flush.payload).expect("utf8"),
                flush.chars,
                flush.bytes,
            ));
        }
        if !self.coalescer.timer_pending() {
            self.scheduled.clear();
        }
    }

    fn handle(&mut self, data: &str) {
        match self.coalescer.handle(data, self.now) {
            Handled::Flushed(flush) => self.record(Some(flush)),
            Handled::Scheduled(timer) => self.scheduled.push((timer.token, timer.delay_ms)),
            Handled::Buffered | Handled::Ignored => {}
        }
    }

    fn flush(&mut self) {
        let flush = self.coalescer.flush(self.now);
        self.record(flush);
    }

    fn run_scheduled(&mut self) {
        for (token, _) in std::mem::take(&mut self.scheduled) {
            let flush = self.coalescer.fire(token, self.now);
            self.record(flush);
        }
    }

    fn flushes(&self) -> Vec<(&str, usize, usize)> {
        self.flushes
            .iter()
            .map(|(payload, chars, bytes)| (payload.as_str(), *chars, *bytes))
            .collect()
    }
}

#[test]
fn flushes_the_first_chunk_immediately_on_the_leading_edge() {
    let mut h = Harness::new();
    h.handle("a");
    assert!(h.scheduled.is_empty());
    assert_eq!(h.flushes(), [("a", 1, 1)]);
}

#[test]
fn coalesces_a_burst_after_the_leading_edge_into_one_trailing_flush() {
    let mut h = Harness::new();
    h.handle("a");
    h.handle("b");
    h.handle("é");
    assert_eq!(h.scheduled.len(), 1);
    assert!((h.scheduled[0].1 - DEFAULT_FLUSH_DELAY_MS).abs() < f64::EPSILON);
    assert_eq!(h.flushes(), [("a", 1, 1)]);
    h.run_scheduled();
    assert_eq!(h.flushes(), [("a", 1, 1), ("bé", 2, 3)]);
}

#[test]
fn flushes_immediately_again_once_the_window_has_elapsed() {
    let mut h = Harness::new();
    h.handle("a");
    h.now += 5.0;
    h.handle("b");
    assert!(h.scheduled.is_empty());
    assert_eq!(h.flushes(), [("a", 1, 1), ("b", 1, 1)]);
}

#[test]
fn manual_flush_drains_pending_output_and_cancels_the_timer() {
    let mut h = Harness::new();
    h.handle("hello");
    h.handle(" world");
    h.flush();
    h.run_scheduled();
    assert!(h.scheduled.is_empty());
    assert_eq!(h.flushes(), [("hello", 5, 5), (" world", 6, 6)]);
}

#[test]
fn dispose_drops_pending_output() {
    let mut h = Harness::new();
    h.handle("done");
    h.handle("pending");
    h.coalescer.dispose();
    h.run_scheduled();
    assert!(!h.coalescer.timer_pending());
    assert_eq!(h.flushes(), [("done", 4, 4)]);
}

#[test]
fn mark_flushed_keeps_the_next_chunk_on_the_trailing_path() {
    let mut h = Harness::new();
    h.coalescer.mark_flushed(h.now);
    h.handle("post-snapshot");
    assert_eq!(h.scheduled.len(), 1);
    assert!(h.flushes.is_empty());
    h.run_scheduled();
    assert_eq!(h.flushes(), [("post-snapshot", 13, 13)]);
}

#[test]
fn preserves_ordering_across_leading_and_trailing_flushes() {
    let mut h = Harness::new();
    h.handle("1");
    h.handle("2");
    h.handle("3");
    h.run_scheduled();
    h.now += 5.0;
    h.handle("4");
    let payloads: Vec<&str> = h.flushes().iter().map(|f| f.0).collect();
    assert_eq!(payloads, ["1", "23", "4"]);
}

#[test]
fn counts_chars_in_utf16_units_and_ignores_empty_chunks() {
    let mut h = Harness::new();
    h.handle("");
    assert!(h.flushes.is_empty());
    h.handle("😀x");
    assert_eq!(h.flushes(), [("😀x", 3, 5)]);
}

#[test]
fn a_stale_timer_token_never_flushes() {
    let mut coalescer = TerminalOutputCoalescer::default();
    assert!(matches!(coalescer.handle("a", 0.0), Handled::Flushed(_)));
    let Handled::Scheduled(timer) = coalescer.handle("b", 1.0) else {
        panic!("expected a trailing timer");
    };
    assert!(coalescer.flush(2.0).is_some());
    assert_eq!(coalescer.fire(timer.token, 6.0), None);
}

/// `["handle", now, text]`, `["fire", now]` (fire every armed timer),
/// `["flush", now]`, `["mark", now]`, `["dispose", now]`.
const OPS: &str = r#"[
  ["handle", 1000, "a"], ["handle", 1001, "b"], ["handle", 1002, "\u00e9"], ["fire", 1006],
  ["handle", 1008, "c"], ["handle", 1011, "d"], ["fire", 1013],
  ["handle", 1013, "within"], ["flush", 1014], ["fire", 1019],
  ["handle", 1030, "x"], ["handle", 1031, "pending"], ["dispose", 1032], ["fire", 1036],
  ["handle", 1036, "after-dispose"], ["mark", 1050], ["handle", 1050, "post"],
  ["handle", 1052, "\ud83d\ude00"], ["fire", 1055], ["handle", 1059, "edge"],
  ["handle", 1060, "e\u0301"], ["flush", 1060], ["flush", 1061], ["handle", 1061, ""],
  ["fire", 1070], ["mark", 1100], ["fire", 1100], ["handle", 1104, "z"], ["handle", 1105, "y"]
]"#;

const NODE_SCRIPT: &str = r#"
const [terminalDir, opsJson] = process.argv.slice(1);
const { TerminalOutputCoalescer } = await import(`${terminalDir}/terminal-output-coalescer.js`);
let clock = 0;
let nextId = 0;
const timers = new Map();
const out = [];
const coalescer = new TerminalOutputCoalescer({
  now: () => clock,
  timers: {
    setTimeout(callback, ms) {
      const id = ++nextId;
      timers.set(id, callback);
      out.push(["schedule", ms]);
      return id;
    },
    clearTimeout(id) {
      timers.delete(id);
      out.push(["clear"]);
    },
  },
  onFlush: ({ payload, chars, bytes }) => out.push(["flush", payload.toString("utf8"), chars, bytes]),
});
for (const [op, now, text] of JSON.parse(opsJson)) {
  clock = now;
  if (op === "handle") coalescer.handle(text);
  if (op === "flush") coalescer.flush();
  if (op === "mark") coalescer.markFlushed();
  if (op === "dispose") coalescer.dispose();
  if (op === "fire") {
    const due = [...timers];
    timers.clear();
    for (const [, callback] of due) callback();
  }
}
process.stdout.write(JSON.stringify(out));
"#;

fn rust_output(ops: &str) -> String {
    let mut coalescer = TerminalOutputCoalescer::default();
    let mut armed: Vec<u64> = Vec::new();
    let mut out: Vec<JsValue> = Vec::new();
    let tag = |name: &str| JsValue::String(name.to_owned());
    let emit = |out: &mut Vec<JsValue>, flush: Option<CoalescerFlush>| {
        if let Some(flush) = flush {
            #[allow(clippy::cast_precision_loss)]
            out.push(JsValue::Array(vec![
                tag("flush"),
                JsValue::String(String::from_utf8(flush.payload).expect("utf8")),
                JsValue::Number(flush.chars as f64),
                JsValue::Number(flush.bytes as f64),
            ]));
        }
    };
    for op in parse(ops).expect("ops").as_array().expect("array") {
        let op = op.as_array().expect("op");
        let now = op[1].as_f64().expect("now");
        let was_pending = coalescer.timer_pending();
        match op[0].as_str().expect("name") {
            "handle" => match coalescer.handle(op[2].as_str().expect("text"), now) {
                Handled::Flushed(flush) => emit(&mut out, Some(flush)),
                Handled::Scheduled(timer) => {
                    armed.push(timer.token);
                    out.push(JsValue::Array(vec![
                        tag("schedule"),
                        JsValue::Number(timer.delay_ms),
                    ]));
                }
                Handled::Buffered | Handled::Ignored => {}
            },
            "fire" => {
                for token in std::mem::take(&mut armed) {
                    let flush = coalescer.fire(token, now);
                    emit(&mut out, flush);
                }
            }
            "mark" => coalescer.mark_flushed(now),
            name => {
                if was_pending {
                    armed.clear();
                    out.push(JsValue::Array(vec![tag("clear")]));
                }
                if name == "flush" {
                    let flush = coalescer.flush(now);
                    emit(&mut out, flush);
                } else {
                    coalescer.dispose();
                }
            }
        }
    }
    stringify(&JsValue::Array(out))
}

/// Seeded operation sequence covering every branch with mixed widths.
fn generated_ops() -> String {
    let texts = [
        "a",
        "bc",
        "é",
        "😀",
        "\u{1b}[31mx\u{1b}[0m",
        "\r\n",
        "中文",
        "z",
    ];
    let mut state: u64 = 0x5eed_1234;
    let mut next = |bound: u64| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) % bound
    };
    let mut now = 5000;
    let mut ops = Vec::new();
    for _ in 0..600 {
        now += next(4);
        let op = match next(10) {
            0..=5 => {
                let text = texts[usize::try_from(next(texts.len() as u64)).expect("index")];
                format!(
                    r#"["handle", {now}, {}]"#,
                    stringify(&JsValue::String(text.to_owned()))
                )
            }
            6 | 7 => format!(r#"["fire", {now}]"#),
            8 => format!(r#"["flush", {now}]"#),
            _ => {
                if next(2) == 0 {
                    format!(r#"["mark", {now}]"#)
                } else {
                    format!(r#"["dispose", {now}]"#)
                }
            }
        };
        ops.push(op);
    }
    format!("[{}]", ops.join(","))
}

#[test]
fn coalescing_matches_pinned_coalescer() {
    let Some(pinned) = support::pinned("output coalescer differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    for ops in [OPS.to_owned(), generated_ops()] {
        let expected = support::run_node(&pinned, NODE_SCRIPT, &[&ops]);
        assert_eq!(rust_output(&ops), expected);
    }
}
