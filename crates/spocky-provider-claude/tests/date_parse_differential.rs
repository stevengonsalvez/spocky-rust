//! The replay timestamp helpers keep or drop a string with the `Date.parse`
//! of `spocky_contracts::js::date_parse`, whose differential against V8 in five
//! time zones is `crates/spocky-contracts/tests/date_parse_differential.rs`.

use spocky_provider_claude::timestamps::{iso_from_date_string, normalize_replay_timestamp_text};

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
