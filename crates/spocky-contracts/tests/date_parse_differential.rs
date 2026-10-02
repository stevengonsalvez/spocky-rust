//! `Date.parse` differential: V8 in the pinned Node against
//! `date_parse`, for the ECMAScript format, the legacy free-form forms, and
//! offset-less date-times, each under several `TZ` values. The Rust side runs
//! as a child of this test binary so `TZ` reaches its system-zone lookup the
//! way it reaches V8's.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use spocky_contracts::js::date_parse::date_parse;

/// The pinned Node and dist, or `None` when the test may skip.
fn pinned() -> Option<(OsString, PathBuf)> {
    match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => Some((node, PathBuf::from(dist))),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: differential not run");
            None
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    }
}

/// Node 22.20.0 from Paseo `.tool-versions`.
const PINNED_NODE_VERSION: &str = "v22.20.0";

/// `gtimeout` where installed, else `timeout`.
fn timeout_command() -> &'static str {
    if Command::new("gtimeout").arg("--version").output().is_ok() {
        "gtimeout"
    } else {
        "timeout"
    }
}

/// Runs `script` as an ES module with `dist` and `args` in `process.argv`,
/// bounded by `gtimeout`, and returns its stdout.
fn run_node(node: &OsString, dist: &Path, script: &str, args: &[String]) -> String {
    run_node_with_env(node, dist, script, args, &[])
}

/// [`run_node`] with extra environment variables (for example `TZ`).
fn run_node_with_env(
    node: &OsString,
    dist: &Path,
    script: &str,
    args: &[String],
    env: &[(&str, &str)],
) -> String {
    let timeout = timeout_command();
    let version = Command::new(timeout)
        .args(["--kill-after=5", "30"])
        .arg(node)
        .arg("--version")
        .output()
        .expect("pinned node --version");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        PINNED_NODE_VERSION
    );
    let output = Command::new(timeout)
        .args(["--kill-after=5", "120"])
        .arg(node)
        .args(["--input-type=module", "-e", script])
        .arg(dist)
        .args(args)
        .envs(env.iter().copied())
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("node stdout is UTF-8")
}

/// Runs the test `test_name` of the current test binary as a child with `TZ`
/// set to `tz` and `child_env` present, bounded by `gtimeout`, and returns
/// its stdout lines that start with `prefix`. The Rust side of a time-zone
/// differential runs this way so `TZ` reaches its zone lookup the way it
/// reaches V8's.
fn run_self_child(test_name: &str, child_env: &str, tz: &str, prefix: &str) -> Vec<String> {
    let output = Command::new(timeout_command())
        .args(["--kill-after=5", "120"])
        .arg(std::env::current_exe().expect("test exe"))
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(child_env, "1")
        .env("TZ", tz)
        .output()
        .expect("run the child");
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("child stdout is UTF-8")
        .lines()
        .filter(|line| line.starts_with(prefix))
        .map(str::to_owned)
        .collect()
}

const CHILD_ENV: &str = "SPOCKY_DATE_PARSE_CHILD";
const RESULT_PREFIX: &str = "RESULT ";

const ZONES: [&str; 5] = [
    "UTC",
    "Europe/London",
    "America/New_York",
    "Asia/Kolkata",
    "Pacific/Auckland",
];

