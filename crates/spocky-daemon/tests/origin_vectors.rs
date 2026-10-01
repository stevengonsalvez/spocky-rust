//! `is_web_socket_same_origin` against the pinned `isWebSocketSameOrigin` over
//! 21 origins and 18 Host headers. Regenerate with
//! `tests/fixtures/gen-origin-vectors.mjs`.

use spocky_daemon::origin::is_web_socket_same_origin;

#[test]
fn matches_the_pinned_function_on_every_vector() {
    let rows: Vec<(String, String, bool)> =
        serde_json::from_str(include_str!("fixtures/origin-vectors.json")).unwrap();
    assert_eq!(rows.len(), 378);
    assert!(rows.iter().any(|row| row.2) && rows.iter().any(|row| !row.2));
    let mismatches: Vec<_> = rows
        .iter()
        .filter(|(origin, host, want)| is_web_socket_same_origin(Some(origin), Some(host)) != *want)
        .collect();
    assert!(mismatches.is_empty(), "{mismatches:?}");
}
