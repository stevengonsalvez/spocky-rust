//! `capacity::Capacity` against the transcript the pinned `PaseoRelay.Capacity` printed for the
//! operation script in `fixtures/relay-capacity-ops.txt` (see
//! `scripts/phase4/relay-capacity-baseline.exs` for the operation language and the block format).
//!
//! The replay gives the port the inputs the BEAM process read (the memory reading and the
//! watermark, the `~` lines) and compares everything else as raw text: replies, messages sent to
//! sockets, gauges, pressure, tree orders, map sizes and metrics.

#![allow(clippy::too_many_lines)]

use spocky_relay::capacity::{
    AdmitConnectionError, AdmitMessageError, Capacity, Config, Effect, Pid, StartDeliveryError,
    Token,
};
use std::collections::{BTreeMap, BTreeSet};

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(path).expect("a fixture")
}

struct Machine {
    capacity: Capacity,
    processes: BTreeMap<String, Pid>,
    names: BTreeMap<Pid, String>,
    dead: BTreeSet<String>,
    tokens: BTreeMap<String, Token>,
    counters: [u64; 2],
    clock: i64,
    disconnects: i64,
    waits: i64,
    received: Vec<String>,
    next_pid: u64,
}

impl Machine {
    fn new(config: Config) -> Self {
        Self {
            capacity: Capacity::new(config),
            processes: BTreeMap::new(),
            names: BTreeMap::new(),
            dead: BTreeSet::new(),
            tokens: BTreeMap::new(),
            counters: [0, 0],
            clock: 0,
            disconnects: 0,
            waits: 0,
            received: Vec::new(),
            next_pid: 0,
        }
    }

    fn pid(&self, name: &str) -> Pid {
        *self.processes.get(name).expect("a spawned process")
    }

    fn token(&self, name: &str) -> Token {
        // An unknown variable is `nil` in the script; no token is ever 0.
        self.tokens.get(name).copied().unwrap_or(Token(0))
    }

    fn drain_effects(&mut self) {
        for effect in self.capacity.take_effects() {
            match effect {
                Effect::MemoryPressure(pid) => {
                    self.received
                        .push(format!("{}:relay_memory_pressure", self.names[&pid]));
                }
                Effect::MemoryPressureDisconnect => self.disconnects += 1,
                Effect::ObserveDeliveryWait { .. } => self.waits += 1,
                _ => {}
            }
        }
    }

    fn tick(&mut self) -> i64 {
        self.clock += 1_000;
        self.clock
    }

    fn state_line(&self) -> String {
        let gauges = self.capacity.gauges();
        let pressure = self
            .capacity
            .pressure()
            .map_or("none".to_owned(), |p| format!("{}/{}", p.victims, p.batch));
        let names = |pids: Vec<Pid>| {
            pids.iter()
                .map(|pid| self.names.get(pid).map_or("?", String::as_str).to_owned())
                .collect::<Vec<_>>()
                .join(",")
        };
        let sizes = self
            .capacity
            .sizes()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "state gauges={},{},{},{} pressure={pressure} active={} blocked={} sizes={sizes} \
             metrics={},{}",
            gauges.active_websockets,
            gauges.ingress_reserved_bytes,
            gauges.inflight_delivery_bytes,
            gauges.backpressured_sources,
            names(self.capacity.active_order()),
            names(self.capacity.blocked_order()),
            self.disconnects,
            self.waits,
        )
    }
}

fn config_error<T: std::fmt::Debug>(error: T) -> String {
    let text = format!("{error:?}");
    let mut snake = String::new();
    for (index, character) in text.chars().enumerate() {
        if character.is_uppercase() && index > 0 {
            snake.push('_');
        }
        snake.push(character.to_ascii_lowercase());
    }
    snake
}

/// One block of the baseline: the lines from a `> ` line up to the next one.
struct Block<'a> {
    heading: Option<&'a str>,
    op: &'a str,
    inputs: Vec<&'a str>,
}

fn parse_blocks(text: &str) -> Vec<Block<'_>> {
    let mut blocks: Vec<Block<'_>> = Vec::new();
    let mut heading = None;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("# ") {
            heading = Some(name);
        } else if let Some(op) = line.strip_prefix("> ") {
            blocks.push(Block {
                heading: heading.take(),
                op,
                inputs: Vec::new(),
            });
        } else if let Some(input) = line.strip_prefix("~ ") {
            blocks.last_mut().expect("a block").inputs.push(input);
        }
    }
    blocks
}

fn input(block: &Block<'_>, key: &str) -> i64 {
    block
        .inputs
        .iter()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .unwrap_or_else(|| panic!("{key} input in block {}", block.op))
        .parse()
        .expect("an integer")
}

