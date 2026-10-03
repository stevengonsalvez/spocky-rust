//! `URLSearchParams` differential: node v22.20.0 against `UrlSearchParams`
//! and `url.searchParams`, over generated operation sequences. Each case
//! builds a list (from a query string, from pairs, or from `url.search`),
//! runs `append`, `delete`, `get`, `getAll`, `has`, `keys`, `set`, and
//! `toString`, and prints every result, the final `toString()`, and for a URL
//! its `href` and `search`; all must match.

#[path = "support/pinned_node.rs"]
mod support;

use spocky_contracts::js_value::{JsValue, parse, stringify};
use spocky_contracts::url::{Url, UrlSearchParams};

const NODE_SCRIPT: &str = r#"
const [dist, file] = process.argv.slice(1);
const { readFileSync } = await import("node:fs");
const cases = JSON.parse(readFileSync(file, "utf8"));
const out = cases.map(([kind, source, ops]) => {
  try {
    let url = null;
    let params;
    if (kind === "url") {
      url = new URL(source);
      params = url.searchParams;
    } else if (kind === "query") {
      params = new URLSearchParams(source);
    } else if (kind === "record") {
      params = new URLSearchParams(JSON.parse(source));
    } else if (kind === "pairs") {
      params = new URLSearchParams(source);
    } else {
      params = new URLSearchParams();
    }
    const results = [];
    for (const [op, name, value] of ops) {
      switch (op) {
        case "append": params.append(name, value); break;
        case "set": params.set(name, value); break;
        case "delete": if (value === null) params.delete(name); else params.delete(name, value); break;
        case "get": results.push(params.get(name)); break;
        case "getAll": results.push(params.getAll(name)); break;
        case "has": results.push(value === null ? params.has(name) : params.has(name, value)); break;
        case "keys": results.push([...params.keys()]); break;
        case "toString": results.push(params.toString()); break;
      }
    }
    results.push(params.toString());
    if (url) results.push(url.href, url.search);
    return JSON.stringify(results);
  } catch (error) {
    return "THROW " + error.name;
  }
});
process.stdout.write(out.join("\n") + "\n");
"#;

const QUERIES: &[&str] = &[
    "",
    "?",
    "??a",
    "a=b",
    "?a=b&c=d",
    "a=1&a=2&b=3&a=4",
    "a",
    "=b",
    "a=&b",
    "+a+=+b+",
    "%41=%42",
    "%zz=%",
    "a=%E2%82%AC",
    "a=%FF",
    "a%20b=c",
    "&&a=b&&",
    "a=b=c",
    "\u{e9}=\u{1f600}",
    "a=b#c",
    "a%2Bb=c%2bd",
    "%",
    "a=%",
    "a=%4",
    "%E2%82",
    "a+b=c+d",
    "*-._~!'()=*-._~!'()",
    "a=b&A=B",
    "x=1&y=2&x=3&y=4&z",
    "=",
    "==",
    "&=&",
    "a=%00",
    "a=%0A%0D",
    "[]=1",
    "a[]=1&a[]=2",
];
const HREFS: &[&str] = &[
    "http://example.com/",
    "http://example.com/?",
    "http://example.com/?a=b",
    "http://example.com/?a=b&c=d#frag",
    "http://example.com/?a=1&a=2",
    "http://example.com/?x=%7e&y=a+b",
    "http://example.com/?%41=%42",
    "http://example.com/#h",
    "foo://h/p?q=1",
    "data:text/plain,a?b=c",
    "http://example.com/?a=%zz",
    "http://example.com/?a=\u{e9}",
    "file:///a?b=c",
    "http://example.com/?&&a=b&&",
];
const NAMES: &[&str] = &[
    "a",
    "b",
    "c",
    "x",
    "y",
    "z",
    "",
    "a b",
    "\u{e9}",
    "%41",
    "+",
    "a+b",
    "a&b",
    "a=b",
    "\u{1f600}",
    "A",
    "*",
    "-",
    ".",
    "_",
    "~",
    "!",
    "'",
    "(",
    ")",
    "\u{0}",
    "\n",
];
const VALUES: &[&str] = &[
    "",
    "1",
    "2",
    "v",
    "a b",
    "\u{e9}",
    "%41",
    "+",
    "a&b",
    "a=b",
    "\u{1f600}",
    "*-._~!'()",
    "\u{0}",
    "\n",
    "x y z",
    "\u{20ac}",
    "a%b",
    "?",
    "#",
];

