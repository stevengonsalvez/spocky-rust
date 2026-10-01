//! `parse_subprotocols` against `ws` 8.20.0 `lib/subprotocol.js` over generated
//! headers. Regenerate with `tests/fixtures/gen-subprotocol-vectors.mjs`.

use serde_json::Value;
use spocky_daemon::subprotocol::parse_subprotocols;

#[test]
fn matches_the_ws_library_on_every_vector() {
    let rows: Vec<(String, Value)> =
        serde_json::from_str(include_str!("fixtures/subprotocol-vectors.json")).unwrap();
    assert!(rows.len() > 2000, "fixture shrank to {} rows", rows.len());
    let mismatches: Vec<String> = rows
        .iter()
        .filter(|(header, want)| {
            let got = parse_subprotocols(header);
            let same = match (&got, want.get("ok"), want.get("err")) {
                (Ok(protocols), Some(ok), _) => serde_json::json!(protocols) == *ok,
                (Err(message), _, Some(err)) => err.as_str() == Some(message.as_str()),
                _ => false,
            };
            !same
        })
        .map(|(header, want)| format!("{header:?} want {want}"))
        .collect();
    assert!(mismatches.is_empty(), "{mismatches:?}");
}