fn render(baseline: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut machine: Option<Machine> = None;
    for block in parse_blocks(baseline) {
        let fields: Vec<&str> = block.op.split(' ').collect();
        if let ["scenario", _, budget, weight, watermark] = fields[..] {
            let name = block.heading.expect("a scenario heading");
            let config = Config {
                ingress_budget_bytes: budget.parse().unwrap(),
                ingress_weight: weight.parse().unwrap(),
                memory_watermark_bytes: watermark.parse().unwrap(),
            };
            let fresh = Machine::new(config);
            out.push(format!(
                "# {name}\n> {}\n= ok\n{}",
                block.op,
                fresh.state_line()
            ));
            machine = Some(fresh);
            continue;
        }
        let machine = machine.as_mut().expect("a scenario first");
        machine.received.clear();
        let mut inputs: Vec<String> = Vec::new();
        let reply = step(machine, &fields, &block, &mut inputs);
        machine.drain_effects();
        let mut lines = vec![format!("> {}", block.op), format!("= {reply}")];
        lines.extend(inputs.iter().map(|line| format!("~ {line}")));
        if !machine.received.is_empty() {
            let mut received = machine.received.clone();
            received.sort();
            lines.push(format!("@ {}", received.join(" ")));
        }
        lines.push(machine.state_line());
        out.push(lines.join("\n"));
    }
    out.join("\n")
}

fn step(
    machine: &mut Machine,
    fields: &[&str],
    block: &Block<'_>,
    inputs: &mut Vec<String>,
) -> String {
    match fields {
        ["spawn", name] => {
            machine.next_pid += 1;
            let pid = Pid(machine.next_pid);
            machine.processes.insert((*name).to_owned(), pid);
            machine.names.insert(pid, (*name).to_owned());
            "ok".to_owned()
        }
        ["kill", name] => {
            let pid = machine.pid(name);
            machine.dead.insert((*name).to_owned());
            machine.capacity.process_down(pid);
            "ok".to_owned()
        }
        ["admit", _caller, namespace, limit, holder] => {
            let alive = !machine.dead.contains(*holder);
            let holder = machine.pid(holder);
            let result =
                machine
                    .capacity
                    .admit_connection(namespace, limit.parse().unwrap(), holder, alive);
            reply_token(
                machine,
                0,
                "c",
                result.map_err(config_error::<AdmitConnectionError>),
            )
        }
        ["attach", caller, cvar] => {
            let result = machine
                .capacity
                .attach_connection(machine.token(cvar), machine.pid(caller));
            result.map_or_else(|_| "error expired".to_owned(), |()| "ok".to_owned())
        }
        ["release", _caller, cvar] => {
            machine.capacity.release_connection(machine.token(cvar));
            "ok".to_owned()
        }
        ["expire", cvar] => {
            machine.capacity.expire(machine.token(cvar));
            "ok".to_owned()
        }
        ["msg", caller, bytes] => {
            let result = machine
                .capacity
                .admit_message(machine.pid(caller), bytes.parse().unwrap());
            reply_token(
                machine,
                1,
                "m",
                result.map_err(config_error::<AdmitMessageError>),
            )
        }
        ["start", caller, mvar] => {
            let now = machine.tick();
            let result =
                machine
                    .capacity
                    .start_delivery(machine.token(mvar), machine.pid(caller), now);
            result.map_or_else(
                |error| format!("error {}", config_error::<StartDeliveryError>(error)),
                |()| "ok".to_owned(),
            )
        }
        ["finish", _caller, mvar] => {
            let now = machine.tick();
            machine.capacity.finish_message(machine.token(mvar), now);
            "ok".to_owned()
        }
        ["cancel", _caller, mvar] => {
            machine.capacity.cancel_message(machine.token(mvar));
            "ok".to_owned()
        }
        ["check_now"] => {
            let memory = input(block, "memory");
            machine.capacity.check_now(memory);
            inputs.push(format!("memory={memory}"));
            "ok".to_owned()
        }
        ["check"] => {
            let memory = input(block, "memory");
            machine.capacity.check(memory);
            inputs.push(format!("memory={memory}"));
            "ok".to_owned()
        }
        ["recheck"] => {
            let memory = input(block, "memory");
            machine.capacity.pressure_recheck(memory);
            inputs.push(format!("memory={memory}"));
            "ok".to_owned()
        }
        ["set_watermark_rel", _delta] => {
            let watermark = input(block, "watermark");
            machine.capacity.set_watermark(watermark);
            inputs.push(format!("watermark={watermark}"));
            "ok".to_owned()
        }
        ["set_watermark_abs", bytes] => {
            machine.capacity.set_watermark(bytes.parse().unwrap());
            "ok".to_owned()
        }
        ["status", namespace, limit] => {
            let status = machine.capacity.status(namespace, limit.parse().unwrap());
            let gauges = status.gauges;
            format!(
                "available {} {},{},{},{}",
                config_error(status.admission),
                gauges.active_websockets,
                gauges.ingress_reserved_bytes,
                gauges.inflight_delivery_bytes,
                gauges.backpressured_sources
            )
        }
        ["active", namespace] => machine.capacity.active_connections(namespace).to_string(),
        ["value", name] => {
            let gauges = machine.capacity.gauges();
            match *name {
                "active_websockets" => gauges.active_websockets,
                "ingress_reserved_bytes" => gauges.ingress_reserved_bytes,
                "inflight_delivery_bytes" => gauges.inflight_delivery_bytes,
                "backpressured_sources" => gauges.backpressured_sources,
                other => panic!("unknown value {other}"),
            }
            .to_string()
        }
        other => panic!("unknown operation {other:?}"),
    }
}

