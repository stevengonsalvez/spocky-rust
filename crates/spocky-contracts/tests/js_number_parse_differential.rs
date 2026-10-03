//! `Number(text)` differential: V8 in the pinned Node against `js_to_number`,
//! over the `StringToNumber` grammar (whitespace, signs, `Infinity`, `0x`,
//! `0o`, `0b`, decimal literals, separators that are not allowed) and over
//! generated decimal and radix literals whose rounding must agree. `-0` is
//! printed as `-0`, since `String(-0)` hides it.

#[path = "support/pinned_node.rs"]
mod support;

use spocky_contracts::js_value::{JsValue, stringify};
use spocky_contracts::number::{format_js_number, js_to_number};

const NODE_SCRIPT: &str = r#"
const [dist, file] = process.argv.slice(1);
const { readFileSync } = await import("node:fs");
const out = JSON.parse(readFileSync(file, "utf8")).map((text) => {
  const value = Number(text);
  return Object.is(value, -0) ? "-0" : String(value);
});
process.stdout.write(out.join("\n") + "\n");
"#;

const FIXED: &[&str] = &[
    "",
    " ",
    "\t\n\u{b}\u{c}\r \u{a0}\u{1680}\u{2000}\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}",
    "\u{85}",
    "\u{200b}",
    "\u{180e}",
    "0",
    "-0",
    "+0",
    "0.0",
    "-0.0",
    "00",
    "007",
    "-007",
    "1",
    "-1",
    "+1",
    "  12  ",
    "\u{feff}12\u{a0}",
    "\u{85}12",
    "12\u{200b}",
    "1.5",
    ".5",
    "5.",
    ".",
    "+.",
    "-.",
    "+.5",
    "-.5e1",
    "5.e1",
    ".e1",
    "e1",
    "1e",
    "1e+",
    "1e-",
    "1e1",
    "1E1",
    "1e+1",
    "1e-1",
    "1e00",
    "1e0001",
    "-1e-1",
    "1e308",
    "1e309",
    "-1e309",
    "1e-324",
    "1e-323",
    "5e-324",
    "2.4703282292062327e-324",
    "2.4703282292062328e-324",
    "1.7976931348623157e308",
    "1.7976931348623158e308",
    "1.7976931348623159e308",
    "9007199254740993",
    "9007199254740992",
    "9007199254740991",
    "18014398509481985",
    "0.1",
    "0.30000000000000004",
    "123456789012345678901234567890",
    "Infinity",
    "+Infinity",
    "-Infinity",
    "infinity",
    "INFINITY",
    "Infinit",
    "Infinity ",
    " -Infinity",
    "+-1",
    "-+1",
    "--1",
    "1-",
    "1+1",
    "1 1",
    "1,5",
    "1_000",
    "1__0",
    "_1",
    "1_",
    "NaN",
    "nan",
    "inf",
    "-inf",
    "0x",
    "0X",
    "0x0",
    "0xf",
    "0xF",
    "0xff",
    "0XFF",
    "0x1f",
    "0xg",
    "0x-1",
    "-0x1",
    "+0x1",
    "0x+1",
    "0x1.5",
    "0xffffffffffffffff",
    "0x10000000000000000",
    "0x1fffffffffffff",
    "0x20000000000001",
    "0x20000000000002",
    "0x20000000000003",
    "0x3fffffffffffffffff",
    "0x1ffffffffffffffffffffffffffffffffffffffff",
    "0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
    "0o",
    "0o7",
    "0o8",
    "0O17",
    "0o777777777777777777777",
    "0o-1",
    "0b",
    "0b1",
    "0B101",
    "0b2",
    "0b11111111111111111111111111111111111111111111111111111111",
    "0b1000000000000000000000000000000000000000000000000000010000000000000",
    "0b1000000000000000000000000000000000000000000000000000010000000000001",
    "0b1000000000000000000000000000000000000000000000000000110000000000000",
    "0b1000000000000000000000000000000000000000000000000000110000000000001",
    "0b111111111111111111111111111111111111111111111111111111111",
    "0b1000000000000000000000000000000000000000000000000000000000000000000",
    "0b10000000000000000000000000000000000000000000000000000000000000000000000000001",
    "0x0000000000000000000000000000000000000000000000000000000000000000001",
    "00x1",
    "0 x1",
    "0x 1",
    "\u{ff11}",
    "\u{661}\u{662}",
    "1\u{ff10}",
    "1.\u{ff10}",
    "\u{2212}1",
    "1\u{2e}5",
    "१२",
    "1e1.5",
    "1.5.5",
    "1..5",
    "1.e",
    "0.0000001",
    "0.000001",
    "1e21",
    "1e-7",
    "123e-20",
    "-123.456e+7",
    "0000000000000000000000000000000000000000001",
    "1.000000000000000000000000000000000000000001",
    "4.35",
    "4.35e1",
    "5e-1",
    "0.5e-1",
    "9.999999999999999e22",
    "9.9999999999999999e22",
    "1e23",
];

