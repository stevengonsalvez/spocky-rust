//! Corpus differential: every scenario of `scripts/phase4/xterm-corpus.json`
//! (the 16 memo scenarios first, then one per ported `InputHandler` branch)
//! must produce the same `JSON.stringify` text through the pinned xterm and
//! through `spocky-xterm`. See `common/mod.rs` for the environment.

mod common;

use spocky_contracts::js_value::{JsValue, parse};

#[test]
fn corpus_matches_pinned_xterm() {
    let Some((node, paseo_root)) = common::pinned() else {
        return;
    };
    let corpus_path = common::repo_path("scripts/phase4/xterm-corpus.json");
    let corpus =
        parse(&std::fs::read_to_string(&corpus_path).expect("corpus")).expect("corpus json");
    let scenarios = corpus
        .get("scenarios")
        .and_then(JsValue::as_array)
        .expect("scenarios");
    let out =
        std::env::temp_dir().join(format!("spocky-xterm-corpus-{}.jsonl", std::process::id()));
    let captured = common::capture(&node, &paseo_root, &corpus_path, &out, 300);
    let _ = std::fs::remove_file(&out);
    assert_eq!(
        captured.len(),
        scenarios.len(),
        "one capture line per scenario"
    );
    let mut failures = Vec::new();
    for (scenario, expected) in scenarios.iter().zip(&captured) {
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
    let memo = scenarios.len().min(16);
    let memo_failures = scenarios[..memo]
        .iter()
        .zip(&captured)
        .filter(|(scenario, expected)| &common::replay(scenario) != *expected)
        .count();
    eprintln!(
        "memo scenarios: {} of {memo} full matches; whole corpus: {} of {} full matches",
        memo - memo_failures,
        scenarios.len() - failures.len(),
        scenarios.len()
    );
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
