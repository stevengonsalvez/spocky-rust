//! `new URL(input, base)` differential: node v22.20.0 (ada 2.9.2) against
//! `Url::parse`, over the inputs of the Web Platform Tests `urltestdata.json`
//! and `IdnaTestV2.json` (see `fixtures/url-corpus.json` for the commits).
//! Only the inputs are vendored: the pinned node prints the expected `href`,
//! `origin`, `protocol`, `username`, `password`, `host`, `hostname`, `port`,
//! `pathname`, `search`, and `hash`, or `null` where it throws, and every
//! line must match.

//!
//! # Mutants that survive, and why each is equivalent
//!
//! Mutation runs against this test (default port, IDNA mapping and validity,
//! IPv4 and IPv6 rules, normalization order, the `ContextJ` early returns)
//! all fail it, except three that cannot change any output:
//!
//! - `parse_prepared_path` with its fast path disabled: ada's fast path (a
//!   special, non-file path with nothing to encode and no backslash or `%`)
//!   is a speed-up of the same segment loop the general path runs, so both
//!   build the same path for every input.
//! - `is_label_valid` without the `index == 0` test in the ZWNJ rule: with a
//!   ZWNJ first, the characters before it are an empty slice, no joining
//!   character is found there, and the rule returns false anyway; a lone ZWNJ
//!   is caught by the `index + 1 >= len` test that follows.
//! - `utf32_to_punycode` with the surrogate bound `0xd880` moved to `0xd800`:
//!   no surrogate reaches the encoder, because the UTS 46 mapping disallows
//!   surrogate code points and the input is UTF-8.

#[path = "support/pinned_node.rs"]
mod support;

use spocky_contracts::js_value::js_text_to_utf8;
use spocky_contracts::js_value::{JsValue, parse, stringify};
use spocky_contracts::url::Url;

const NODE_SCRIPT: &str = r#"
const [dist, file, generatedFile] = process.argv.slice(1);
const { readFileSync } = await import("node:fs");
const { cases } = JSON.parse(readFileSync(file, "utf8"));
const generated = JSON.parse(readFileSync(generatedFile, "utf8"));
const out = [...cases, ...generated].map(([input, base]) => {
  try {
    const url = base === null ? new URL(input) : new URL(input, base);
    return JSON.stringify([url.href, url.origin, url.protocol, url.username, url.password, url.host, url.hostname, url.port, url.pathname, url.search, url.hash]);
  } catch {
    return "null";
  }
});
process.stdout.write(out.join("\n") + "\n");
"#;

/// xorshift64*, so every run builds the same cases.
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

    fn pick<'a>(&mut self, pool: &[&'a str]) -> &'a str {
        pool[self.below(pool.len())]
    }
}

