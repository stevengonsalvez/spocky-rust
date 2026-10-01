//! Terminal restore policy and frames: the pinned `terminal-restore.test.ts`
//! and `terminal-snapshot.test.ts` cases, and a differential that encodes
//! seeded snapshot states as restore and legacy snapshot frames through the
//! pinned `terminal-restore.js` and compares the bytes.

mod support;

use std::fmt::Write as _;

use spocky_contracts::js_value::{JsObject, JsValue, stringify};
use spocky_terminal::restore::{
    RestoreMode, RestoreOptions, RestoreSnapshot, SnapshotMode, SnapshotOptions,
    encode_legacy_snapshot_frame, encode_restore_frame, render_snapshot_to_ansi,
    restore_after_output_overflow, restore_snapshot_options, subscription_snapshot_mode,
};
use spocky_wire::{
    TerminalCell, TerminalCursor, TerminalCursorStyle, TerminalOpcode, TerminalState,
    decode_terminal_frame, encode_terminal_snapshot,
};

fn row(text: &str, cols: usize) -> Vec<TerminalCell> {
    let chars: Vec<char> = text.chars().collect();
    (0..cols)
        .map(|index| TerminalCell::new(chars.get(index).map_or(" ".to_owned(), char::to_string)))
        .collect()
}

fn state(grid: Vec<Vec<TerminalCell>>, cols: f64, cursor: (f64, f64)) -> TerminalState {
    TerminalState {
        #[allow(clippy::cast_precision_loss)]
        rows: grid.len() as f64,
        cols,
        grid,
        scrollback: Vec::new(),
        cursor: TerminalCursor {
            row: cursor.0,
            col: cursor.1,
            hidden: None,
            style: None,
            blink: None,
        },
        title: None,
        grid_wrapped: None,
        scrollback_wrapped: None,
    }
}

#[test]
fn uses_ready_snapshots_only_for_restore_aware_subscriptions() {
    assert_eq!(subscription_snapshot_mode(None), SnapshotMode::State);
    assert_eq!(
        subscription_snapshot_mode(Some(&RestoreOptions::new(RestoreMode::Live))),
        SnapshotMode::Ready
    );
}

#[test]
fn resolves_bounded_restore_snapshot_options() {
    let visible = |lines| RestoreOptions {
        scrollback_lines: lines,
        ..RestoreOptions::new(RestoreMode::VisibleSnapshot)
    };
    let bounded = |lines| {
        RestoreSnapshot::Options(SnapshotOptions {
            scrollback_lines: Some(lines),
            include_wrap_flags: false,
        })
    };
    assert_eq!(
        restore_snapshot_options(&RestoreOptions::new(RestoreMode::Live)),
        RestoreSnapshot::None
    );
    assert_eq!(
        restore_snapshot_options(&RestoreOptions::new(RestoreMode::FullSnapshot)),
        RestoreSnapshot::Full
    );
    assert_eq!(restore_snapshot_options(&visible(None)), bounded(200));
    assert_eq!(restore_snapshot_options(&visible(Some(999))), bounded(500));
}

#[test]
fn promotes_live_restore_to_visible_restore_after_output_overflow() {
    let live = RestoreOptions {
        scrollback_lines: Some(3),
        size: Some((24, 80)),
        ..RestoreOptions::new(RestoreMode::Live)
    };
    assert_eq!(
        restore_after_output_overflow(Some(live)),
        Some(RestoreOptions::new(RestoreMode::VisibleSnapshot))
    );
    let full = RestoreOptions::new(RestoreMode::FullSnapshot);
    assert_eq!(restore_after_output_overflow(Some(full)), Some(full));
    assert_eq!(restore_after_output_overflow(None), None);
}

#[test]
fn encodes_restore_snapshots_as_restore_frames() {
    let frame = decode_terminal_frame(&encode_restore_frame(
        4,
        &state(vec![row("restored", 80)], 80.0, (0.0, 8.0)),
    ))
    .expect("frame");
    assert_eq!(frame.opcode, TerminalOpcode::Restore);
    assert_eq!(frame.slot, 4);
    assert!(
        String::from_utf8(frame.payload)
            .expect("utf8")
            .contains("restored")
    );
}

#[test]
fn renders_soft_wrapped_rows_as_one_logical_line_when_wrap_flags_are_present() {
    let mut wrapped = state(
        vec![row("ABCDEFGHIJ", 10), row("KLMNOP", 6)],
        10.0,
        (1.0, 6.0),
    );
    wrapped.grid_wrapped = Some(vec![true, false]);
    wrapped.scrollback_wrapped = Some(Vec::new());
    let ansi = render_snapshot_to_ansi(&wrapped);
    assert!(ansi.contains("ABCDEFGHIJKLMNOP"));
    assert!(!ansi.contains("[?7l"));
}