fn reply_token(
    machine: &mut Machine,
    counter: usize,
    prefix: &str,
    result: Result<Token, String>,
) -> String {
    match result {
        Ok(token) => {
            machine.counters[counter] += 1;
            let name = format!("{prefix}{}", machine.counters[counter]);
            machine.tokens.insert(name.clone(), token);
            format!("ok {name}")
        }
        Err(reason) => format!("error {reason}"),
    }
}

fn baseline() -> String {
    std::env::var_os("SPOCKY_RELAY_CAPACITY_BASELINE").map_or_else(
        || fixture("relay-capacity-baseline.txt"),
        |path| std::fs::read_to_string(path).expect("the baseline file"),
    )
}

#[test]
fn the_baseline_ran_the_committed_operations() {
    let committed = fixture("relay-capacity-ops.txt");
    let ops: Vec<&str> = committed
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    let ran: Vec<String> = parse_blocks(&baseline())
        .iter()
        .map(|block| block.op.to_owned())
        .collect();
    assert_eq!(ran, ops);
}

#[test]
fn capacity_matches_the_pinned_relay() {
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

/// Deterministic xorshift for the generated scenarios.
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
# basic lifecycle
scenario lifecycle 1000000 4 0
spawn p1
spawn p2
admit p1 a 2 p1
admit p2 a 2 p2
admit p1 a 2 p1
attach p1 c1
attach p1 c1
attach p2 c1
attach p2 c2
status a 2
active a
release p1 c1
status a 2
expire c2
active a
admit p1 a 2 p1
expire c3
admit p1 a 3 p1
value active_websockets
# capacity and configuration
scenario limits 1000000 4 0
spawn p1
spawn p2
spawn p3
admit p1 a 1 p1
admit p2 a 1 p2
admit p2 a 2 p2
admit p2 b 1 p2
attach p1 c1
attach p2 c2
status a 1
status a 2
status b 1
status c 5
release p1 c1
admit p3 a 1 p3
kill p3
admit p1 a 1 p3
# holders that exit
scenario exits 1000000 4 0
spawn p1
spawn p2
spawn p3
admit p1 a 5 p1
admit p2 a 5 p2
admit p3 a 5 p3
attach p2 c2
msg p2 100
start p2 m1
kill p1
kill p2
status a 5
msg p3 10
kill p3
status a 5
# message budget
scenario budget 1000 2 0
spawn p1
spawn p2
admit p1 a 5 p1
admit p2 a 5 p2
attach p1 c1
attach p2 c2
msg p1 100
msg p2 400
msg p2 1
msg p1 501
msg p1 500
cancel p1 m1
msg p1 500
msg p2 1
finish p2 m2
finish p2 m2
msg p2 1
value ingress_reserved_bytes
# delivery and blocked sources
scenario delivery 1000000 1 0
spawn p1
spawn p2
admit p1 a 5 p1
admit p2 a 5 p2
attach p1 c1
attach p2 c2
msg p1 10
msg p1 20
msg p2 30
start p1 m1
start p1 m1
start p1 m2
start p2 m3
start p2 m3
value inflight_delivery_bytes
value backpressured_sources
finish p1 m1
value backpressured_sources
finish p1 m2
value backpressured_sources
cancel p2 m3
value backpressured_sources
value inflight_delivery_bytes
start p1 m9
msg p1 5
start p2 m4
release p1 c1
status a 5
# shedding order: blocked oldest first, then the newest active
scenario shedding 1000000 1 0
spawn p1
spawn p2
spawn p3
spawn p4
spawn p5
admit p1 a 9 p1
admit p2 a 9 p2
admit p3 a 9 p3
admit p4 a 9 p4
admit p5 a 9 p5
attach p1 c1
attach p2 c2
attach p3 c3
attach p4 c4
attach p5 c5
msg p3 10
msg p2 10
msg p4 10
start p3 m1
start p2 m2
start p4 m3
set_watermark_rel -16777216
check_now
status a 9
msg p1 10
start p1 m4
admit p1 a 9 p1
recheck
recheck
recheck
check
set_watermark_abs 0
check_now
status a 9
msg p5 1
# pressure with nothing to shed, and recovery
scenario pressure_idle 1000000 1 0
spawn p1
set_watermark_rel -16777216
check_now
status a 1
recheck
set_watermark_abs 0
status a 1
admit p1 a 1 p1
set_watermark_rel 536870912
check_now
check
status a 1
";

#[test]
#[ignore = "writes the operation script: SPOCKY_RELAY_OPS_OUT=path"]
fn write_ops() {
    let mut out = String::from(HAND_SCENARIOS);
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    for index in 0..300 {
        scenario(&mut random, index, &mut out);
    }
    std::fs::write(
        std::env::var_os("SPOCKY_RELAY_OPS_OUT").expect("output path"),
        out,
    )
    .unwrap();
}

fn scenario(random: &mut Random, index: usize, out: &mut String) {
    use std::fmt::Write;
    let budget = [1_000, 5_000, 1_000_000][random.below(3)];
    let weight = 1 + random.below(4);
    writeln!(out, "# generated {index}").unwrap();
    writeln!(out, "scenario generated{index} {budget} {weight} 0").unwrap();
    let processes = 2 + random.below(5);
    for process in 1..=processes {
        writeln!(out, "spawn p{process}").unwrap();
    }
    let mut dead: BTreeSet<usize> = BTreeSet::new();
    let mut admitted: BTreeSet<usize> = BTreeSet::new();
    let (mut connections, mut messages) = (0, 0);
    let mut pressure = false;
    let length = 20 + random.below(40);
    for _ in 0..length {
        let live: Vec<usize> = (1..=processes).filter(|p| !dead.contains(p)).collect();
        if live.is_empty() {
            break;
        }
        let caller = live[random.below(live.len())];
        // A process holds one connection at a time in the relay (the request process). A second
        // attach by the same holder overwrites its socket entry and leaves a stale key in the
        // `active` tree that crashes the next shed (the process restarts), so the script avoids
        // it: each process admits for itself once, other holders are dead processes.
        let holder = match dead.iter().next() {
            Some(&gone) if random.below(6) == 0 => gone,
            _ => caller,
        };
        let namespace = ["a", "b"][random.below(2)];
        let limit = 1 + random.below(3);
        let connection = format!("c{}", 1 + random.below(connections + 2));
        let message = format!("m{}", 1 + random.below(messages + 2));
        match random.below(24) {
            0..=3 if holder != caller || admitted.insert(caller) => {
                writeln!(out, "admit p{caller} {namespace} {limit} p{holder}").unwrap();
                connections += 1;
            }
            4..=6 => writeln!(out, "attach p{caller} {connection}").unwrap(),
            7..=10 => {
                let bytes = [1, 10, 100, 250, 999, 1_001][random.below(6)];
                writeln!(out, "msg p{caller} {bytes}").unwrap();
                messages += 1;
            }
            11..=13 => writeln!(out, "start p{caller} {message}").unwrap(),
            14 | 15 => writeln!(out, "finish p{caller} {message}").unwrap(),
            16 => writeln!(out, "cancel p{caller} {message}").unwrap(),
            17 => writeln!(out, "release p{caller} {connection}").unwrap(),
            18 => writeln!(out, "expire {connection}").unwrap(),
            19 if live.len() > 1 => {
                writeln!(out, "kill p{caller}").unwrap();
                dead.insert(caller);
            }
            20 => writeln!(out, "status {namespace} {limit}").unwrap(),
            21 if !pressure => {
                writeln!(out, "set_watermark_rel -16777216").unwrap();
                pressure = true;
            }
            22 if pressure => writeln!(
                out,
                "{}",
                ["check_now", "check", "recheck"][random.below(3)]
            )
            .unwrap(),
            _ => writeln!(out, "active {namespace}").unwrap(),
        }
    }
    if pressure {
        writeln!(out, "set_watermark_abs 0").unwrap();
        writeln!(out, "check_now").unwrap();
    }
}