const FIXED: &[&str] = &[
    "2026-10-01T10:00:00Z",
    "2026-10-01T10:00:00",
    "2026-10-01T10:00",
    "2026-10-01T10",
    "2026-10-01",
    "2026-10",
    "2026",
    "+002026-10-01T10:00:00Z",
    "-000001-01-01T00:00:00Z",
    "-000000-01-01T00:00:00Z",
    "+000000-01-01T00:00:00Z",
    "2026-10-01T24:00:00",
    "2026-10-01T24:00:01",
    "2026-10-01T24:00:00.001",
    "2026-10-01T10:00:00.5",
    "2026-10-01T10:00:00.123456789Z",
    "2026-10-01T10:00:00.0001Z",
    "2026-10-01T10:00:00.Z",
    "2026-10-01T10:00:00+01:00",
    "2026-10-01T10:00:00-0530",
    "2026-10-01T10:00:00+01",
    "2026-10-01T10:00:00+24:00",
    "2026-10-01T10:00:00+01:60",
    "2026-10-01T10:00:00 +0100",
    "2026-10-01T10:00:00Zjunk",
    "2026-02-31T00:00:00Z",
    "2026-02-30",
    "2026-13-01",
    "2026-10-32",
    "2026-00-10",
    "2026-10-00",
    "2026-1-1",
    "2026-10-1",
    "2026-10-01t10:00:00z",
    "2026-10-01T1:00",
    "2026-10-01T10:0",
    "2026-10-01T10:00:0",
    "2026-10-01T25:00",
    "2026-10-01T10:60",
    "2026-10-01T10:00:60",
    "2026-10-01 10:00:00",
    "2026-10-01 10:00",
    "2026-10-01 10:00:00Z",
    "2026-10-01 10:00:00 +0100",
    "2026-10-01 10:00:00 +01:00",
    "2026-10-01 10:00:00+0100",
    "2026-10-01 10:00:00 GMT+0100",
    "2026-10-01 10:00:00 UTC",
    "2026-10-01 10:00:00.123",
    "2026-10-01 10:00:00.123456",
    "2026-10-01 UTC",
    "2026-10-01 PM",
    "Oct 1 2026",
    "Oct 1, 2026",
    "October 1, 2026",
    "Octo 1 2026",
    "Oct 1 2026 10:00:00",
    "Oct 1 2026 10:00:00.5",
    "Oct 1 2026 10:00:00.123456",
    "Oct 1 2026 10:00 +05:30",
    "Oct 1 2026 10:00 -8",
    "Oct 1 2026 10:00 +0100",
    "Oct 1 2026 10:00 +12345",
    "Oct 1 2026 10:00 PST",
    "Oct 1 2026 10:00 EDT",
    "Oct 1 2026 10 PM",
    "Oct 1 2026 10:00 PM",
    "Oct 1 2026 12:00 AM",
    "Oct 1 2026 13:00 PM",
    "Oct 1 2026 25:00",
    "Oct 1 2026 24:00",
    "Oct 1 2026 24:00:01",
    "Oct 1 2026 10::",
    "Oct 1 2026 10:: 5",
    "Oct 1 2026 10:00:00:00",
    "Oct 1 2026 10:00:00:00:00",
    "1 Oct 2026",
    "01 Oct 2026 10:00:00 GMT",
    "Thu, 01 Oct 2026 10:00:00 GMT",
    "Thu Oct 01 2026 10:00:00 GMT+0100 (BST)",
    "Thu Oct 01 2026 10:00:00 GMT+0100 (British Summer Time)",
    "Thu Oct 01 2026 10:00:00 GMT-0700 (PDT)",
    "10/1/2026",
    "10/01/2026 10:00 PM",
    "10/1/2026 12:00 AM",
    "13/1/2026",
    "2026/10/01",
    "2026/10/01 10:00",
    "2026.10.01",
    "10.1.2026",
    "12:00 PM",
    "12:00",
    "Oct 1",
    "1/2",
    "1",
    "12",
    "99",
    "50",
    "49",
    "0",
    "00",
    "2026-",
    "20261",
    "12345678901",
    "1e3",
    "Sep 31 2026",
    "Feb 30 2026",
    "Feb 29 2027",
    "2026 Oct 1",
    "10 1 2026 EST",
    "Oct 1 99",
    "Oct 1 49",
    "Oct 1 50",
    "Oct 1 0001",
    "Oct 1 0000",
    "Oct 1 275760",
    "Oct 1 999999",
    "Jan 1 1970",
    "Jan 1 1970 00:00:00 GMT",
    "Dec 31 1969 23:59:59.999 GMT",
    "00:00",
    "0:0",
    "Z",
    "UTC",
    "T",
    "x",
    "",
    " ",
    "   Oct 1 2026  ",
    "foo Oct 1 2026",
    "foo 2026",
    "Oct 1 foo 2026",
    "Oct 1 2026 foo",
    "Oct1 2026",
    "Oct-1-2026",
    "Oct 1 2026 (a comment) 10:00",
    "Oct 1 2026 (nested (parens)) 10:00",
    "(unbalanced Oct 1 2026",
    "Oct 1 2026)",
    "Oct 1 2026 + 5",
    "Oct 1 2026 - 5",
    "+ 5",
    "-5",
    "+2026",
    "-2026",
    "2026-10-01T10:00:00Z extra",
    "2026-03-29T00:59:59",
    "2026-03-29T01:00:00",
    "2026-03-29T01:30:00",
    "2026-03-29T02:00:00",
    "2026-10-25T00:59:59",
    "2026-10-25T01:00:00",
    "2026-10-25T01:30:00",
    "2026-10-25T02:00:00",
    "2026-03-08T02:30:00",
    "2026-11-01T01:30:00",
    "2026-09-27T02:30:00",
    "2026-04-05T02:30:00",
    "2026-03-29 01:30",
    "Mar 29 2026 01:30",
    "Oct 25 2026 01:30",
    "1800-01-01T00:00:00",
    "1883-11-18T12:00:00",
    "1900-01-01T00:00:00",
    "1969-06-01T12:00:00",
    "1970-01-01T00:00:00",
    "1970-06-01T12:00:00",
    "2100-06-01T12:00:00",
    "9999-07-01T12:00:00",
    "+012000-07-01T12:00:00",
    "+012000-01-01T12:00:00",
    "-002000-07-01T12:00:00",
    "+275760-09-13T00:00:00.000Z",
    "+275760-09-13T00:00:00.001Z",
    "-271821-04-20T00:00:00.000Z",
    "-271821-04-19T23:59:59.999Z",
    "+275760-09-12T23:59:59",
    "+275760-09-13T00:00:00",
    "+275760-09-13T01:00:00",
    "+275760-09-14T00:00:00",
    "-271821-04-20T00:00:00",
    "-271821-04-19T00:00:00",
    "-271821-04-19T12:00:00",
    "-271821-04-20T10:00:00",
    "+275760-09-13",
    "+275761-01-01",
    "275760-09-13",
    "Oct 1 2026 ünï",
    "\u{ff12}\u{ff10}\u{ff12}\u{ff16}-10-01",
    "2026-10-01T10:00:00\u{a0}",
    "2026-10-01\u{a0}10:00",
    "2026-10-01\u{2028}10:00",
    "Oct\t1\n2026",
    "\u{feff}Oct 1 2026",
    "Oct 1 2026\u{0}junk",
    "\u{0}Oct 1 2026",
    "2026-10-01T10:00:00\u{0}",
    "Oct 1 2026 10:00 \u{1d11e}",
    "Oct 1 \u{1d11e} 2026",
];

