//! Seeded byte fuzz differential: random PTY byte streams (text, wide and
//! combining characters, invalid UTF-8, C0 and C1 controls, CSI, SGR, DEC
//! modes, ESC, OSC and DCS sequences) cut into random chunks with resizes in
//! between, run through the pinned xterm and through `spocky-xterm`, compared
//! as `JSON.stringify` text per scenario. Nothing is normalized.
//!
//! `SPOCKY_XTERM_FUZZ_SEEDS` (default 400) and `SPOCKY_XTERM_FUZZ_START`
//! (default 0) choose the seeds. A second, biased mode aims at xterm's
//! exception paths: it puts the cursor at the bottom right of a fresh
//! screen, erases above it with `CSI 1 J` (which throws there), and then
//! writes and resizes against the wedged terminal; `SPOCKY_XTERM_BIASED_SEEDS`
//! (default 200) and `SPOCKY_XTERM_BIASED_START` (default 0) choose its seeds.
//! See `common/mod.rs` for the environment.

mod common;

use spocky_contracts::js_value::{JsObject, JsValue, stringify};

/// `SplitMix64`: small, seedable, and the same on every platform.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn range(&mut self, low: u64, high: u64) -> u64 {
        low + self.below(high - low + 1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[usize::try_from(self.below(items.len() as u64)).unwrap_or(0)]
    }
}

fn push_char(out: &mut Vec<u8>, code: u32) {
    if let Some(character) = char::from_u32(code) {
        let mut buffer = [0; 4];
        out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
    }
}

fn unicode(rng: &mut Rng, out: &mut Vec<u8>) {
    const SPECIAL: &[u32] = &[
        0x00e9, 0x00a0, 0x0300, 0x0301, 0x036f, 0x0483, 0x0e31, 0x1160, 0x200b, 0x200d, 0x2028,
        0x2329, 0x3000, 0x303f, 0x4e2d, 0xac00, 0xfe0f, 0xfe20, 0xfeff, 0xff01, 0xffe0, 0x20d0,
        0x1f600, 0x1f468, 0x10a01, 0x20000, 0x2fffd, 0x30000, 0xe0001, 0xe0100, 0x2764,
    ];
    let code = match rng.below(4) {
        0 => rng.pick(SPECIAL),
        1 => u32::try_from(rng.range(0x4e00, 0x9fff)).unwrap_or(0x4e00),
        2 => u32::try_from(rng.range(0xa0, 0xd7ff)).unwrap_or(0xa0),
        _ => u32::try_from(rng.range(0x1_0000, 0x3_ffff)).unwrap_or(0x1_0000),
    };
    push_char(out, code);
}

fn number_text(rng: &mut Rng, large: bool) -> String {
    match rng.below(10) {
        0 => String::new(),
        1..=6 => rng.below(13).to_string(),
        7 | 8 => rng.range(13, 120).to_string(),
        _ if large => rng.range(1000, 9_999_999_999).to_string(),
        _ => rng.range(120, 300).to_string(),
    }
}

fn params_text(rng: &mut Rng, large: bool) -> String {
    let count = rng.below(5);
    let mut text = String::new();
    for index in 0..count {
        if index > 0 {
            text.push(';');
        }
        text.push_str(&number_text(rng, large));
        if rng.chance(8) {
            for _ in 0..rng.range(1, 4) {
                text.push(':');
                text.push_str(&number_text(rng, large));
            }
        }
    }
    text
}

fn csi(rng: &mut Rng, out: &mut Vec<u8>) {
    const FINALS: &[u8] = b"@ABCDEFGHIJKLMPSTXZ`abcdefghlmnqrstu}~JKhl";
    // Finals whose count is a loop over lines or characters in xterm.
    const LOOPING: &[u8] = b"LMSTbIZ";
    out.extend_from_slice(b"\x1b[");
    match rng.below(10) {
        0 => out.push(b'?'),
        1 => out.push(rng.pick(b"<=>")),
        _ => {}
    }
    let final_byte = if rng.chance(85) {
        rng.pick(FINALS)
    } else {
        u8::try_from(rng.range(0x40, 0x7e)).unwrap_or(b'@')
    };
    let text = params_text(rng, !LOOPING.contains(&final_byte));
    out.extend_from_slice(text.as_bytes());
    if rng.chance(12) {
        out.push(rng.pick(b" !$\"'#"));
    }
    out.push(final_byte);
}