/// xorshift64*, so every run builds the same strings.
struct Random(u64);

impl Random {
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        usize::try_from(
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % u64::try_from(bound).expect("bound"),
        )
        .expect("index")
    }

    fn pick<'a>(&mut self, alphabet: &'a [char]) -> &'a char {
        &alphabet[self.below(alphabet.len())]
    }
}

fn digits(random: &mut Random, length: usize) -> String {
    let alphabet: Vec<char> = "0123456789".chars().collect();
    (0..length).map(|_| *random.pick(&alphabet)).collect()
}

fn generated() -> Vec<String> {
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    let mut out = Vec::new();
    // Decimal literals of every shape, long enough to need correct rounding.
    for _ in 0..3000 {
        let sign = ["", "+", "-"][random.below(3)];
        let integer_length = random.below(25);
        let fraction_length = random.below(25);
        let mut text = format!("{sign}{}", digits(&mut random, integer_length));
        if random.below(2) == 0 {
            text.push('.');
            let fraction = digits(&mut random, fraction_length);
            text.push_str(&fraction);
        }
        if random.below(2) == 0 {
            let exponent_sign = ["", "+", "-"][random.below(3)];
            let exponent_length = 1 + random.below(3);
            let exponent = digits(&mut random, exponent_length);
            text.push('e');
            text.push_str(exponent_sign);
            text.push_str(&exponent);
        }
        out.push(text);
    }
    // Radix literals past 53 and 64 bits.
    for (prefix, alphabet) in [
        ("0x", "0123456789abcdefABCDEF"),
        ("0o", "01234567"),
        ("0b", "01"),
    ] {
        let alphabet: Vec<char> = alphabet.chars().collect();
        for _ in 0..400 {
            let length = 1 + random.below(90);
            let body: String = (0..length).map(|_| *random.pick(&alphabet)).collect();
            out.push(format!("{prefix}{body}"));
        }
    }
    // Arbitrary short strings from the characters the grammar cares about.
    let alphabet: Vec<char> = "0123456789.eE+- xXoObBaAfF_Infity\t\u{a0}\u{feff}\u{85}"
        .chars()
        .collect();
    for _ in 0..3000 {
        let length = 1 + random.below(10);
        out.push((0..length).map(|_| *random.pick(&alphabet)).collect());
    }
    out
}

fn texts() -> Vec<String> {
    FIXED
        .iter()
        .map(|text| (*text).to_owned())
        .chain(generated())
        .collect()
}

fn print(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value.is_infinite() {
        if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
    } else if value == 0.0 && value.is_sign_negative() {
        "-0".to_owned()
    } else {
        format_js_number(value)
    }
}

#[test]
fn js_to_number_matches_v8() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    let texts = texts();
    let json = stringify(&JsValue::Array(
        texts.iter().cloned().map(JsValue::String).collect(),
    ));
    let file = std::env::temp_dir().join(format!("spocky-js-number-{}.json", std::process::id()));
    std::fs::write(&file, json).expect("write cases");
    let expected = support::run_node(&node, &dist, NODE_SCRIPT, &[file.display().to_string()]);
    std::fs::remove_file(&file).expect("remove cases");
    let actual: String = texts
        .iter()
        .map(|text| print(js_to_number(text)) + "\n")
        .collect();
    for (index, (node_line, rust_line)) in expected.lines().zip(actual.lines()).enumerate() {
        assert_eq!(
            rust_line, node_line,
            "case {index} ({:?}) differs",
            texts[index]
        );
    }
    assert_eq!(actual, expected);
}