const TOKENS: &[&str] = &[
    "Oct",
    "October",
    "Mar",
    "Sep",
    "1",
    "2026",
    "10",
    "00",
    "0",
    "31",
    "12",
    "99",
    "5",
    "30",
    "01",
    ":",
    "-",
    "+",
    "/",
    ".",
    ",",
    " ",
    " ",
    "T",
    "Z",
    "z",
    "GMT",
    "UTC",
    "PM",
    "am",
    "EST",
    "PDT",
    "(x)",
    "0100",
    "05:30",
    "foo",
    "2026-10-01",
    "10:00:00",
    "T10:00:00",
    "+0100",
    "-08",
    "\t",
    "(",
    ")",
    "24",
    "60",
    "275760",
    "x",
];

/// A deterministic corpus of token soups.
fn generated() -> Vec<String> {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    (0..6000)
        .map(|_| {
            let count = 1 + usize::try_from(next() % 9).expect("small");
            (0..count)
                .map(|_| TOKENS[usize::try_from(next() % TOKENS.len() as u64).expect("small")])
                .collect::<String>()
        })
        .collect()
}

fn corpus() -> Vec<String> {
    FIXED
        .iter()
        .map(|text| (*text).to_owned())
        .chain(generated())
        .collect()
}

fn render(value: Option<i64>) -> String {
    value.map_or_else(|| "NaN".to_owned(), |millis| millis.to_string())
}