#[test]
fn falls_back_to_verbatim_per_row_replay_without_wrap_flags() {
    let ansi = render_snapshot_to_ansi(&state(
        vec![row("ABCDEFGHIJ", 10), row("KLMNOP", 6)],
        10.0,
        (1.0, 6.0),
    ));
    assert!(ansi.contains("[?7l"));
    assert!(ansi.contains("ABCDEFGHIJ\r\nKLMNOP"));
}

struct Seeded(u64);

impl Seeded {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from((self.0 >> 33) % u64::try_from(bound).expect("bound")).expect("index")
    }

    fn flag(&mut self) -> Option<bool> {
        [None, Some(false), Some(true)][self.next(3)]
    }
}

fn seeded_cell(rng: &mut Seeded) -> TerminalCell {
    let chars = [" ", "a", "Z", "\u{4e2d}", "", "\u{1f600}", "\u{e9}", "-"];
    let mut cell = TerminalCell::new(chars[rng.next(chars.len())]);
    let color = |rng: &mut Seeded| -> (Option<f64>, Option<f64>) {
        match rng.next(5) {
            0 => (Some([0.0, 7.0, 8.0, 15.0][rng.next(4)]), Some(1.0)),
            1 => (Some([16.0, 200.0, 255.0][rng.next(3)]), Some(2.0)),
            2 => (Some([0.0, 66051.0, 16_777_215.0][rng.next(3)]), Some(3.0)),
            3 => (Some(3.0), None),
            _ => (None, None),
        }
    };
    (cell.fg, cell.fg_mode) = color(rng);
    (cell.bg, cell.bg_mode) = color(rng);
    cell.bold = rng.flag();
    cell.italic = rng.flag();
    cell.underline = rng.flag();
    cell.dim = rng.flag();
    cell.inverse = rng.flag();
    cell.strikethrough = rng.flag();
    if rng.next(3) == 0 {
        cell = TerminalCell::new(cell.char);
    }
    cell
}

#[allow(clippy::cast_precision_loss)]
fn seeded_state(rng: &mut Seeded) -> TerminalState {
    let cols = 1 + rng.next(6);
    let rows = |count: usize, rng: &mut Seeded| -> Vec<Vec<TerminalCell>> {
        (0..count)
            .map(|_| (0..rng.next(cols + 2)).map(|_| seeded_cell(rng)).collect())
            .collect()
    };
    let grid = rows(1 + rng.next(3), rng);
    let scrollback = rows(rng.next(3), rng);
    let flags = |count: usize, rng: &mut Seeded| -> Vec<bool> {
        (0..count).map(|_| rng.next(2) == 0).collect()
    };
    let (grid_wrapped, scrollback_wrapped) = match rng.next(4) {
        0 => (None, None),
        1 => (Some(flags(grid.len(), rng)), None),
        2 => (
            Some(flags(grid.len(), rng)),
            Some(flags(scrollback.len() + 1, rng)),
        ),
        _ => (
            Some(flags(grid.len(), rng)),
            Some(flags(scrollback.len(), rng)),
        ),
    };
    TerminalState {
        rows: grid.len() as f64,
        cols: cols as f64,
        cursor: TerminalCursor {
            row: rng.next(3) as f64,
            col: rng.next(7) as f64,
            hidden: rng.flag(),
            style: [
                None,
                Some(TerminalCursorStyle::Block),
                Some(TerminalCursorStyle::Underline),
                Some(TerminalCursorStyle::Bar),
            ][rng.next(4)],
            blink: rng.flag(),
        },
        title: (rng.next(2) == 0).then(|| "title".to_owned()),
        grid,
        scrollback,
        grid_wrapped,
        scrollback_wrapped,
    }
}

const NODE_SCRIPT: &str = r#"
const [terminalDir, statesJson, restoresJson] = process.argv.slice(1);
const restore = await import(`${terminalDir}/terminal-restore.js`);
const hex = (bytes) => Buffer.from(bytes).toString("hex");
const frames = JSON.parse(statesJson).map((state, index) => {
  const snapshot = { state, revision: index };
  return [
    hex(restore.encodeTerminalRestoreFrame({ slot: index % 256, snapshot })),
    hex(restore.encodeLegacyTerminalSnapshotFrame({ slot: index % 256, snapshot })),
  ];
});
const shown = (value) => (value === undefined ? "undefined" : value);
const policies = JSON.parse(restoresJson).map((options) => {
  const input = options === null ? undefined : options;
  return [
    restore.resolveTerminalSubscriptionSnapshotMode(input),
    shown(restore.resolveRestoreAfterOutputOverflow(input)),
    input === undefined ? "skip" : shown(restore.resolveTerminalRestoreSnapshotOptions(input)),
  ];
});
process.stdout.write(JSON.stringify([
  frames,
  policies,
  restore.MAX_TERMINAL_OUTPUT_FRAME_BYTES,
  restore.MAX_CLIENT_BUFFERED_BYTES,
]));
"#;

