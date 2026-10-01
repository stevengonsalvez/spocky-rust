//! Manual trigger intake, manual run matching and the public manual-run response mapping.

use std::collections::BTreeMap;

use serde_json::Value;

use super::timezone::LocalOffset;
use super::webhook::json_type_name;

const MISSING_PAYLOAD: &str = "Invalid input: expected nonoptional, received undefined";
const RECEIVED_AT_MESSAGE: &str = "receivedAt must be an ISO date string when provided";

#[derive(Clone, Debug, PartialEq)]
pub struct ManualTriggerInput {
    pub organization_id: String,
    pub project_id: String,
    pub connection_id: Option<String>,
    pub resource_id: Option<String>,
    pub source: String,
    pub delivery_id: String,
    /// Epoch milliseconds; `None` when the request omitted `receivedAt`.
    pub received_at_ms: Option<i64>,
    pub payload: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManualParseFailure {
    InvalidJson,
    Invalid(String),
}

/// Decodes a request body the way `Request.json()` does: the body reader drops one leading byte
/// order mark, the lossy UTF-8 decode drops one more, then `JSON.parse` runs. Observed on the
/// baseline runtime: two leading marks parse, three do not.
pub(super) fn decode_request_json(body: &[u8]) -> Option<Value> {
    let body = body.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(body);
    let decoded = String::from_utf8_lossy(body);
    let decoded = decoded.strip_prefix('\u{feff}').unwrap_or(&decoded);
    serde_json::from_str(decoded).ok()
}

/// Parses a manual trigger request body the way the baseline does: the first issue, in schema
/// key order, becomes the error message. `local_offset` supplies the host offset applied to
/// date-times written without an offset.
///
/// # Errors
///
/// Returns [`ManualParseFailure`] when the body is not JSON or fails the payload schema.
pub fn parse_manual_payload(
    body: &[u8],
    local_offset: impl LocalOffset,
) -> Result<ManualTriggerInput, ManualParseFailure> {
    let value = decode_request_json(body).ok_or(ManualParseFailure::InvalidJson)?;
    let Value::Object(map) = &value else {
        return Err(invalid(format!(
            "Invalid input: expected object, received {}",
            json_type_name(&value)
        )));
    };

    let organization_id = required_string(map.get("organizationId"), |text| {
        text.is_empty().then_some("organizationId is required")
    })?;
    let project_id = required_string(map.get("projectId"), |text| {
        (!is_rfc_uuid(text)).then_some("projectId is required")
    })?;
    let connection_id = optional_string(map.get("connectionId"), |text| {
        (!is_rfc_uuid(text)).then_some("Invalid UUID")
    })?;
    let resource_id = optional_string(map.get("resourceId"), |_| None)?;
    let source = required_string(map.get("source"), |text| {
        if text.is_empty() {
            Some("source is required and must be a non-empty string")
        } else if !text.contains('.') {
            Some("source must be provider-namespaced, for example github.issue_comment")
        } else {
            None
        }
    })?;
    let delivery_id = required_string(map.get("deliveryId"), |text| {
        text.is_empty()
            .then_some("deliveryId is required and must be a non-empty string")
    })?;
    let received_at_ms = match map.get("receivedAt") {
        None => None,
        Some(Value::String(text)) => Some(
            parse_iso_ms(text, &local_offset)
                .and_then(|ms| i64::try_from(ms).ok())
                .ok_or_else(|| invalid(RECEIVED_AT_MESSAGE.to_owned()))?,
        ),
        Some(_) => return Err(invalid(RECEIVED_AT_MESSAGE.to_owned())),
    };
    let payload = map
        .get("payload")
        .cloned()
        .ok_or_else(|| invalid(MISSING_PAYLOAD.to_owned()))?;

    Ok(ManualTriggerInput {
        organization_id,
        project_id,
        connection_id,
        resource_id,
        source,
        delivery_id,
        received_at_ms,
        payload,
    })
}

fn invalid(message: String) -> ManualParseFailure {
    ManualParseFailure::Invalid(message)
}

fn type_issue(value: Option<&Value>) -> ManualParseFailure {
    let received = value.map_or("undefined", json_type_name);
    invalid(format!(
        "Invalid input: expected string, received {received}"
    ))
}

fn required_string(
    value: Option<&Value>,
    check: impl Fn(&str) -> Option<&'static str>,
) -> Result<String, ManualParseFailure> {
    match value {
        Some(Value::String(text)) => match check(text) {
            Some(message) => Err(invalid(message.to_owned())),
            None => Ok(text.clone()),
        },
        other => Err(type_issue(other)),
    }
}

fn optional_string(
    value: Option<&Value>,
    check: impl Fn(&str) -> Option<&'static str>,
) -> Result<Option<String>, ManualParseFailure> {
    match value {
        None | Some(Value::Null) => Ok(None),
        other => required_string(other, check).map(Some),
    }
}

/// RFC 9562 UUID text, versions 1 to 8, plus the nil and max UUIDs.
fn is_rfc_uuid(text: &str) -> bool {
    if text == "00000000-0000-0000-0000-000000000000"
        || text.eq_ignore_ascii_case("ffffffff-ffff-ffff-ffff-ffffffffffff")
    {
        return true;
    }
    let groups: Vec<&str> = text.split('-').collect();
    let lengths = [8, 4, 4, 4, 12];
    groups.len() == 5
        && groups.iter().zip(lengths).all(|(group, length)| {
            group.len() == length && group.bytes().all(|b| b.is_ascii_hexdigit())
        })
        && matches!(groups[2].as_bytes()[0], b'1'..=b'8')
        && matches!(
            groups[3].as_bytes()[0],
            b'8' | b'9' | b'a' | b'b' | b'A' | b'B'
        )
}

fn digits(bytes: &[u8], cursor: &mut usize, count: usize) -> Option<i128> {
    let slice = bytes.get(*cursor..*cursor + count)?;
    if !slice.iter().all(u8::is_ascii_digit) {
        return None;
    }
    *cursor += count;
    std::str::from_utf8(slice).ok()?.parse().ok()
}

/// Epoch milliseconds of the ISO-8601 forms `new Date(text)` accepts in the baseline runtime.
///
/// Date-only forms are UTC; date-times without an offset are local time (`local_offset`).
/// Fractions keep their first three digits. Known gap: the baseline runtime also accepts non-ISO
/// legacy forms such as `Aug 6 2026`, `2026/08/06` and `2026-08-06 12:00`; those are rejected
/// here.
fn parse_iso_ms(text: &str, local_offset: &impl LocalOffset) -> Option<i128> {
    let bytes = text.as_bytes();
    let mut cursor = 0;
    let year = match bytes.first()? {
        sign @ (b'+' | b'-') => {
            cursor += 1;
            let value = digits(bytes, &mut cursor, 6)?;
            match (*sign, value) {
                (b'-', 0) => return None,
                (b'-', _) => -value,
                _ => value,
            }
        }
        _ => digits(bytes, &mut cursor, 4)?,
    };
    let mut month = 1;
    let mut day = 1;
    if bytes.get(cursor) == Some(&b'-') {
        cursor += 1;
        month = digits(bytes, &mut cursor, 2)?;
        if bytes.get(cursor) == Some(&b'-') {
            cursor += 1;
            day = digits(bytes, &mut cursor, 2)?;
        }
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (time_ms, offset) = if bytes.get(cursor) == Some(&b'T') {
        cursor += 1;
        parse_clock(text, &mut cursor)?
    } else {
        (0, Some(0))
    };
    if cursor != bytes.len() {
        return None;
    }
    let wall_ms = days_from_civil(year, month, day) * 86_400_000 + time_ms;
    let total = wall_ms - offset.unwrap_or_else(|| local_offset.offset_ms_at_local(wall_ms));
    (total.abs() <= 8_640_000_000_000_000).then_some(total)
}

/// `HH:mm[:ss[.f+]]` plus an optional `Z`, `+HH:mm` or `+HHmm` offset (in milliseconds).
fn parse_clock(text: &str, cursor: &mut usize) -> Option<(i128, Option<i128>)> {
    let bytes = text.as_bytes();
    let hour = digits(bytes, cursor, 2)?;
    if bytes.get(*cursor) != Some(&b':') {
        return None;
    }
    *cursor += 1;
    let minute = digits(bytes, cursor, 2)?;
    let mut second = 0;
    let mut fraction_ms = 0;
    let mut fraction_nonzero = false;
    if bytes.get(*cursor) == Some(&b':') {
        *cursor += 1;
        second = digits(bytes, cursor, 2)?;
        if bytes.get(*cursor) == Some(&b'.') {
            *cursor += 1;
            let start = *cursor;
            while bytes.get(*cursor).is_some_and(u8::is_ascii_digit) {
                *cursor += 1;
            }
            if *cursor == start {
                return None;
            }
            let fraction = &text[start..*cursor];
            fraction_nonzero = fraction.bytes().any(|b| b != b'0');
            fraction_ms = format!("{fraction:0<3}")[..3].parse().ok()?;
        }
    }
    if hour > 24 || minute > 59 || second > 59 {
        return None;
    }
    if hour == 24 && (minute != 0 || second != 0 || fraction_nonzero) {
        return None;
    }
    let time_ms = ((hour * 60 + minute) * 60 + second) * 1000 + fraction_ms;
    let offset = match bytes.get(*cursor) {
        Some(b'Z' | b'z') => {
            *cursor += 1;
            Some(0)
        }
        Some(sign @ (b'+' | b'-')) => {
            *cursor += 1;
            let offset_hour = digits(bytes, cursor, 2)?;
            if bytes.get(*cursor) == Some(&b':') {
                *cursor += 1;
            }
            let offset_minute = digits(bytes, cursor, 2)?;
            if offset_hour > 23 || offset_minute > 59 {
                return None;
            }
            let magnitude = (offset_hour * 60 + offset_minute) * 60_000;
            Some(if *sign == b'-' { -magnitude } else { magnitude })
        }
        _ => None,
    };
    Some((time_ms, offset))
}

/// Days since 1970-01-01 for a proleptic Gregorian date; `day` may overflow the month.
fn days_from_civil(year: i128, month: i128, day: i128) -> i128 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// One event handed to the manual trigger handler.
#[derive(Clone, Debug, PartialEq)]
pub struct ManualEvent {
    pub receipt_id: String,
    pub organization_id: String,
    pub project_id: String,
    pub configuration_revision_id: String,
    pub source: String,
    pub delivery_id: String,
    pub connection_id: Option<String>,
    pub resource_id: Option<String>,
    pub received_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManualHttpResponse {
    pub status: u16,
    pub body: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManualDispatchError {
    /// The project is unknown, belongs to another organization, or has no active revision.
    ProjectConfigurationUnavailable,
}

impl std::fmt::Display for ManualDispatchError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("manual project configuration unavailable")
    }
}

impl std::error::Error for ManualDispatchError {}

pub(super) fn manual_json_error(message: &str) -> String {
    format!(
        "{{\"error\":{}}}",
        serde_json::to_string(message).unwrap_or_default()
    )
}

pub(super) fn manual_accepted_body(delivery_id: &str) -> String {
    format!(
        "{{\"status\":\"accepted\",\"deliveryId\":{}}}",
        serde_json::to_string(delivery_id).unwrap_or_default()
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunTrigger {
    pub name: String,
    pub on: String,
    /// `filters.from_users`; absent means no actor is allowed.
    pub from_users: Option<Vec<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunConfiguration {
    pub revision_id: String,
    pub triggers: Vec<RunTrigger>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManualRunPayload {
    pub expected_version_id: Option<String>,
    pub trigger: String,
    pub actor: String,
    pub public_delivery_key: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManualRunRejection {
    ConfigurationNotFound,
    ExpectedConfigurationNotCurrent,
    TriggerNotFound,
    ActorForbidden,
}

impl ManualRunRejection {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ConfigurationNotFound => "configuration_not_found",
            Self::ExpectedConfigurationNotCurrent => "expected_configuration_not_current",
            Self::TriggerNotFound => "trigger_not_found",
            Self::ActorForbidden => "actor_forbidden",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManualRunMatch {
    pub trigger_name: String,
    pub delivery_id: String,
    pub configuration_revision_id: String,
}

/// Matches a manual run to a configured trigger.
///
/// Not modelled: invocation input parsing and input filters (the baseline `parseInvocation`
/// path); callers here send no declared inputs.
///
/// # Errors
///
/// Returns the [`ManualRunRejection`] the baseline throws.
pub fn match_manual_run(
    revisions: &BTreeMap<String, RunConfiguration>,
    configuration_revision_id: &str,
    external_delivery_id: &str,
    payload: &ManualRunPayload,
) -> Result<ManualRunMatch, ManualRunRejection> {
    let stored = revisions
        .get(configuration_revision_id)
        .ok_or(ManualRunRejection::ConfigurationNotFound)?;
    if payload
        .expected_version_id
        .as_ref()
        .is_some_and(|expected| *expected != stored.revision_id)
    {
        return Err(ManualRunRejection::ExpectedConfigurationNotCurrent);
    }
    let trigger = stored
        .triggers
        .iter()
        .find(|candidate| candidate.name == payload.trigger && candidate.on == "manual.run")
        .ok_or(ManualRunRejection::TriggerNotFound)?;
    let allowed = trigger.from_users.as_ref().is_some_and(|users| {
        users
            .iter()
            .any(|user| user == "*" || *user == payload.actor)
    });
    if !allowed {
        return Err(ManualRunRejection::ActorForbidden);
    }
    Ok(ManualRunMatch {
        trigger_name: trigger.name.clone(),
        delivery_id: payload
            .public_delivery_key
            .clone()
            .unwrap_or_else(|| external_delivery_id.to_owned()),
        configuration_revision_id: stored.revision_id.clone(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthOutcome {
    Authorized,
    Unauthorized,
    Forbidden,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DispatchedRun {
    pub delivery_key: String,
    pub provider_event_receipt_id: String,
    pub trigger_run_id: String,
    pub configured_trigger_name: String,
    pub workflow_status: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManualRunResult {
    Dispatched(DispatchedRun),
    ProjectNotFound,
    ActorForbidden,
    DaemonOffline,
    ExpectedConfigurationNotCurrent,
    ConfigurationNotFound,
    TriggerNotFound,
    /// The run was created but rejected the submitted input; the issue list is empty here.
    InvalidInput {
        trigger_run_id: String,
    },
    DispatchConflict,
    InfrastructureUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub www_authenticate: Option<&'static str>,
    /// Response bytes, key order included.
    pub body: String,
}

const MANUAL_RUN_SCOPE: &str = "runs:dispatch";

fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

fn problem(
    request_id: &str,
    status: u16,
    code: &str,
    title: &str,
    detail: &str,
    with_empty_issues: bool,
) -> PublicResponse {
    let issues = if with_empty_issues {
        ",\"issues\":[]"
    } else {
        ""
    };
    PublicResponse {
        status,
        content_type: "application/problem+json",
        www_authenticate: (status == 401).then_some("Bearer"),
        body: format!(
            "{{\"type\":{},\"title\":{},\"status\":{status},\"detail\":{},\"code\":{},\"requestId\":{}{issues}}}",
            quoted(&format!(
                "https://paseo.sh/problems/{}",
                code.replace('_', "-")
            )),
            quoted(title),
            quoted(detail),
            quoted(code),
            quoted(request_id),
        ),
    }
}

/// Maps one `POST /api/v1/manual-runs` exchange to its HTTP response, body bytes included.
///
/// Not modelled: request schema validation issues; the body is assumed schema-valid once it is
/// JSON with a JSON content type.
#[must_use]
pub fn public_manual_run(
    auth: AuthOutcome,
    request_id: &str,
    content_type: Option<&str>,
    body: &[u8],
    result: &ManualRunResult,
) -> PublicResponse {
    let fail =
        |status, code, title, detail: &str| problem(request_id, status, code, title, detail, false);
    match auth {
        AuthOutcome::Unavailable => {
            return fail(
                503,
                "authentication_unavailable",
                "Authentication unavailable",
                "Bearer-credential authentication is currently unavailable. Retry the request later.",
            );
        }
        AuthOutcome::Unauthorized => {
            return fail(
                401,
                "unauthorized",
                "Authentication required",
                "Provide an active Paseo organization credential in the Authorization: Bearer header.",
            );
        }
        AuthOutcome::Forbidden => {
            return fail(
                403,
                "insufficient_scope",
                "Insufficient scope",
                &format!("This operation requires the {MANUAL_RUN_SCOPE} scope."),
            );
        }
        AuthOutcome::Authorized => {}
    }
    let json_content =
        content_type.is_some_and(|value| value.to_ascii_lowercase().contains("application/json"));
    if !json_content || decode_request_json(body).is_none() {
        return fail(
            400,
            "invalid_json",
            "Invalid JSON",
            "Send a JSON request body using Content-Type: application/json.",
        );
    }
    result_response(request_id, result)
}

fn result_response(request_id: &str, result: &ManualRunResult) -> PublicResponse {
    let fail =
        |status, code, title, detail: &str| problem(request_id, status, code, title, detail, false);
    match result {
        ManualRunResult::Dispatched(run) => PublicResponse {
            status: 200,
            content_type: "application/json",
            www_authenticate: None,
            body: format!(
                "{{\"deliveryKey\":{},\"providerEventReceiptId\":{},\"triggerRunId\":{},\"configuredTriggerName\":{},\"workflowStatus\":{}}}",
                quoted(&run.delivery_key),
                quoted(&run.provider_event_receipt_id),
                quoted(&run.trigger_run_id),
                quoted(&run.configured_trigger_name),
                quoted(&run.workflow_status),
            ),
        },
        ManualRunResult::ProjectNotFound => fail(
            404,
            "project_not_found",
            "Project not found",
            "No active project with that slug exists in the credential's organization.",
        ),
        ManualRunResult::ActorForbidden => fail(
            403,
            "actor_forbidden",
            "Actor forbidden",
            "The configured manual trigger does not allow this actor.",
        ),
        ManualRunResult::ConfigurationNotFound => fail(
            404,
            "configuration_not_found",
            "Configuration not found",
            "The requested configuration revision is not available.",
        ),
        ManualRunResult::TriggerNotFound => fail(
            404,
            "trigger_not_found",
            "Trigger not found",
            "The active configuration has no matching manual trigger.",
        ),
        ManualRunResult::ExpectedConfigurationNotCurrent => fail(
            409,
            "configuration_changed",
            "Configuration changed",
            "expectedVersionId is not the configuration version selected for this delivery.",
        ),
        ManualRunResult::DaemonOffline => fail(
            409,
            "daemon_offline",
            "Daemon offline",
            "The selected daemon is not connected. Reconnect it before retrying.",
        ),
        ManualRunResult::InvalidInput { trigger_run_id } => problem(
            request_id,
            400,
            "invalid_input",
            "Invalid trigger input",
            &format!("Run {trigger_run_id} rejected the submitted input."),
            true,
        ),
        ManualRunResult::DispatchConflict => fail(
            409,
            "dispatch_conflict",
            "Run not dispatched",
            "The durable event exists but no matching run is available yet. Retry with the same deliveryKey.",
        ),
        ManualRunResult::InfrastructureUnavailable => fail(
            503,
            "infrastructure_unavailable",
            "Service unavailable",
            "The operation could not reach durable storage. Retry the request later.",
        ),
    }
}