const SCHEMES: &[&str] = &[
    "http",
    "https",
    "ws",
    "wss",
    "ftp",
    "file",
    "blob",
    "data",
    "mailto",
    "foo",
    "HTTP",
    "Https",
    "a+b-c.d",
    "javascript",
    "",
];
const SEPARATORS: &[&str] = &[
    "://", ":", ":/", ":///", "//", "/", "\\\\", ":\\\\", "", ":////",
];
const USERINFOS: &[&str] = &[
    "",
    "",
    "",
    "user:pass@",
    "u@",
    "@",
    ":@",
    "a:b:c@",
    "us er@",
    "%41:%42@",
    "\u{e9}@",
    "a@b@",
];
const HOSTS: &[&str] = &[
    "example.com",
    "EXAMPLE.COM",
    "a.b.c",
    "a..b",
    "a.",
    ".",
    "xn--",
    "xn--9hb",
    "xn--a-ecp.ru",
    "XN--A-ECP.RU",
    "xn--zz",
    "xn--a-",
    "\u{661}.com",
    "\u{df}.de",
    "a\u{200c}b.com",
    "a\u{200d}b",
    "\u{200c}b",
    "b\u{200c}",
    "\u{200c}",
    "\u{200d}",
    "\u{200d}a",
    "\u{915}\u{94d}\u{200d}",
    "\u{628}\u{200c}\u{628}",
    "\u{628}\u{200c}\u{627}",
    "\u{a872}\u{200c}\u{628}",
    "\u{915}\u{94d}\u{200c}\u{915}",
    "1.2.3.4",
    "0x7f.1",
    "0X7F.0.0.1",
    "0300.0250.1.1",
    "1.2.3",
    "4294967295",
    "4294967296",
    "1.2.3.4.5",
    "256.1.1.1",
    "01.02.03.04",
    "0x",
    "0x.1",
    "[::1]",
    "[1:2:3:4:5:6:7:8]",
    "[::ffff:1.2.3.4]",
    "[1::2::3]",
    "[::",
    "::1]",
    "[]",
    "[fe80::1%25eth0]",
    "[0:0:0:0:0:0:0:0]",
    "[1:0:0:2:0:0:0:3]",
    "%41.com",
    "%zz",
    "a b",
    "a%20b",
    "a%00b",
    "%e2%80%8b",
    "%ff",
    "localhost",
    "LOCALHOST",
    "\u{e9}",
    "\u{e9}.com",
    "\u{ff21}\u{ff22}.com",
    "a\u{3002}b",
    "a\u{ff0e}b",
    "\u{5d0}\u{5d1}",
    "\u{5d0}1",
    "1\u{5d0}",
    "\u{627}\u{661}",
    "\u{627}\u{6f1}",
    "a\u{301}",
    "\u{301}a",
    "\u{ac00}",
    "\u{1100}\u{1161}",
    "\u{1f600}",
    "\u{10400}",
    "\u{a7ce}",
    "\u{104b0}",
    "foo_bar",
    "-a",
    "a-",
    "a--b",
    "xn--a--b",
    "\u{d7ff}",
    "\u{e000}",
    "\u{ffff}",
    "\u{10ffff}",
    "a/b",
    "a?b",
    "a#b",
    "a@b",
    "a:b",
    "a\\b",
    "a<b",
    "a>b",
    "a^b",
    "a|b",
    "a[b",
];
const PORTS: &[&str] = &[
    "", "", "", ":", ":0", ":80", ":443", ":8080", ":65535", ":65536", ":99999", ":00080", ":-1",
    ":a", ":80a", ":8 0", ":\u{661}",
];
const PATHS: &[&str] = &[
    "",
    "/",
    "/a",
    "/a/b",
    "/a/b/",
    "/a/../b",
    "/a/./b",
    "/../",
    "/./",
    "/..",
    "/.",
    "/%2e/",
    "/%2E%2e/",
    "/a/%2e%2E/b",
    "/.%2e",
    "/%2e.",
    "\\a\\b",
    "/a\\b",
    "/a b",
    "/a%20b",
    "/\u{e9}",
    "/a%",
    "/a%zz",
    "/{}",
    "/`",
    "/a/b/c/../../d",
    "//",
    "///",
    "/a//b",
    "/C:/x",
    "/c|/x",
    "/C:",
    "/C|",
    "C:",
    "C|/x",
    "/a/b/%2e%2e/%2E%2E/c",
    "/?",
    "/#",
    "/\u{1f600}",
    "/\u{d7ff}",
];
const QUERIES: &[&str] = &[
    "", "", "?", "?a=b", "?a b", "?a\"b", "?a'b", "?a<b>", "?\u{e9}", "?%", "?#", "??", "?a#b",
];
const FRAGMENTS: &[&str] = &[
    "", "", "#", "#a", "#a b", "#a`b", "#\u{e9}", "#%", "##", "#?", "#\u{0}", "#a\u{1}b",
];
const RELATIVES: &[&str] = &[
    "",
    "/",
    "//",
    "///",
    "////",
    "//host",
    "///host",
    "/path",
    "path",
    "./",
    "../",
    "../..",
    "?q",
    "#f",
    "?",
    "#",
    " ",
    "\t",
    "\n",
    "\r\n",
    "  http://a/  ",
    "a:b",
    "A:",
    "http:",
    "http:/",
    "http://",
    "https:a",
    "file:",
    "file:a",
    "file:/a",
    "file://",
    "file:///",
    "file://h/",
    "file:c:",
    "file:c|",
    "file:/c:",
    "file:///c|/x",
    "\\\\",
    "\\",
    "/\\",
    "\\/",
    "\\\\host",
    "//\\host",
];
const BASES: &[&str] = &[
    "http://example.com/a/b/c?d#e",
    "https://u:p@example.com:8443/x/y",
    "ws://h/p",
    "ftp://f/",
    "file:///C:/a/b",
    "file://host/share/x",
    "file:///a/b",
    "blob:https://example.com/uuid",
    "blob:null/uuid",
    "data:text/plain,hello",
    "mailto:a@b.c",
    "foo://h/a/b?q",
    "foo:/a/b",
    "foo:a/b",
    "http://\u{e9}.com/",
    "http://[::1]:8080/a",
    "javascript:alert(1)",
    "about:blank",
    "http://example.com/a b/%2e",
];
const STRAYS: &[char] = &[
    '\u{0}', '\u{1}', '\t', '\n', '\r', ' ', '#', '?', '/', '\\', '@', ':', '[', ']', '%', '.',
    '\u{e9}', '\u{661}', '\u{200c}',
];

