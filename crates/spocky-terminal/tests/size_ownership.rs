//! Terminal size ownership: the pinned `terminal-size-ownership.test.ts`
//! cases, a dropped-owner case, and a differential against the pinned
//! `applyTerminalSize` over a seeded request sequence from three owners.

mod support;

use std::sync::Arc;

use spocky_contracts::js_value::{JsValue, parse, stringify};
use spocky_terminal::size_ownership::{SizeIntent, SizeOwnership, SizeRequest, SizeTarget};

struct FakeTerminal {
    size: (u16, u16),
    applied: Vec<String>,
}

impl FakeTerminal {
    fn new() -> Self {
        Self {
            size: (24, 80),
            applied: Vec::new(),
        }
    }
}

impl SizeTarget for FakeTerminal {
    fn size(&self) -> (u16, u16) {
        self.size
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.size = (rows, cols);
        self.applied.push(format!("{cols}x{rows}"));
    }
}

fn request(rows: u16, cols: u16, intent: Option<SizeIntent>) -> SizeRequest {
    SizeRequest { rows, cols, intent }
}

#[test]
fn only_the_latest_claimant_can_update_a_terminal_size() {
    let mut terminal = FakeTerminal::new();
    let mut ownership = SizeOwnership::default();
    let owner_a = Arc::new(());
    let owner_b = Arc::new(());
    let claim = Some(SizeIntent::Claim);
    let update = Some(SizeIntent::Update);

    assert!(ownership.apply(&mut terminal, &owner_a, request(30, 100, claim)));
    assert!(ownership.apply(&mut terminal, &owner_a, request(31, 101, update)));
    assert!(!ownership.apply(&mut terminal, &owner_b, request(32, 102, update)));
    assert!(ownership.apply(&mut terminal, &owner_b, request(33, 103, claim)));
    assert!(!ownership.apply(&mut terminal, &owner_a, request(34, 104, update)));
    assert!(ownership.apply(&mut terminal, &owner_a, request(35, 105, None)));
    assert!(!ownership.apply(&mut terminal, &owner_b, request(36, 106, update)));

    assert_eq!(terminal.applied, ["100x30", "101x31", "103x33", "105x35"]);
}

#[test]
fn transfers_ownership_when_a_new_claimant_reports_the_current_size() {
    let mut terminal = FakeTerminal::new();
    let mut ownership = SizeOwnership::default();
    let owner_a = Arc::new(());
    let owner_b = Arc::new(());

    ownership.apply(
        &mut terminal,
        &owner_a,
        request(30, 100, Some(SizeIntent::Claim)),
    );
    ownership.apply(
        &mut terminal,
        &owner_b,
        request(30, 100, Some(SizeIntent::Claim)),
    );
    ownership.apply(
        &mut terminal,
        &owner_a,
        request(31, 101, Some(SizeIntent::Update)),
    );
    ownership.apply(
        &mut terminal,
        &owner_b,
        request(32, 102, Some(SizeIntent::Update)),
    );

    assert_eq!(terminal.applied, ["100x30", "102x32"]);
}

#[test]
fn a_dropped_owner_no_longer_owns_the_terminal() {
    let mut terminal = FakeTerminal::new();
    let mut ownership = SizeOwnership::default();
    let owner = Arc::new(());
    ownership.apply(
        &mut terminal,
        &owner,
        request(30, 100, Some(SizeIntent::Claim)),
    );
    drop(owner);
    let replacement = Arc::new(());
    assert!(!ownership.apply(
        &mut terminal,
        &replacement,
        request(31, 101, Some(SizeIntent::Update))
    ));
    assert_eq!(terminal.applied, ["100x30"]);
}

const NODE_SCRIPT: &str = r"
const [terminalDir, opsJson] = process.argv.slice(1);
const { applyTerminalSize } = await import(`${terminalDir}/terminal-size-ownership.js`);
const owners = [{}, {}, {}];
let size = { rows: 24, cols: 80 };
const out = [];
const terminal = {
  getSize: () => size,
  send: (message) => {
    size = { rows: message.rows, cols: message.cols };
    out.push(`${message.cols}x${message.rows}`);
  },
};
for (const [owner, rows, cols, intent] of JSON.parse(opsJson)) {
  out.push(applyTerminalSize(terminal, owners[owner], intent === null ? { rows, cols } : { rows, cols, intent }));
}
process.stdout.write(JSON.stringify(out));
";

/// `[owner, rows, cols, intent]` with intent `"claim"`, `"update"` or `null`.
fn generated_ops() -> String {
    let mut state: u64 = 0x0517_e0f5;
    let mut next = |bound: u64| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) % bound
    };
    let ops: Vec<String> = (0..400)
        .map(|_| {
            let intent =
                ["\"claim\"", "\"update\"", "null"][usize::try_from(next(3)).expect("index")];
            format!(
                "[{}, {}, {}, {intent}]",
                next(3),
                20 + next(3),
                80 + next(3)
            )
        })
        .collect();
    format!("[{}]", ops.join(","))
}

fn rust_output(ops: &str) -> String {
    let owners = [Arc::new(()), Arc::new(()), Arc::new(())];
    let mut terminal = FakeTerminal::new();
    let mut ownership = SizeOwnership::default();
    let mut out = Vec::new();
    for op in parse(ops).expect("ops").as_array().expect("array") {
        let op = op.as_array().expect("op");
        let number = |index: usize| {
            let value = op[index].as_f64().expect("number");
            format!("{value}").parse::<u16>().expect("u16")
        };
        let intent = match op[3].as_str() {
            Some("claim") => Some(SizeIntent::Claim),
            Some("update") => Some(SizeIntent::Update),
            _ => None,
        };
        let before = terminal.applied.len();
        let accepted = ownership.apply(
            &mut terminal,
            &owners[usize::from(number(0))],
            request(number(1), number(2), intent),
        );
        out.extend(
            terminal.applied[before..]
                .iter()
                .cloned()
                .map(JsValue::String),
        );
        out.push(JsValue::Bool(accepted));
    }
    stringify(&JsValue::Array(out))
}

#[test]
fn size_ownership_matches_pinned_apply_terminal_size() {
    let Some(pinned) = support::pinned("size ownership differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let ops = generated_ops();
    let expected = support::run_node(&pinned, NODE_SCRIPT, &[&ops]);
    assert_eq!(rust_output(&ops), expected);
}