/// The child: prints one `RESULT` line per corpus entry.
#[test]
fn child_prints_date_parse_results() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    // The harness prints its own text before the first line without a break.
    println!();
    for text in corpus() {
        println!("{RESULT_PREFIX}{}", render(date_parse(&text)));
    }
}

const NODE_SCRIPT: &str = r#"
const corpus = JSON.parse(process.argv[2]);
process.stdout.write(corpus.map((text) => `RESULT ${String(Date.parse(text))}`).join("\n") + "\n");
"#;

/// A tzdb release as a number: year times 26 plus the letter, so
/// consecutive releases (`2025a`, `2025b`, `2026a`) differ by one.
fn release_ordinal(version: &str) -> i32 {
    let digits: String = version.chars().take_while(char::is_ascii_digit).collect();
    let letter = version[digits.len()..]
        .chars()
        .next()
        .filter(char::is_ascii_lowercase)
        .expect("a tzdb version ends its year with a letter");
    let year: i32 = digits.parse().expect("a tzdb year");
    year * 26 + i32::from(u8::try_from(letter).expect("ASCII") - b'a')
}

/// The host's tzdb release, from the zoneinfo directory the zone lookup reads.
fn host_tzdb_version() -> String {
    if let Ok(version) = std::fs::read_to_string("/usr/share/zoneinfo/+VERSION") {
        return version.trim().to_owned();
    }
    let zi = std::fs::read_to_string("/usr/share/zoneinfo/tzdata.zi")
        .expect("host tzdb version: no +VERSION and no tzdata.zi");
    zi.lines()
        .next()
        .and_then(|line| line.strip_prefix("# version "))
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_alphanumeric()).next())
        .expect("tzdata.zi header")
        .to_owned()
}

#[test]
fn date_parse_matches_v8_in_every_zone() {
    let Some((node, dist)) = pinned() else {
        return;
    };
    // Both sides read zone rules from their own tzdb; a gap of more than one
    // release can move a transition, so it fails the run instead of a case.
    let host = host_tzdb_version();
    let node_version = run_node(
        &node,
        &dist,
        "process.stdout.write(process.versions.tz)",
        &[],
    );
    println!("tzdb: host {host}, node ICU {node_version}");
    assert!(
        (release_ordinal(&host) - release_ordinal(&node_version)).abs() <= 1,
        "host tzdb {host} and node tzdata {node_version} differ by more than a release"
    );
    let corpus = corpus();
    let json = spocky_contracts::js_value::stringify(&spocky_contracts::js_value::JsValue::Array(
        corpus
            .iter()
            .map(|text| spocky_contracts::js_value::JsValue::String(text.clone()))
            .collect(),
    ));
    for zone in ZONES {
        let expected: Vec<String> = run_node_with_env(
            &node,
            &dist,
            NODE_SCRIPT,
            std::slice::from_ref(&json),
            &[("TZ", zone)],
        )
        .lines()
        .map(str::to_owned)
        .collect();
        let actual = run_self_child(
            "child_prints_date_parse_results",
            CHILD_ENV,
            zone,
            RESULT_PREFIX,
        );
        assert_eq!(actual.len(), expected.len(), "{zone}: result count");
        for (index, (rust, node_line)) in actual.iter().zip(&expected).enumerate() {
            assert_eq!(rust, node_line, "{zone}: {:?}", corpus[index]);
        }
    }
}
