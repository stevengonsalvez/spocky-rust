//! URL setter differential: node v22.20.0 (ada 2.9.2) against the setters of
//! `Url`, over the Web Platform Tests `setters_tests.json` inputs (href,
//! setter, new value; see `fixtures/url-setters-corpus.json` for the commit)
//! and generated cases. The pinned node prints the expected `href`, `origin`,
//! `protocol`, `username`, `password`, `host`, `hostname`, `port`, `pathname`,
//! `search`, and `hash` after `url[setter] = value`; every line must match.

#[path = "support/pinned_node.rs"]
mod support;

use spocky_contracts::js_value::{JsValue, js_text_to_utf8, parse, stringify};
use spocky_contracts::url::Url;

const NODE_SCRIPT: &str = r#"
const [dist, file, generatedFile] = process.argv.slice(1);
const { readFileSync } = await import("node:fs");
const { cases } = JSON.parse(readFileSync(file, "utf8"));
const generated = JSON.parse(readFileSync(generatedFile, "utf8"));
const out = [...cases, ...generated].map(([href, setter, value]) => {
  try {
    const url = new URL(href);
    url[setter] = value;
    return JSON.stringify([url.href, url.origin, url.protocol, url.username, url.password, url.host, url.hostname, url.port, url.pathname, url.search, url.hash]);
  } catch {
    return "null";
  }
});
process.stdout.write(out.join("\n") + "\n");
"#;

