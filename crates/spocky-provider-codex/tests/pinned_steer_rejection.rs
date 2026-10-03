//! `isDefinitiveCodexSteerRejection` against the pinned Paseo build: a
//! corpus of errors goes through the pinned function (a patched copy of the
//! agent module exports it) and through `is_definitive_steer_rejection`.
//! A steer that Codex definitively rejected reports `unavailable`; any other
//! failure is ambiguous and surfaces as an error, so each case decides which.

mod support;

use std::time::Duration;

use serde_json::{Value, json};
use spocky_provider_codex::is_definitive_steer_rejection;
use spocky_provider_codex::transport::{ClientError, RpcErrorFields};
use support::DisposableRoot;

const TURN: &str = "expected active turn id `turn-a` but found `turn-b`";

type Case = (&'static str, String, Option<Value>);

/// A `CodexAppServerRpcError`'s `{code?, data?}`: a null `code` stays missing.
/// `Some` always, as a case holds `None` for a plain `Error`.
#[allow(clippy::unnecessary_wraps)]
fn rpc(code: &Value, data: Option<Value>) -> Option<Value> {
    let mut rpc = serde_json::Map::new();
    if !code.is_null() {
        rpc.insert("code".to_owned(), code.clone());
    }
    if let Some(data) = data {
        rpc.insert("data".to_owned(), data);
    }
    Some(Value::Object(rpc))
}

/// An invalid request (-32600) carrying `message`.
fn invalid(name: &'static str, message: &str) -> Case {
    (name, message.to_owned(), rpc(&json!(-32600), None))
}

/// Cases that differ by the JSON-RPC code.
fn code_cases() -> Vec<Case> {
    let with = |name, code: Value| (name, "x".to_owned(), rpc(&code, None));
    let steer = |name, code: Value| (name, "no active turn to steer".to_owned(), rpc(&code, None));
    vec![
        with("method not found", json!(-32601)),
        with("method not found text code", json!("-32601")),
        with("method not found float code", json!(-32601.0)),
        steer("no code", Value::Null),
        steer("other code", json!(-32000)),
        steer("text invalid code", json!("-32600")),
        steer("float invalid code", json!(-32600.0)),
        ("plain error", "no active turn to steer".to_owned(), None),
        (
            "plain error, method not found text",
            "Method not found".to_owned(),
            None,
        ),
    ]
}

/// Cases that differ by the message of an invalid request.
fn message_cases() -> Vec<Case> {
    vec![
        invalid("no active turn", "no active turn to steer"),
        invalid("no active turn, other case", "No active turn to steer"),
        invalid("no active turn, trailing space", "no active turn to steer "),
        invalid(
            "different output schema",
            "active turn uses a different output schema",
        ),
        invalid(
            "another output schema",
            "active turn uses another output schema",
        ),
        invalid("turn mismatch", TURN),
        invalid(
            "turn mismatch, empty expected",
            "expected active turn id `` but found `turn-b`",
        ),
        invalid(
            "turn mismatch, empty found",
            "expected active turn id `turn-a` but found ``",
        ),
        invalid(
            "turn mismatch, backtick in found",
            "expected active turn id `turn-a` but found `tu`rn`",
        ),
        invalid(
            "turn mismatch, backtick in expected",
            "expected active turn id `tu`rn` but found `turn-b`",
        ),
        invalid("turn mismatch, trailing newline", &format!("{TURN}\n")),
        invalid(
            "turn mismatch, newline inside",
            "expected active turn id `a\nb` but found `c\nd`",
        ),
        invalid("turn mismatch, leading text", &format!("error: {TURN}")),
        invalid("other invalid request", "invalid request"),
    ]
}

/// Cases that differ by `data.codexErrorInfo.activeTurnNotSteerable`.
fn data_cases() -> Vec<Case> {
    let info = |name, info: Value| {
        (
            name,
            "x".to_owned(),
            rpc(&json!(-32600), Some(json!({"codexErrorInfo": info}))),
        )
    };
    let data = |name, code: Value, data: Value| (name, "x".to_owned(), rpc(&code, Some(data)));
    let marker = json!({"activeTurnNotSteerable": {}});
    vec![
        info(
            "not steerable object",
            json!({"activeTurnNotSteerable": {"turnKind": "review"}}),
        ),
        info("not steerable empty object", marker.clone()),
        info("not steerable array", json!({"activeTurnNotSteerable": []})),
        info(
            "not steerable null",
            json!({"activeTurnNotSteerable": null}),
        ),
        info("not steerable text", json!({"activeTurnNotSteerable": "x"})),
        info(
            "not steerable true",
            json!({"activeTurnNotSteerable": true}),
        ),
        info("error info text", json!("activeTurnNotSteerable")),
        info("error info array", json!([marker.clone()])),
        info("error info null", Value::Null),
        data(
            "data array",
            json!(-32600),
            json!([{"codexErrorInfo": marker.clone()}]),
        ),
        data("data text", json!(-32600), json!("activeTurnNotSteerable")),
        data(
            "on another code",
            json!(-32000),
            json!({"codexErrorInfo": marker}),
        ),
        (
            "other error info, steer message",
            "no active turn to steer".to_owned(),
            rpc(&json!(-32600), Some(json!({"codexErrorInfo": "other"}))),
        ),
    ]
}

fn corpus() -> Vec<Case> {
    [code_cases(), message_cases(), data_cases()].concat()
}

fn rust_result(message: &str, rpc: Option<&Value>) -> bool {
    let error = ClientError {
        message: message.to_owned(),
        rpc: rpc.map(|rpc| {
            Box::new(RpcErrorFields {
                code: rpc.get("code").cloned(),
                data: rpc.get("data").cloned(),
            })
        }),
    };
    is_definitive_steer_rejection(&error)
}

#[test]
#[ignore = "needs the pinned Paseo build; run with --include-ignored"]
fn steer_rejection_classification_matches_pinned() {
    let cases = corpus();
    let entries: Vec<Value> = cases
        .iter()
        .map(|(_, message, rpc)| match rpc {
            Some(rpc) => json!({"message": message, "rpc": rpc}),
            None => json!({"message": message}),
        })
        .collect();
    let pinned = support::pinned_paseo();
    pinned.verified_oracle("codex-app-server-agent.js");
    let root = DisposableRoot::new("steer-rejection");
    let output = support::run_pinned_node(
        &pinned,
        "tests/support/pinned_steer_rejection.mjs",
        &[
            pinned.providers_dir().to_string_lossy().into_owned(),
            root.join("pinned-steer").to_string_lossy().into_owned(),
            Value::Array(entries).to_string(),
        ],
        &root,
        Duration::from_secs(60),
    );
    let pinned_results: Vec<bool> = serde_json::from_str(&output).expect("pinned results");
    assert_eq!(pinned_results.len(), cases.len());
    for ((name, message, rpc), pinned_result) in cases.iter().zip(pinned_results.iter().copied()) {
        assert_eq!(
            rust_result(message, rpc.as_ref()),
            pinned_result,
            "{name}: Rust differs from pinned"
        );
    }
    assert!(
        pinned_results.contains(&true) && pinned_results.contains(&false),
        "the corpus must hold both definitive and ambiguous errors"
    );
}
