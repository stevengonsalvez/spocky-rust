//! `resolveStoredAgentUpdatedAt` orders `updatedAt` and `lastActivityAt` with
//! `Date.parse` (`persistence-hooks.ts:122`), which reads every form V8 reads,
//! not only ISO. Expected values are what the pinned `persistence-hooks.js`
//! returned under node v22.20.0 for the same records; each zone is named, so
//! the results do not depend on the local time zone.

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_session::persistence_hooks::resolve_stored_agent_updated_at;

/// (`updatedAt`, `lastActivityAt`, the pinned result).
const CASES: [(&str, &str, &str); 7] = [
    (
        "Jan 3 2020 00:00:00 GMT",
        "2020-01-02T00:00:00.000Z",
        "Jan 3 2020 00:00:00 GMT",
    ),
    (
        "2020-01-01T00:00:00.000Z",
        "Wed, 01 Jan 2020 12:00:00 GMT",
        "Wed, 01 Jan 2020 12:00:00 GMT",
    ),
    (
        "Jan 1 2020 00:00:00 GMT",
        "1 Jan 2020 00:00:00 UTC",
        "Jan 1 2020 00:00:00 GMT",
    ),
    ("garbage", "also garbage", "garbage"),
    (
        "2020-01-01 00:00:00 GMT",
        "2020-01-01T00:00:01Z",
        "2020-01-01T00:00:01Z",
    ),
    (
        "2020-01-02T00:00:00Z",
        "1/3/2020 00:00:00 GMT",
        "1/3/2020 00:00:00 GMT",
    ),
    ("", "Jan 5 2020 GMT", "Jan 5 2020 GMT"),
];

#[test]
fn stored_agent_updated_at_orders_by_date_parse() {
    for (updated_at, last_activity_at, expected) in CASES {
        let mut record = JsObject::new();
        record.insert("updatedAt", JsValue::String(updated_at.to_owned()));
        record.insert(
            "lastActivityAt",
            JsValue::String(last_activity_at.to_owned()),
        );
        assert_eq!(
            resolve_stored_agent_updated_at(&JsValue::Object(record)),
            JsValue::String(expected.to_owned()),
            "{updated_at:?} vs {last_activity_at:?}"
        );
    }
}