fn sgr(rng: &mut Rng, out: &mut Vec<u8>) {
    const PARTS: &[&str] = &[
        "0",
        "1",
        "2",
        "3",
        "4",
        "4:0",
        "4:3",
        "4:-1",
        "5",
        "7",
        "8",
        "9",
        "21",
        "22",
        "23",
        "24",
        "25",
        "27",
        "28",
        "29",
        "31",
        "37",
        "39",
        "41",
        "47",
        "49",
        "53",
        "55",
        "59",
        "92",
        "97",
        "101",
        "107",
        "38;5;1",
        "38;5;200",
        "48;5;17",
        "38;2;1;2;3",
        "48;2;255;0;128",
        "38:2::10:20:30",
        "38:5:44",
        "48:2:1:2:3",
        "58;5;9",
        "58:2::1:2:3",
        "38;2;1;2",
        "38;5",
        "38",
        "48;2",
        "38;3;1;2;3;4;5",
        "",
    ];
    out.extend_from_slice(b"\x1b[");
    let count = rng.range(1, 4);
    for index in 0..count {
        if index > 0 {
            out.push(b';');
        }
        out.extend_from_slice(rng.pick(PARTS).as_bytes());
    }
    out.push(b'm');
}

fn dec_mode(rng: &mut Rng, out: &mut Vec<u8>) {
    const MODES: &[&str] = &[
        "1", "2", "3", "6", "7", "9", "12", "25", "45", "47", "66", "1000", "1004", "1006", "1047",
        "1048", "1049", "2004", "2026", "7;25", "1049;6", "4",
    ];
    out.extend_from_slice(if rng.chance(85) { b"\x1b[?" } else { b"\x1b[" });
    out.extend_from_slice(rng.pick(MODES).as_bytes());
    out.push(if rng.chance(50) { b'h' } else { b'l' });
}

fn esc(rng: &mut Rng, out: &mut Vec<u8>) {
    const SEQUENCES: &[&str] = &[
        "7", "8", "D", "E", "H", "M", "=", ">", "\\", "n", "o", "|", "}", "~", "(0", "(B", "(A",
        ")0", "*4", "+5", "-C", ".R", "/K", "%G", "%@", "#8", "(X", "((0", "(<", " F", "Z", "N",
    ];
    out.push(0x1b);
    if rng.chance(4) {
        out.push(b'c');
    } else {
        out.extend_from_slice(rng.pick(SEQUENCES).as_bytes());
    }
}

fn osc(rng: &mut Rng, out: &mut Vec<u8>) {
    const IDS: &[&str] = &[
        "0",
        "1",
        "2",
        "4",
        "8",
        "10",
        "11",
        "12",
        "104",
        "110",
        "633",
        "7",
        "",
        "x",
        "00002",
        "99999999999999999999",
    ];
    const PAYLOADS: &[&str] = &[
        "?",
        " ? ",
        "\u{a0}?\u{3000}",
        "title \u{fc}n\u{ef}",
        "D",
        "D;0",
        "D;-5",
        "D;007",
        "D;x",
        "D;1;2",
        "-0",
        "D;-0",
        ";http://x",
        "id=a;http://y",
        "id=a;",
        "",
        "  ;",
        "rgb:1/2/3",
        "1;?",
        "a;b;c",
        "\u{7f}",
    ];
    out.extend_from_slice(if rng.chance(90) {
        b"\x1b]"
    } else {
        "\u{9d}".as_bytes()
    });
    out.extend_from_slice(rng.pick(IDS).as_bytes());
    if rng.chance(90) {
        out.push(b';');
    }
    if rng.chance(70) {
        out.extend_from_slice(rng.pick(PAYLOADS).as_bytes());
    } else {
        for _ in 0..rng.range(0, 12) {
            out.push(u8::try_from(rng.range(0x20, 0x7e)).unwrap_or(b' '));
        }
    }
    let terminator: &[u8] = match rng.below(7) {
        0..=2 => b"\x07",
        3 | 4 => b"\x1b\\",
        5 => "\u{9c}".as_bytes(),
        _ => rng.pick(&[&b"\x18"[..], b"\x1a", b"\x1b[", b"\x01", b""]),
    };
    out.extend_from_slice(terminator);
}

fn dcs(rng: &mut Rng, out: &mut Vec<u8>) {
    const BODIES: &[&str] = &[
        "$qm",
        "$q\"q",
        "$qr",
        "$q q",
        "1;2|abc",
        "q#0;2;0;0;0",
        "+q544e",
        ">|x",
    ];
    out.extend_from_slice(rng.pick(&[
        &b"\x1bP"[..],
        "\u{90}".as_bytes(),
        b"\x1bX",
        b"\x1b^",
        b"\x1b_",
    ]));
    out.extend_from_slice(rng.pick(BODIES).as_bytes());
    out.extend_from_slice(rng.pick(&[&b"\x1b\\"[..], b"\x07", b"\x18", "\u{9c}".as_bytes()]));
}