/// Hosts that pin ada's `ContextJ` and Bidi order. ada's `is_label_valid`
/// returns from inside the ZWNJ/ZWJ rules, so a label whose joiner follows a
/// virama (or sits between joining characters) is accepted without the Bidi
/// rule ever running, where UTS 46 would reject it: `a`, a virama, a joiner,
/// and a Hebrew letter mix LTR and RTL; Arabic letters around a ZWNJ with both
/// European and Arabic-Indic digits break rule 4. Joiners with no rule to
/// satisfy (first, last, after a non-virama) are rejected.
const CONTEXT_J_CASES: &[&str] = &[
    "https://a\u{94d}\u{200c}\u{5d0}b.com/",
    "https://a\u{94d}\u{200d}\u{5d0}b.com/",
    "https://\u{628}\u{200c}\u{627}1\u{661}\u{627}.com/",
    "https://\u{628}\u{200c}\u{628}1\u{661}\u{627}.com/",
    "https://\u{915}\u{94d}\u{200c}1\u{5d0}b.com/",
    "https://\u{915}\u{94d}\u{200d}1\u{661}b.com/",
    "https://a\u{94d}\u{200c}\u{5d0}.com/",
    "https://a\u{200c}\u{5d0}b.com/",
    "https://a\u{200d}\u{5d0}b.com/",
    "https://\u{200c}\u{5d0}.com/",
    "https://\u{5d0}\u{200c}.com/",
    "https://\u{628}\u{200c}\u{627}.com/",
    "https://\u{628}\u{200c}\u{628}.com/",
    "https://\u{627}\u{200c}\u{627}.com/",
    "https://\u{a872}\u{200c}\u{628}.com/",
];