/// Own keys for record cases: array-index keys, which JavaScript enumerates
/// first in ascending order, near-misses that are ordinary names, and the
/// names above.
const RECORD_KEYS: &[&str] = &[
    "b",
    "a",
    "2",
    "10",
    "1",
    "0",
    "-1",
    "01",
    "1.5",
    "4294967294",
    "4294967295",
    "",
    "a b",
    "\\u00e9",
    "\\ud83d\\ude00",
    "\\ud800",
    "x=y",
    "&",
    "%41",
    "__proto__",
    "length",
];
/// JSON texts for record values: every type `String(value)` converts.
const RECORD_VALUES: &[&str] = &[
    "\"v\"",
    "\"\"",
    "\"a b\"",
    "\"\\ud800\"",
    "\"\\u00e9\"",
    "\"\\ud83d\\ude00\"",
    "1",
    "-0",
    "0.1",
    "1e21",
    "1e-7",
    "123456789012345680000",
    "true",
    "false",
    "null",
    "[]",
    "[1,2]",
    "[null,\"a\"]",
    "[[1,[2]],3]",
    "{}",
    "{\"a\":1}",
];

/// Records whose order or conversion is easy to get wrong.
const FIXED_RECORDS: &[&str] = &[
    "{}",
    r#"{"b":"1","a":"2"}"#,
    r#"{"b":"1","2":"x","a":"2","1":"y"}"#,
    r#"{"10":"a","9":"b","a":"c","0":"d","-1":"e","01":"f"}"#,
    r#"{"4294967295":"a","4294967294":"b","1":"c"}"#,
    r#"{"a":"1","b":"2","a":"3"}"#,
    r#"{"a":1,"b":true,"c":null,"d":[1,[2,3]],"e":{"x":1},"f":1e21,"g":-0}"#,
    r#"{"a b":"c d","&":"=","\u00e9":"\ud83d\ude00"}"#,
    r#"{"\ud800":"\udc00","a":"\ud800"}"#,
    r#"{"__proto__":"p","length":"3"}"#,
];

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