fn token(rng: &mut Rng, out: &mut Vec<u8>) {
    match rng.below(100) {
        0..=21 => {
            let length = if rng.chance(15) {
                rng.range(20, 90)
            } else {
                rng.range(1, 12)
            };
            for _ in 0..length {
                out.push(u8::try_from(rng.range(0x20, 0x7e)).unwrap_or(b'x'));
            }
        }
        22..=31 => {
            for _ in 0..rng.range(1, 4) {
                unicode(rng, out);
            }
        }
        32..=35 => {
            if rng.chance(50) {
                out.push(rng.pick(&[0xe4, 0xf0, 0xc3, 0xed, 0xe0, 0xf4]));
            } else {
                for _ in 0..rng.range(1, 3) {
                    out.push(u8::try_from(rng.range(0x80, 0xff)).unwrap_or(0xff));
                }
            }
        }
        36..=47 => out.extend_from_slice(rng.pick(&[
            &b"\r\n"[..],
            b"\n",
            b"\r",
            b"\t",
            b"\x08",
            b"\x0b",
            b"\x0c",
            b"\x0e",
            b"\x0f",
            b"\x07",
            b"\x00",
            b"\x7f",
            b"\x18",
            b"\x1a",
            b"\x1b",
            b"\r\n\r\n",
        ])),
        48..=50 => {
            out.push(0xc2);
            out.push(rng.pick(&[
                0x84, 0x85, 0x88, 0x9b, 0x9d, 0x90, 0x9c, 0x98, 0x9e, 0x9f, 0x80, 0x99,
            ]));
        }
        51..=72 => csi(rng, out),
        73..=80 => sgr(rng, out),
        81..=85 => dec_mode(rng, out),
        86..=91 => esc(rng, out),
        92..=96 => osc(rng, out),
        97 | 98 => dcs(rng, out),
        _ => {
            let count = rng.range(5, 40);
            for index in 0..count {
                out.extend_from_slice(format!("ln{index}\r\n").as_bytes());
            }
        }
    }
}

fn number(value: u64) -> JsValue {
    #[allow(clippy::cast_precision_loss)]
    JsValue::Number(value as f64)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// Cuts `stream` into random chunks with resizes between some of them.
fn chunk_ops(rng: &mut Rng, stream: &[u8], resize_percent: u64) -> Vec<JsValue> {
    let mut ops = Vec::new();
    let mut position = 0;
    while position < stream.len() {
        if rng.chance(resize_percent) {
            let size = vec![number(rng.range(1, 30)), number(rng.range(1, 12))];
            let mut op = JsObject::new();
            op.insert("resize", JsValue::Array(size));
            ops.push(JsValue::Object(op));
        }
        let length = if rng.chance(70) {
            rng.range(1, 24)
        } else {
            rng.range(25, 400)
        };
        let end = (position + usize::try_from(length).unwrap_or(1)).min(stream.len());
        let mut op = JsObject::new();
        op.insert("hex", JsValue::String(hex(&stream[position..end])));
        ops.push(JsValue::Object(op));
        position = end;
    }
    ops
}

fn scenario(seed: u64) -> JsValue {
    let mut rng = Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ 0x5EED);
    let (cols, rows) = if rng.chance(5) {
        (80, 24)
    } else {
        (rng.range(2, 24), rng.range(1, 10))
    };
    let mut stream = Vec::new();
    for _ in 0..rng.range(4, 60) {
        token(&mut rng, &mut stream);
    }
    if rng.chance(2) {
        // Overflow the 1000 line scrollback on a narrow screen.
        for index in 0..rng.range(1000, 1100) {
            stream.extend_from_slice(format!("{index}\r\n").as_bytes());
            if index % 97 == 0 {
                token(&mut rng, &mut stream);
            }
        }
    }
    let ops = chunk_ops(&mut rng, &stream, 10);
    let mut object = JsObject::new();
    object.insert("name", JsValue::String(format!("seed-{seed}")));
    object.insert("rows", number(rows));
    object.insert("cols", number(cols));
    object.insert("ops", JsValue::Array(ops));
    JsValue::Object(object)
}

