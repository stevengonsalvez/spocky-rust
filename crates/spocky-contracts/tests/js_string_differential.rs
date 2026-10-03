//! `String.prototype.trimStart`, `trimEnd`, and `toLowerCase` differential:
//! V8 in the pinned Node against `js_trim_start`, `js_trim_end`, and
//! `js_to_lowercase`.
//!
//! `toLowerCase` is checked for every Unicode scalar value (the set that
//! changes must be identical, with the same mapped code points, so a Unicode
//! table that moves ahead of node's fails here) and in context: `Final_Sigma`
//! with case-ignorable characters between, characters with `SpecialCasing`
//! mappings, supplementary-plane letters, and lone surrogates.

#[path = "support/pinned_node.rs"]
mod support;

use std::fmt::Write as _;

use spocky_contracts::js_value::{JsValue, parse, stringify};
use spocky_contracts::text::{js_to_lowercase, js_trim_end, js_trim_start};

const NODE_SCRIPT: &str = r#"
const [dist, file] = process.argv.slice(1);
const { readFileSync } = await import("node:fs");
const { trims, contexts } = JSON.parse(readFileSync(file, "utf8"));
const out = [];
for (const text of trims) out.push(JSON.stringify(text.trimStart()) + "|" + JSON.stringify(text.trimEnd()));
for (let code = 0; code <= 0x10ffff; code++) {
  if (code >= 0xd800 && code <= 0xdfff) continue;
  const text = String.fromCodePoint(code);
  const lower = text.toLowerCase();
  if (lower !== text) out.push(code.toString(16) + ":" + [...lower].map((c) => c.codePointAt(0).toString(16)).join(","));
}
for (const text of contexts) out.push(JSON.stringify(text.toLowerCase()));
process.stdout.write(out.join("\n") + "\n");
"#;

/// Every character `String.prototype.trim` removes, and look-alikes it does
/// not (`U+0085`, `U+180E`, `U+200B`, `U+2060`), as JSON escapes.
const TRIM_CHARS: &[&str] = &[
    "\\t",
    "\\n",
    "\\u000b",
    "\\u000c",
    "\\r",
    " ",
    "\\u00a0",
    "\\u1680",
    "\\u2000",
    "\\u2001",
    "\\u2009",
    "\\u200a",
    "\\u2028",
    "\\u2029",
    "\\u202f",
    "\\u205f",
    "\\u3000",
    "\\ufeff",
    "\\u0085",
    "\\u180e",
    "\\u200b",
    "\\u2060",
    "\\u001f",
    "\\u0021",
    "a",
    "Z",
    "0",
    "\\u00e9",
    "\\ud83d\\ude00",
    "\\ud800",
    "\\udc00",
];

/// Characters whose case mapping depends on context or on `SpecialCasing`.
const CONTEXT_CHARS: &[&str] = &[
    "A",
    "a",
    "\\u03a3",
    "\\u03c3",
    "\\u03c2",
    ".",
    "'",
    "\\u0301",
    "1",
    " ",
    "\\u0130",
    "\\u01c5",
    "\\ud801\\udc00",
    "\\ud801\\udc28",
    "\\u00ad",
    "\\u200d",
    "\\u03a9",
    "\\u00df",
    "\\u1e9e",
    "\\u2126",
    "\\u212a",
    "\\u03d2",
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
}

fn json_strings(parts: &[Vec<&str>]) -> String {
    let items: Vec<String> = parts
        .iter()
        .map(|chars| format!("\"{}\"", chars.concat()))
        .collect();
    format!("[{}]", items.join(","))
}

fn trims() -> Vec<Vec<&'static str>> {
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    let mut out: Vec<Vec<&'static str>> = vec![vec![]];
    for character in TRIM_CHARS {
        out.push(vec![character]);
    }
    for _ in 0..3000 {
        let length = random.below(9);
        out.push(
            (0..length)
                .map(|_| TRIM_CHARS[random.below(TRIM_CHARS.len())])
                .collect(),
        );
    }
    out
}

fn contexts() -> Vec<Vec<&'static str>> {
    let mut random = Random(0x1234_5678_9ABC_DEF1);
    let mut out: Vec<Vec<&'static str>> = Vec::new();
    let mut frontier: Vec<Vec<&'static str>> = vec![vec![]];
    for _ in 0..3 {
        let mut next = Vec::new();
        for prefix in &frontier {
            for character in CONTEXT_CHARS {
                let mut combined = prefix.clone();
                combined.push(character);
                next.push(combined);
            }
        }
        out.extend(next.iter().cloned());
        frontier = next;
    }
    // Longer strings, where a sigma has several case-ignorable neighbours.
    for _ in 0..20_000 {
        let length = 4 + random.below(4);
        out.push(
            (0..length)
                .map(|_| CONTEXT_CHARS[random.below(CONTEXT_CHARS.len())])
                .collect(),
        );
    }
    // Lone surrogates between a cased letter and a sigma, and around it.
    for text in [
        vec!["a", "\\ud800", "\\u03a3"],
        vec!["\\u03a3", "\\udc00", "a"],
        vec!["a", "\\udc00", "\\u03a3", "\\ud800", "b"],
        vec!["\\ud800", "\\u03a3"],
        vec!["\\u03a3", "\\ud800"],
    ] {
        out.push(text);
    }
    out
}

fn text_of(json: &str) -> String {
    let value = parse(&format!("\"{json}\"")).expect("string JSON");
    value.as_str().expect("a string").to_owned()
}

fn quoted(text: String) -> String {
    stringify(&JsValue::String(text))
}

fn rust_output(trims: &[Vec<&str>], contexts: &[Vec<&str>]) -> String {
    let mut out = String::new();
    for chars in trims {
        let text = text_of(&chars.concat());
        out.push_str(&quoted(js_trim_start(&text).to_owned()));
        out.push('|');
        out.push_str(&quoted(js_trim_end(&text).to_owned()));
        out.push('\n');
    }
    for code in 0..=0x0010_FFFF_u32 {
        let Some(character) = char::from_u32(code) else {
            continue;
        };
        let text = character.to_string();
        let lower = js_to_lowercase(&text);
        if lower != text {
            let mapped: Vec<String> = lower
                .chars()
                .map(|c| format!("{:x}", u32::from(c)))
                .collect();
            writeln!(out, "{code:x}:{}", mapped.join(",")).expect("write to a string");
        }
    }
    for chars in contexts {
        out.push_str(&quoted(js_to_lowercase(&text_of(&chars.concat()))));
        out.push('\n');
    }
    out
}

#[test]
fn trim_ends_and_lowercase_match_v8() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    let (trims, contexts) = (trims(), contexts());
    let file = std::env::temp_dir().join(format!("spocky-js-string-{}.json", std::process::id()));
    std::fs::write(
        &file,
        format!(
            r#"{{"trims":{},"contexts":{}}}"#,
            json_strings(&trims),
            json_strings(&contexts)
        ),
    )
    .expect("write cases");
    let expected = support::run_node(&node, &dist, NODE_SCRIPT, &[file.display().to_string()]);
    std::fs::remove_file(&file).expect("remove cases");
    let actual = rust_output(&trims, &contexts);
    for (index, (node_line, rust_line)) in expected.lines().zip(actual.lines()).enumerate() {
        assert_eq!(rust_line, node_line, "line {index} differs");
    }
    assert_eq!(actual.lines().count(), expected.lines().count());
    assert_eq!(actual, expected);
}