fn hex(bytes: &[u8]) -> JsValue {
    JsValue::String(bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    }))
}

fn restore_inputs() -> Vec<Option<RestoreOptions>> {
    let mut inputs = vec![None];
    for mode in [
        RestoreMode::Live,
        RestoreMode::VisibleSnapshot,
        RestoreMode::FullSnapshot,
    ] {
        for scrollback_lines in [None, Some(0), Some(17), Some(500), Some(501), Some(9999)] {
            for size in [None, Some((30, 100))] {
                inputs.push(Some(RestoreOptions {
                    mode,
                    scrollback_lines,
                    size,
                }));
            }
        }
    }
    inputs
}

fn restore_value(options: Option<&RestoreOptions>) -> JsValue {
    let Some(options) = options else {
        return JsValue::String("undefined".to_owned());
    };
    let mut object = JsObject::new();
    let mode = match options.mode {
        RestoreMode::Live => "live",
        RestoreMode::VisibleSnapshot => "visible-snapshot",
        RestoreMode::FullSnapshot => "full-snapshot",
    };
    object.insert("mode", JsValue::String(mode.to_owned()));
    if let Some(lines) = options.scrollback_lines {
        object.insert("scrollbackLines", JsValue::Number(f64::from(lines)));
    }
    if let Some((rows, cols)) = options.size {
        let mut size = JsObject::new();
        size.insert("rows", JsValue::Number(f64::from(rows)));
        size.insert("cols", JsValue::Number(f64::from(cols)));
        object.insert("size", JsValue::Object(size));
    }
    JsValue::Object(object)
}

fn policy_value(options: Option<&RestoreOptions>) -> JsValue {
    let mode = match subscription_snapshot_mode(options) {
        SnapshotMode::State => "state",
        SnapshotMode::Ready => "ready",
    };
    let snapshot = match options.map(restore_snapshot_options) {
        None => JsValue::String("skip".to_owned()),
        Some(RestoreSnapshot::None) => JsValue::Null,
        Some(RestoreSnapshot::Full) => JsValue::String("undefined".to_owned()),
        Some(RestoreSnapshot::Options(options)) => {
            let mut object = JsObject::new();
            if let Some(lines) = options.scrollback_lines {
                object.insert("scrollbackLines", JsValue::Number(f64::from(lines)));
            }
            JsValue::Object(object)
        }
    };
    JsValue::Array(vec![
        JsValue::String(mode.to_owned()),
        restore_value(restore_after_output_overflow(options.copied()).as_ref()),
        snapshot,
    ])
}

#[test]
fn restore_frames_and_policy_match_pinned_terminal_restore() {
    let Some(pinned) = support::pinned("restore differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let mut rng = Seeded(0x0005_7a7e);
    let states: Vec<TerminalState> = (0..400).map(|_| seeded_state(&mut rng)).collect();
    let states_json = format!(
        "[{}]",
        states
            .iter()
            .map(
                |state| String::from_utf8(encode_terminal_snapshot(state).expect("json"))
                    .expect("utf8")
            )
            .collect::<Vec<_>>()
            .join(",")
    );
    let inputs = restore_inputs();
    let restores_json = stringify(&JsValue::Array(
        inputs
            .iter()
            .map(|input| match input {
                None => JsValue::Null,
                Some(options) => restore_value(Some(options)),
            })
            .collect(),
    ));
    let expected = support::run_node(&pinned, NODE_SCRIPT, &[&states_json, &restores_json]);

    let frames = states
        .iter()
        .enumerate()
        .map(|(index, state)| {
            let slot = u8::try_from(index % 256).expect("slot");
            JsValue::Array(vec![
                hex(&encode_restore_frame(slot, state)),
                hex(&encode_legacy_snapshot_frame(slot, state).expect("json")),
            ])
        })
        .collect();
    #[allow(clippy::cast_precision_loss)]
    let actual = stringify(&JsValue::Array(vec![
        JsValue::Array(frames),
        JsValue::Array(
            inputs
                .iter()
                .map(|input| policy_value(input.as_ref()))
                .collect(),
        ),
        JsValue::Number(spocky_terminal::restore::MAX_TERMINAL_OUTPUT_FRAME_BYTES as f64),
        JsValue::Number(spocky_terminal::restore::MAX_CLIENT_BUFFERED_BYTES as f64),
    ]));
    assert_eq!(actual, expected);
}