const HREFS: &[&str] = &[
    "http://example.com/a/b?q#h",
    "https://user:pass@example.com:8443/x?y#z",
    "http://u@host/",
    "http://:p@host/",
    "ws://h:81/p",
    "wss://h/p",
    "ftp://f:21/dir/",
    "http://[::1]:8080/a",
    "http://xn--9hb.com/",
    "http://1.2.3.4/",
    "http://example.com:80/",
    "https://example.com:443/",
    "file:///C:/a/b",
    "file://host/share/x",
    "file:///",
    "foo://h/a/b?q#f",
    "foo://u:p@h:9/a",
    "foo:/a/b",
    "foo:a/b?q",
    "foo:///x",
    "foo:",
    "data:text/plain,hello world",
    "data:text/plain,hello   ",
    "mailto:a@b.c",
    "javascript:alert(1)",
    "blob:https://example.com/uuid",
    "a://example.net",
    "http://example.com/a b/%2e%2e/c",
    "http://example.com/?a=b&c=d",
    "http://example.com/#",
    "http://example.com/?",
    "http://\u{e9}.com/",
    "about:blank",
    "non-spec:/.//p",
    "non-spec://x/..//p",
    "http://example.com//a//b",
];
const PROTOCOLS: &[&str] = &[
    "http",
    "https",
    "HTTPS",
    "ws",
    "wss",
    "ftp",
    "file",
    "foo",
    "bar:",
    "http:",
    "http://x",
    "a+b",
    "1a",
    "",
    "h t t p",
    "hTTp:foo",
    "blob",
    "javascript",
    "mailto",
    "\u{e9}",
    "http\n",
    "ht\ttp",
    "x-y.z",
    ":",
    "https:foo:bar",
];
const CREDENTIALS: &[&str] = &[
    "",
    "u",
    "us er",
    "a:b",
    "a@b",
    "%41",
    "\u{e9}",
    "a/b",
    "?#",
    "x\\y",
    "a\tb",
    "\u{0}",
    "~!$&'()*+,;=",
];
const HOSTS: &[&str] = &[
    "example.com",
    "EXAMPLE.com:8080",
    "a.b:80",
    ":80",
    "",
    "[::1]",
    "[::1]:99",
    "1.2.3.4",
    "0x7f.1",
    "a b",
    "a/b",
    "a?b",
    "a#b",
    "a@b",
    "a:b:c",
    "xn--9hb.com",
    "\u{e9}.com",
    "a:99999",
    "a:0",
    "a:-1",
    "a:\t80",
    "\t",
    "localhost",
    "[1:2:3:4:5:6:7:8]:1",
    "host:80/path",
    "host?x",
    "host\\x",
    "%41",
    "a:",
    "a:b",
    "a:8x",
    "a:x8",
    "HOST.COM:443",
    "\u{661}.com",
    "[",
    "]",
    "[::1",
    "::1]",
    "ex ample",
    "1.2.3",
    "256.1.1.1",
];
const PATHNAMES: &[&str] = &[
    "",
    "/",
    "a",
    "/a/b",
    "../x",
    "/%2e",
    "\\a",
    "/a b",
    "/\u{e9}",
    "//",
    "?x",
    "#y",
    "/a?b#c",
    "C:",
    "/C|/",
    "...",
    "/a/../../b",
    "/./",
    "a/./b",
    "/%2E%2e/c",
    "/\u{0}",
    "/a\tb",
    "//a",
    "/..",
    ".",
    "..",
    "/a/b/c/..",
    "/%",
    "%zz",
    "\\\\a\\\\b",
];
const HASHES: &[&str] = &[
    "",
    "#",
    "a",
    "#a b",
    "\u{e9}",
    "a#b",
    "?x",
    "\u{0}",
    "##",
    "a\tb",
    "#\n",
    "\u{1f600}",
    "%",
    "`a`",
];
const SEARCHES: &[&str] = &[
    "",
    "?",
    "a=b",
    "?a b",
    "?\u{e9}",
    "a#b",
    "'q'",
    "a\tb",
    "??",
    "a=b&c=d",
    "\u{1f600}",
    "%",
    "\"x\"",
    "?#",
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

/// Generated `(href, setter, value)` triples.
fn generated_cases() -> Vec<(String, &'static str, String)> {
    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    let mut cases = Vec::new();
    for href in HREFS {
        for (setter, pool) in [
            ("protocol", PROTOCOLS),
            ("username", CREDENTIALS),
            ("password", CREDENTIALS),
            ("host", HOSTS),
            ("hostname", HOSTS),
            ("pathname", PATHNAMES),
            ("hash", HASHES),
            ("search", SEARCHES),
        ] {
            for value in pool {
                cases.push(((*href).to_owned(), setter, (*value).to_owned()));
            }
        }
    }
    // Random pairings of the pools beyond the full cross above are not needed;
    // chained setters on one URL are covered by the next loop.
    for _ in 0..3000 {
        let href = random.pick(HREFS);
        let (setter, pool) = match random.below(8) {
            0 => ("protocol", PROTOCOLS),
            1 => ("username", CREDENTIALS),
            2 => ("password", CREDENTIALS),
            3 => ("host", HOSTS),
            4 => ("hostname", HOSTS),
            5 => ("pathname", PATHNAMES),
            6 => ("hash", HASHES),
            _ => ("search", SEARCHES),
        };
        let mut value = random.pick(pool).to_owned();
        if random.below(3) == 0 {
            value.push_str(random.pick(pool));
        }
        cases.push((href.to_owned(), setter, value));
    }
    cases
}

fn apply(href: &str, setter: &str, value: &str) -> String {
    let Some(mut url) = Url::parse(href, None) else {
        return "null".to_owned();
    };
    match setter {
        "protocol" => {
            url.set_protocol(value);
        }
        "username" => {
            url.set_username(value);
        }
        "password" => {
            url.set_password(value);
        }
        "host" => {
            url.set_host(value);
        }
        "hostname" => {
            url.set_hostname(value);
        }
        "pathname" => {
            url.set_pathname(value);
        }
        "hash" => url.set_hash(value),
        "search" => url.set_search(value),
        other => panic!("unknown setter {other}"),
    }
    stringify(&JsValue::Array(
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
    ))
}

#[test]
fn url_setters_match_node() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/url-setters-corpus.json");
    let generated = generated_cases();
    let generated_json = stringify(&JsValue::Array(
        generated
            .iter()
            .map(|(href, setter, value)| {
                JsValue::Array(vec![
                    JsValue::String(href.clone()),
                    JsValue::String((*setter).to_owned()),
                    JsValue::String(value.clone()),
                ])
            })
            .collect(),
    ));
    let generated_file =
        std::env::temp_dir().join(format!("spocky-url-setters-{}.json", std::process::id()));
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
    let usv = |value: &JsValue| js_text_to_utf8(value.as_str().expect("string"));
    let fixture = corpus
        .get("cases")
        .and_then(JsValue::as_array)
        .expect("cases")
        .iter()
        .map(|case| {
            let triple = case.as_array().expect("case");
            (usv(&triple[0]), usv(&triple[1]), usv(&triple[2]))
        });
    let all: Vec<(String, String, String)> = fixture
        .chain(
            generated
                .into_iter()
                .map(|(href, setter, value)| (href, setter.to_owned(), value)),
        )
        .collect();
    let mut lines = expected.lines();
    let mut mismatches = Vec::new();
    for (index, (href, setter, value)) in all.iter().enumerate() {
        let actual = apply(href, setter, value);
        let wanted = lines.next().expect("one line per case");
        if actual != wanted {
            mismatches.push(format!(
                "case {index} {href:?} {setter} = {value:?}\n  node: {wanted}\n  rust: {actual}"
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