/// Generated `(input, base)` pairs: absolute URLs from the pools above,
/// relative references against a base, and short strings of delimiters.
fn generated_cases() -> Vec<(String, Option<String>)> {
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    let mut cases: Vec<(String, Option<String>)> = CONTEXT_J_CASES
        .iter()
        .map(|input| ((*input).to_owned(), None))
        .collect();
    for _ in 0..12_000 {
        let mut input = String::new();
        input.push_str(random.pick(SCHEMES));
        input.push_str(random.pick(SEPARATORS));
        input.push_str(random.pick(USERINFOS));
        input.push_str(random.pick(HOSTS));
        input.push_str(random.pick(PORTS));
        input.push_str(random.pick(PATHS));
        input.push_str(random.pick(QUERIES));
        input.push_str(random.pick(FRAGMENTS));
        cases.push((input, None));
    }
    for _ in 0..8_000 {
        let mut input = String::new();
        match random.below(3) {
            0 => input.push_str(random.pick(RELATIVES)),
            1 => {
                input.push_str(random.pick(RELATIVES));
                input.push_str(random.pick(HOSTS));
                input.push_str(random.pick(PATHS));
            }
            _ => {
                input.push_str(random.pick(PATHS));
                input.push_str(random.pick(QUERIES));
                input.push_str(random.pick(FRAGMENTS));
            }
        }
        cases.push((input, Some(random.pick(BASES).to_owned())));
    }
    for _ in 0..6_000 {
        let length = 1 + random.below(14);
        let mut input = String::new();
        if random.below(2) == 0 {
            input.push_str(random.pick(&[
                "http://", "https://", "file://", "foo://", "http:", "/", "//",
            ]));
        }
        for _ in 0..length {
            if random.below(3) == 0 {
                input.push(STRAYS[random.below(STRAYS.len())]);
            } else {
                input.push_str(random.pick(&[
                    "a", "b", "1", "0", "9", "A", "x", "e", "f", "-", "_", "+", "xn--", ".", "%41",
                    "%2e", "%zz",
                ]));
            }
        }
        let base = (random.below(2) == 0).then(|| random.pick(BASES).to_owned());
        cases.push((input, base));
    }
    cases
}

fn rust_line(input: &str, base: Option<&str>) -> String {
    match Url::parse(input, base) {
        None => "null".to_owned(),
        Some(url) => stringify(&JsValue::Array(
            [
                url.href(),
                url.origin(),
                url.protocol(),
                url.username(),
                url.password(),
                url.host(),
                url.hostname(),
                url.port(),
                url.pathname(),
                url.search(),
                url.hash(),
            ]
            .into_iter()
            .map(JsValue::String)
            .collect(),
        )),
    }
}

#[test]
fn url_parsing_matches_node() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    let file =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/url-corpus.json");
    let generated = generated_cases();
    let generated_json = stringify(&JsValue::Array(
        generated
            .iter()
            .map(|(input, base)| {
                JsValue::Array(vec![
                    JsValue::String(input.clone()),
                    base.clone().map_or(JsValue::Null, JsValue::String),
                ])
            })
            .collect(),
    ));
    let generated_file =
        std::env::temp_dir().join(format!("spocky-url-cases-{}.json", std::process::id()));
    std::fs::write(&generated_file, generated_json).expect("write generated cases");
    let expected = support::run_node(
        &node,
        &dist,
        NODE_SCRIPT,
        &[
            file.display().to_string(),
            generated_file.display().to_string(),
        ],
    );
    std::fs::remove_file(&generated_file).expect("remove generated cases");
    let corpus = parse(&std::fs::read_to_string(&file).expect("corpus")).expect("corpus JSON");
    let cases = corpus
        .get("cases")
        .and_then(JsValue::as_array)
        .expect("cases");
    // A lone surrogate is U+FFFD once it reaches a USVString, as `new URL` takes it.
    let usv = |value: &JsValue| value.as_str().map(js_text_to_utf8);
    let mut mismatches = Vec::new();
    let mut lines = expected.lines();
    let fixture = cases.iter().map(|case| {
        let pair = case.as_array().expect("case");
        (usv(&pair[0]).expect("input"), usv(&pair[1]))
    });
    let all: Vec<(String, Option<String>)> = fixture.chain(generated).collect();
    for (index, (input, base)) in all.iter().enumerate() {
        let actual = rust_line(input, base.as_deref());
        let wanted = lines.next().expect("one line per case");
        if actual != wanted {
            mismatches.push(format!(
                "case {index} {input:?} base {base:?}\n  node: {wanted}\n  rust: {actual}"
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} of {} cases differ:\n{}",
        mismatches.len(),
        all.len(),
        mismatches
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