type Op = (&'static str, String, Option<String>);

fn random_ops(random: &mut Random) -> Vec<Op> {
    let count = random.below(7);
    (0..count)
        .map(|_| {
            let name = random.pick(NAMES).to_owned();
            match random.below(8) {
                0 => ("append", name, Some(random.pick(VALUES).to_owned())),
                1 => ("set", name, Some(random.pick(VALUES).to_owned())),
                2 => (
                    "delete",
                    name,
                    (random.below(2) == 0).then(|| random.pick(VALUES).to_owned()),
                ),
                3 => ("get", name, None),
                4 => ("getAll", name, None),
                5 => (
                    "has",
                    name,
                    (random.below(2) == 0).then(|| random.pick(VALUES).to_owned()),
                ),
                6 => ("keys", String::new(), None),
                _ => ("toString", String::new(), None),
            }
        })
        .collect()
}

/// `(kind, source, ops)`: `source` is a query, an href, or a JSON pair list.
fn generated_cases() -> Vec<(&'static str, String, Vec<Op>)> {
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    let mut cases = Vec::new();
    for query in QUERIES {
        cases.push((
            "query",
            (*query).to_owned(),
            vec![("toString", String::new(), None)],
        ));
    }
    for href in HREFS {
        cases.push(("url", (*href).to_owned(), Vec::new()));
        cases.push((
            "url",
            (*href).to_owned(),
            vec![("delete", "zzz".to_owned(), None)],
        ));
    }
    for _ in 0..4000 {
        cases.push((
            "query",
            random.pick(QUERIES).to_owned(),
            random_ops(&mut random),
        ));
    }
    for _ in 0..3000 {
        cases.push((
            "url",
            random.pick(HREFS).to_owned(),
            random_ops(&mut random),
        ));
    }
    for _ in 0..1000 {
        cases.push(("empty", String::new(), random_ops(&mut random)));
    }
    for _ in 0..1000 {
        let pairs: Vec<String> = (0..random.below(5))
            .map(|_| format!("{}\u{1}{}", random.pick(NAMES), random.pick(VALUES)))
            .collect();
        cases.push(("pairs", pairs.join("\u{2}"), random_ops(&mut random)));
    }
    for record in FIXED_RECORDS {
        cases.push(("record", (*record).to_owned(), Vec::new()));
    }
    for _ in 0..600 {
        let members: Vec<String> = (0..random.below(6))
            .map(|_| {
                format!(
                    "\"{}\":{}",
                    random.pick(RECORD_KEYS),
                    random.pick(RECORD_VALUES)
                )
            })
            .collect();
        cases.push((
            "record",
            format!("{{{}}}", members.join(",")),
            random_ops(&mut random),
        ));
    }
    cases
}

fn json_string(text: &str) -> JsValue {
    JsValue::String(text.to_owned())
}

fn case_json(kind: &str, source: &str, ops: &[Op]) -> JsValue {
    let source = if kind == "pairs" {
        JsValue::Array(
            source
                .split('\u{2}')
                .filter(|pair| !pair.is_empty())
                .map(|pair| {
                    let (name, value) = pair.split_once('\u{1}').expect("pair");
                    JsValue::Array(vec![json_string(name), json_string(value)])
                })
                .collect(),
        )
    } else {
        json_string(source)
    };
    JsValue::Array(vec![
        json_string(kind),
        source,
        JsValue::Array(
            ops.iter()
                .map(|(op, name, value)| {
                    JsValue::Array(vec![
                        json_string(op),
                        json_string(name),
                        value.as_deref().map_or(JsValue::Null, json_string),
                    ])
                })
                .collect(),
        ),
    ])
}

fn strings(values: &[&str]) -> JsValue {
    JsValue::Array(values.iter().map(|value| json_string(value)).collect())
}

fn run(kind: &str, source: &str, ops: &[Op]) -> String {
    let mut url = (kind == "url").then(|| Url::parse(source, None).expect("href"));
    let mut params = match kind {
        "url" => url.as_ref().expect("url").search_params(),
        "query" => UrlSearchParams::parse(source),
        "record" => UrlSearchParams::from_record(
            parse(source)
                .expect("record JSON")
                .as_object()
                .expect("record"),
        ),
        "pairs" => {
            let pairs: Vec<(&str, &str)> = source
                .split('\u{2}')
                .filter(|pair| !pair.is_empty())
                .map(|pair| pair.split_once('\u{1}').expect("pair"))
                .collect();
            UrlSearchParams::from_pairs(&pairs)
        }
        _ => UrlSearchParams::new(),
    };
    let mut results = Vec::new();
    for (op, name, value) in ops {
        let value = value.as_deref();
        let mut mutated = false;
        match *op {
            "append" => {
                params.append(name, value.unwrap_or(""));
                mutated = true;
            }
            "set" => {
                params.set(name, value.unwrap_or(""));
                mutated = true;
            }
            "delete" => {
                params.delete(name, value);
                mutated = true;
            }
            "get" => results.push(params.get(name).map_or(JsValue::Null, json_string)),
            "getAll" => results.push(strings(&params.get_all(name))),
            "has" => results.push(JsValue::Bool(params.has(name, value))),
            "keys" => results.push(strings(&params.keys())),
            _ => results.push(json_string(&params.to_string())),
        }
        if mutated && let Some(url) = url.as_mut() {
            url.set_search_params(&params);
        }
    }
    results.push(json_string(&params.to_string()));
    if let Some(url) = &url {
        results.push(json_string(&url.href()));
        results.push(json_string(&url.search()));
    }
    stringify(&JsValue::Array(results))
}

#[test]
fn url_search_params_match_node() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    let cases = generated_cases();
    let json = stringify(&JsValue::Array(
        cases
            .iter()
            .map(|(kind, source, ops)| case_json(kind, source, ops))
            .collect(),
    ));
    let file = std::env::temp_dir().join(format!("spocky-url-params-{}.json", std::process::id()));
    std::fs::write(&file, json).expect("write cases");
    let expected = support::run_node(&node, &dist, NODE_SCRIPT, &[file.display().to_string()]);
    std::fs::remove_file(&file).expect("remove cases");
    let mut lines = expected.lines();
    let mut mismatches = Vec::new();
    for (index, (kind, source, ops)) in cases.iter().enumerate() {
        let actual = run(kind, source, ops);
        let wanted = lines.next().expect("one line per case");
        if actual != wanted {
            mismatches.push(format!(
                "case {index} {kind} {source:?} {ops:?}\n  node: {wanted}\n  rust: {actual}"
            ));
        }
    }
    assert_eq!(lines.next(), None, "node printed more lines than cases");
    assert!(
        mismatches.is_empty(),
        "{} of {} cases differ:\n{}",
        mismatches.len(),
        cases.len(),
        mismatches
            .iter()
            .take(30)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