/// A scenario aimed at the exception paths: a cursor at the bottom right of
/// a screen with no scrollback, `CSI 1 J` or `CSI ? 1 J` (which indexes the
/// line below the cursor and throws when none exists), then random writes
/// and many resizes against whatever state that left.
fn biased_scenario(seed: u64) -> JsValue {
    let mut rng = Rng(seed.wrapping_mul(0x2545_F491_4F6C_DD1D) ^ 0x00B1_A5ED);
    let cols = rng.range(2, 24);
    let rows = rng.range(1, 10);
    let mut stream = Vec::new();
    let mode = rng.below(6);
    if mode == 3 {
        stream.extend_from_slice(b"\x1b[?1049h");
    }
    if mode == 2 {
        // Scroll first: with scrollback the line below exists, no throw.
        for index in 0..rows + rng.range(1, 6) {
            stream.extend_from_slice(format!("{index}\r\n").as_bytes());
        }
    }
    let column = match mode {
        1 => rng.range(1, cols),
        _ => cols + rng.below(3),
    };
    let row = if mode == 4 {
        rows.saturating_sub(1).max(1)
    } else {
        rows
    };
    stream.extend_from_slice(format!("\x1b[{row};{column}H").as_bytes());
    if rng.chance(30) {
        stream.extend_from_slice(b"x");
    }
    stream.extend_from_slice(if rng.chance(25) {
        b"\x1b[?1J"
    } else {
        b"\x1b[1J"
    });
    for _ in 0..rng.range(2, 24) {
        token(&mut rng, &mut stream);
    }
    let ops = chunk_ops(&mut rng, &stream, 30);
    let mut object = JsObject::new();
    object.insert("name", JsValue::String(format!("biased-{seed}")));
    object.insert("rows", number(rows));
    object.insert("cols", number(cols));
    object.insert("ops", JsValue::Array(ops));
    JsValue::Object(object)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .map_or(default, |value| value.parse().expect(name))
}

/// Runs seeds `start..start + count` of `make` through both emulators and
/// returns how many scenarios wedged xterm.
fn run_differential(label: &str, start: u64, count: u64, make: fn(u64) -> JsValue) -> usize {
    let Some((node, paseo_root)) = common::pinned() else {
        return 0;
    };
    let scenarios: Vec<JsValue> = (start..start + count).map(make).collect();
    let mut corpus = JsObject::new();
    corpus.insert("scenarios", JsValue::Array(scenarios.clone()));
    let corpus_text = stringify(&JsValue::Object(corpus));
    let directory = std::env::temp_dir();
    let tag = format!("{}-{}", label.replace(' ', "-"), std::process::id());
    let corpus_path = directory.join(format!("spocky-xterm-{tag}.json"));
    let out = directory.join(format!("spocky-xterm-{tag}.jsonl"));
    std::fs::write(&corpus_path, &corpus_text).expect("write fuzz corpus");
    let timeout = u32::try_from(120 + count / 2).unwrap_or(u32::MAX);
    let captured = common::capture(&node, &paseo_root, &corpus_path, &out, timeout);
    let _ = std::fs::remove_file(&corpus_path);
    let _ = std::fs::remove_file(&out);
    assert_eq!(
        captured.len(),
        scenarios.len(),
        "one capture line per scenario"
    );
    let mut failures = Vec::new();
    let (mut wedged, mut resize_errors) = (0, 0);
    for (scenario, expected) in scenarios.iter().zip(&captured) {
        if expected.contains("\"wedged\":true") {
            wedged += 1;
        }
        if expected.contains("\"resizeErrors\":") {
            resize_errors += 1;
        }
        let actual = common::replay(scenario);
        if &actual != expected {
            let name = scenario
                .get("name")
                .and_then(JsValue::as_str)
                .unwrap_or("?");
            failures.push(format!(
                "{name}: {}",
                common::first_difference(expected, &actual)
            ));
        }
    }
    eprintln!(
        "{label} seeds {start}..{}: {} of {count} full matches, corpus sha256 {}, {wedged} wedged, {resize_errors} with resize errors",
        start + count,
        count - failures.len() as u64,
        common::sha256_hex(corpus_text.as_bytes()),
    );
    assert!(
        failures.is_empty(),
        "{} of {count} seeds differ:\n{}",
        failures.len(),
        failures
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    wedged
}

#[test]
fn seeded_byte_fuzz_matches_pinned_xterm() {
    let start = env_u64("SPOCKY_XTERM_FUZZ_START", 0);
    let count = env_u64("SPOCKY_XTERM_FUZZ_SEEDS", 400);
    run_differential("fuzz", start, count, scenario);
}

#[test]
fn biased_exception_fuzz_matches_pinned_xterm() {
    let start = env_u64("SPOCKY_XTERM_BIASED_START", 0);
    let count = env_u64("SPOCKY_XTERM_BIASED_SEEDS", 200);
    let wedged = run_differential("biased fuzz", start, count, biased_scenario);
    // Skipped runs return 0; a real run of this size must reach the throw.
    if count >= 20 && std::env::var_os("SPOCKY_PINNED_NODE").is_some() {
        assert!(wedged > 0, "the biased mode never wedged xterm");
    }
}
