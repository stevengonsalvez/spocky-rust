//! `Date.parse` differential: V8 in the pinned Node against
//! `date_parse`, for the ECMAScript format, the legacy free-form forms, and
//! offset-less date-times, each under several `TZ` values. The Rust side runs
//! as a child of this test binary so `TZ` reaches its system-zone lookup the
//! way it reaches V8's.

mod support;

use spocky_provider_claude::date_parse::date_parse;
use spocky_provider_claude::timestamps::{iso_from_date_string, normalize_replay_timestamp_text};

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

#[test]
fn date_parse_matches_v8_in_every_zone() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    let corpus = corpus();
    let json = spocky_contracts::js_value::stringify(&spocky_contracts::js_value::JsValue::Array(
        corpus
            .iter()
            .map(|text| spocky_contracts::js_value::JsValue::String(text.clone()))
            .collect(),
    ));
    for zone in ZONES {
        let expected: Vec<String> = support::run_node_with_env(
            &node,
            &dist,
            NODE_SCRIPT,
            std::slice::from_ref(&json),
            &[("TZ", zone)],
        )
        .lines()
        .map(str::to_owned)
        .collect();
        let actual = support::run_self_child(
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

// The helpers built on `Date.parse` keep or drop the string with the parse.
#[test]
fn replay_timestamps_follow_date_parse() {
    assert_eq!(
        normalize_replay_timestamp_text(" Oct 1 2026 ").as_deref(),
        Some("Oct 1 2026")
    );
    assert_eq!(
        normalize_replay_timestamp_text("2026-10-01 10:00:00").as_deref(),
        Some("2026-10-01 10:00:00")
    );
    assert_eq!(normalize_replay_timestamp_text("2026-13-01"), None);
    assert_eq!(
        iso_from_date_string("2026-10-01T10:00:00Z").as_deref(),
        Some("2026-10-01T10:00:00.000Z")
    );
}
