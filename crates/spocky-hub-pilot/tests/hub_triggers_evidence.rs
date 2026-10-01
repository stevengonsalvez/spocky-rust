//! Rust side of the Hub trigger differential.
//!
//! The trace shape and case list mirror `scripts/phase2/hub-triggers-original.integration.test.ts`.
//! Every run compares the Rust trace byte for byte with the committed baseline trace
//! (`evidence/phase2/hub-triggers-original.json`); `SPOCKY_HUB_TRIGGERS_BASELINE` points at a fresh
//! capture instead, and `SPOCKY_HUB_TRIGGERS_OUTPUT` also writes the Rust trace to a file.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use serde_json::{Map, Number, Value, json};
use spocky_hub_pilot::triggers::{
    AcceptCall, AcceptFailure, Acceptance, AcceptedRunInput, AuthOutcome, DispatchedRun,
    ExecutionRequest, ExecutionReservation, ExecutionStatus, GitHubWebhook, GitHubWebhookRequest,
    LifecycleCall, ManualDispatchError, ManualEvent, ManualHttpResponse, ManualRunPayload,
    ManualRunResult, RunConfiguration, RunTrigger, TriggerStore, WebhookBackend,
    durable_execution_id, github_signature, hash_signature, match_manual_run, public_manual_run,
};

const PROJECT_A: &str = "11111111-1111-4111-8111-111111111111";
const PROJECT_B: &str = "22222222-2222-4222-8222-222222222222";
const GITHUB_SECRET: &str = "github-secret";

const RECEIVED_AT_GRID: [&str; 37] = [
    "2026",
    "2026-08",
    "2026-08-06",
    "2026-08-06T12:00",
    "2026-08-06T12:00:00",
    "2026-08-06T12:00Z",
    "2026-08-06T12:00:00z",
    "2026-08-06T12:00:00+0100",
    "2026-08-06T24:00:00Z",
    "2026-08-06T12:00:00.1234Z",
    "2026-02-30",
    "+002026-08-06T00:00:00Z",
    "-000001-01-01T00:00:00Z",
    "2026-08-06T12:00:00+23:59",
    "2026-08-06T12:00:00-00:00",
    "9999-12-31T23:59:59.999Z",
    "0000-01-01",
    "+275760-09-13T00:00:00.000Z",
    "2026-08-06T12Z",
    "2026-08-06T12:00:00+01",
    "2026-08-06T24:00:01Z",
    "2026-08-06T25:00:00Z",
    "2026-08-06T12:60:00Z",
    "2026-08-06T12:00:60Z",
    "2026-08-06T12:00:00.Z",
    "2026-02-32",
    "2026-13-01",
    "2026-00-10",
    "2026-08-00",
    "-000000-01-01T00:00:00Z",
    "275760-09-13T00:00:00Z",
    "+275760-09-13T00:00:00.001Z",
    "2026-08-06T12:00:00 Z",
    "2026-08-06T12:00:00+24:00",
    "26-08-06",
    "",
    "not-a-date",
];

/// Ordered JSON: object keys keep insertion order, as `JSON.stringify` does in the baseline capture.
#[derive(Clone)]
enum J {
    Null,
    Bool(bool),
    Number(String),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

macro_rules! obj {
    ($($key:expr => $value:expr),* $(,)?) => {
        J::Obj(vec![$(($key.to_string(), J::from($value))),*])
    };
}

impl J {
    fn render(&self, depth: usize, out: &mut String) {
        let pad = |level: usize| "  ".repeat(level);
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(value) => out.push_str(&value.to_string()),
            Self::Number(text) => out.push_str(text),
            Self::Str(text) => out.push_str(&serde_json::to_string(text).expect("string")),
            Self::Arr(items) if items.is_empty() => out.push_str("[]"),
            Self::Arr(items) => {
                out.push_str("[\n");
                for (index, item) in items.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    item.render(depth + 1, out);
                    out.push_str(if index + 1 == items.len() {
                        "\n"
                    } else {
                        ",\n"
                    });
                }
                out.push_str(&pad(depth));
                out.push(']');
            }
            Self::Obj(fields) if fields.is_empty() => out.push_str("{}"),
            Self::Obj(fields) => {
                out.push_str("{\n");
                for (index, (key, value)) in fields.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    out.push_str(&serde_json::to_string(key).expect("key"));
                    out.push_str(": ");
                    value.render(depth + 1, out);
                    out.push_str(if index + 1 == fields.len() {
                        "\n"
                    } else {
                        ",\n"
                    });
                }
                out.push_str(&pad(depth));
                out.push('}');
            }
        }
    }
}

impl From<bool> for J {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}
impl From<&str> for J {
    fn from(value: &str) -> Self {
        Self::Str(value.to_owned())
    }
}
impl From<String> for J {
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}
impl From<&String> for J {
    fn from(value: &String) -> Self {
        Self::Str(value.clone())
    }
}
impl From<i64> for J {
    fn from(value: i64) -> Self {
        Self::Number(value.to_string())
    }
}
impl From<usize> for J {
    fn from(value: usize) -> Self {
        Self::Number(value.to_string())
    }
}
impl From<u64> for J {
    fn from(value: u64) -> Self {
        Self::Number(value.to_string())
    }
}
impl From<u16> for J {
    fn from(value: u16) -> Self {
        Self::Number(value.to_string())
    }
}
impl From<Number> for J {
    fn from(value: Number) -> Self {
        Self::Number(value.to_string())
    }
}
impl From<Vec<J>> for J {
    fn from(value: Vec<J>) -> Self {
        Self::Arr(value)
    }
}
impl<T: Into<J>> From<Option<T>> for J {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Null, Into::into)
    }
}

const COMMITTED_BASELINE: &str =
    include_str!("../../../evidence/phase2/hub-triggers-original.json");

fn baseline_text() -> String {
    std::env::var_os("SPOCKY_HUB_TRIGGERS_BASELINE").map_or_else(
        || COMMITTED_BASELINE.to_owned(),
        |path| fs::read_to_string(path).expect("read baseline trace"),
    )
}

fn local_offset_minutes() -> i32 {
    std::env::var("SPOCKY_HUB_TRIGGERS_LOCAL_OFFSET_MINUTES")
        .map_or(0, |value| value.parse().expect("offset minutes"))
}

fn build_trace(baseline: &Value) -> String {
    let (manual, lease, execution, identity) = store_trace(baseline);
    let trace = obj! {
        "schemaVersion" => 1_usize,
        "manual" => manual,
        "lease" => lease,
        "execution" => execution,
        "identity" => identity,
        "durableExecutionId" => obj! {
            "fixed" => durable_execution_id("run-1", "revision-1", "deploy", Some("step-run-1")),
            "noStep" => durable_execution_id("run-1", "revision-1", "deploy", None),
            "otherTrigger" => durable_execution_id("run-1", "revision-1", "rollback", Some("step-run-1")),
        },
        "manualRequests" => manual_request_trace(),
        "manualRunMatch" => manual_run_match_trace(),
        "publicManualRun" => public_manual_run_trace(),
        "github" => github_trace(),
    };
    let mut text = String::new();
    trace.render(0, &mut text);
    text.push('\n');
    text
}

#[test]
fn rust_trace_is_byte_identical_to_the_baseline_trace() {
    let baseline = baseline_text();
    let parsed: Value = serde_json::from_str(&baseline).expect("baseline trace is JSON");
    let trace = build_trace(&parsed);
    if let Some(path) = std::env::var_os("SPOCKY_HUB_TRIGGERS_OUTPUT") {
        fs::write(path, &trace).expect("write trace");
    }
    assert!(
        trace == baseline,
        "Rust trace differs from the baseline trace; first differing line: {:?}",
        trace
            .lines()
            .zip(baseline.lines())
            .enumerate()
            .find(|(_, (rust, original))| rust != original)
    );
}

fn manual_body(org: &str, project: &str, delivery: &str) -> Vec<u8> {
    format!(
        r#"{{"organizationId":"{org}","projectId":"{project}","source":"manual.run","deliveryId":"{delivery}","payload":{{}}}}"#
    )
    .into_bytes()
}

fn baseline_ids(baseline: &Value, run: &str) -> (String, String, Vec<String>) {
    let entry = &baseline["identity"][run];
    let text = |value: &Value| value.as_str().expect("baseline id").to_owned();
    (
        text(&entry["runId"]),
        text(&entry["revisionId"]),
        entry["stepRunIds"]
            .as_array()
            .expect("step run ids")
            .iter()
            .map(text)
            .collect(),
    )
}

fn execution_request(ordinal: usize, started_at_ms: u64) -> ExecutionRequest {
    ExecutionRequest {
        step_id: if ordinal == 0 {
            "deploy-step"
        } else {
            "rollback-step"
        }
        .to_owned(),
        ordinal,
        started_at_ms,
        deadline_at_ms: 12_000,
        idle_deadline_at_ms: 20_000,
    }
}

#[allow(clippy::too_many_lines)]
fn store_trace(baseline: &Value) -> (J, J, J, J) {
    let (first_run_id, revision, first_steps) = baseline_ids(baseline, "firstRun");
    let (fan_run_id, _, fan_steps) = baseline_ids(baseline, "fanOutRun");
    let mut store = TriggerStore::default();
    store.register_project("org-a", PROJECT_A, &revision);
    store.register_project("org-b", PROJECT_B, "revision-b");
    let mut handled: Vec<ManualEvent> = Vec::new();
    for (org, project) in [
        ("org-a", PROJECT_A),
        ("org-a", PROJECT_A),
        ("org-b", PROJECT_B),
    ] {
        let mut sink = |event: ManualEvent| handled.push(event);
        store
            .handle_manual_request(
                Some(&mut sink),
                0,
                &manual_body(org, project, "shared-delivery"),
            )
            .expect("manual intake");
    }
    let receipt_of = |index: usize| handled[index].receipt_id.clone();
    let run_input = |receipt: String, trigger: &str| AcceptedRunInput {
        receipt_id: receipt,
        project_id: PROJECT_A.to_owned(),
        configuration_revision_id: revision.clone(),
        configured_trigger_name: trigger.to_owned(),
        step_ids: vec!["deploy-step".to_owned()],
        deadline_at_ms: 10_000,
        created_at_ms: 0,
        run_id: None,
        step_run_ids: None,
    };
    let first_input = AcceptedRunInput {
        run_id: Some(first_run_id.clone()),
        step_run_ids: Some(first_steps.clone()),
        ..run_input(receipt_of(0), "deploy")
    };
    let first_run = store.create_accepted_run(&first_input);
    let replay_run = store.create_accepted_run(&first_input);

    let first_lease = store.claim_wakeup(1_000, 500).expect("first lease");
    let ExecutionReservation::Created(first_execution) = store
        .reserve_execution(&first_run.run_id, &execution_request(0, 1_000))
        .expect("reservation")
    else {
        panic!("expected created execution");
    };
    let blocked_before_expiry = store.claim_wakeup(1_499, 500).is_none();
    let recovery_lease = store.claim_wakeup(1_500, 500).expect("recovery lease");
    let ExecutionReservation::Existing(recovered_execution) = store
        .reserve_execution(&first_run.run_id, &execution_request(0, 1_500))
        .expect("recovered reservation")
    else {
        panic!("expected existing execution");
    };
    let executions_after_recovery = store.execution_count();
    store.release_wakeup(&first_lease, 1_502);
    let stale_release_rejected = store.claim_wakeup(1_502, 500).is_none();
    store.release_wakeup(&recovery_lease, 1_502);
    let current_release_accepted = store.claim_wakeup(1_502, 500).is_some();

    let fan_input = AcceptedRunInput {
        run_id: Some(fan_run_id.clone()),
        step_ids: vec!["rollback-step".to_owned(), "rollback-step".to_owned()],
        step_run_ids: Some(fan_steps.clone()),
        ..run_input(receipt_of(0), "rollback")
    };
    let fan_out = store.create_accepted_run(&fan_input);
    let fan_out_replay = store.create_accepted_run(&fan_input);
    let ExecutionReservation::Created(second_step) = store
        .reserve_execution(&fan_out.run_id, &execution_request(1, 1_600))
        .expect("second step reservation")
    else {
        panic!("expected created second-step execution");
    };
    let missing_ordinal =
        match store.reserve_execution(&fan_out.run_id, &execution_request(5, 1_600)) {
            None => "threw: workflow step run not found",
            Some(_) => "no error",
        };
    store.create_accepted_run(&AcceptedRunInput {
        project_id: PROJECT_B.to_owned(),
        configuration_revision_id: "revision-b".to_owned(),
        ..run_input(receipt_of(2), "deploy")
    });

    let running = store
        .transition_execution(&first_execution.id, ExecutionStatus::Running, 2_010)
        .expect("running");
    let succeeded = store
        .transition_execution(&first_execution.id, ExecutionStatus::Succeeded, 2_020)
        .expect("succeeded");
    let conflicting = store
        .transition_execution(&first_execution.id, ExecutionStatus::Failed, 2_030)
        .expect("conflicting");
    let run_succeeded = store.succeed_run(&first_run.run_id).expect("run");
    let run_succeeded_again = store.succeed_run(&first_run.run_id).expect("run");

    let manual = obj! {
        "firstCreated" => first_run.created,
        "replayCreated" => replay_run.created,
        "sameReceipt" => receipt_of(1) == receipt_of(0),
        "crossOrgDistinct" => receipt_of(2) != receipt_of(0),
        "receiptCount" => store.receipt_count(),
        "runCount" => store.run_count(),
        "fanOutDistinct" => fan_out.run_id != first_run.run_id,
        "fanOutCreated" => fan_out.created,
        "fanOutReplayCreated" => fan_out_replay.created,
        "fanOutReplaySameRun" => fan_out_replay.run_id == fan_out.run_id,
    };
    let lease = obj! {
        "blockedBeforeExpiry" => blocked_before_expiry,
        "recoveredAfterExpiry" => recovery_lease.run_id == first_run.run_id,
        "leasedBeforeClaim" => recovery_lease.leased_before_claim,
        "sameExecution" => recovered_execution.id == first_execution.id,
        "executionCount" => executions_after_recovery,
        "staleReleaseRejected" => stale_release_rejected,
        "currentReleaseAccepted" => current_release_accepted,
    };
    let execution = obj! {
        "initial" => first_execution.status.name(),
        "runningTransition" => running.transitioned,
        "firstTerminal" => succeeded.execution.status.name(),
        "conflictingTerminalTransition" => conflicting.transitioned,
        "finalStatus" => conflicting.execution.status.name(),
        "completedAtKept" => conflicting.execution.completed_at_ms == succeeded.execution.completed_at_ms,
        "idleDeadlineAtMs" => first_execution.idle_deadline_at_ms,
        "idleDeadlineCleared" => conflicting.execution.idle_deadline_at_ms.is_none(),
        "runStatus" => store.run(&first_run.run_id).expect("run").status,
        "runSucceededTransition" => run_succeeded.transitioned,
        "runSucceededAgainTransition" => run_succeeded_again.transitioned,
    };
    let ids = |values: &[String]| values.iter().map(J::from).collect::<Vec<J>>();
    let identity = obj! {
        "localOffsetMinutes" => i64::from(local_offset_minutes()),
        "firstRun" => obj! {
            "runId" => &first_run.run_id,
            "revisionId" => &revision,
            "stepRunIds" => ids(&first_steps),
            "executionId" => &first_execution.id,
        },
        "fanOutRun" => obj! {
            "runId" => &fan_out.run_id,
            "revisionId" => &revision,
            "stepRunIds" => ids(&fan_steps),
            "executionId" => &second_step.id,
            "selectedStepRunId" => &second_step.step_run_id,
            "missingOrdinal" => missing_ordinal,
        },
    };
    (manual, lease, execution, identity)
}

fn delivery(project: &str, org: &str, overrides: &[(&str, Value)]) -> Vec<u8> {
    let mut map = Map::new();
    map.insert("organizationId".into(), json!(org));
    map.insert("projectId".into(), json!(project));
    map.insert("source".into(), json!("discord.mention"));
    map.insert("deliveryId".into(), json!("manual-1"));
    map.insert("payload".into(), json!({"guildId": "guild-1"}));
    for (key, value) in overrides {
        map.insert((*key).to_owned(), value.clone());
    }
    Value::Object(map).to_string().into_bytes()
}

const NOW_MS: i64 = 1_700_000_000_000;

fn deliver(
    store: &mut TriggerStore,
    events: Option<&mut Vec<ManualEvent>>,
    body: &[u8],
) -> Result<ManualHttpResponse, ManualDispatchError> {
    match events {
        Some(events) => {
            let mut sink = |event: ManualEvent| events.push(event);
            store.handle_manual_request(Some(&mut sink), NOW_MS, body)
        }
        None => store.handle_manual_request(None, NOW_MS, body),
    }
}

fn record(
    cases: &mut Vec<(String, J)>,
    store: &mut TriggerStore,
    name: &str,
    events: Option<&mut Vec<ManualEvent>>,
    body: &[u8],
) {
    let outcome = match deliver(store, events, body) {
        Ok(response) => obj! {"status" => response.status, "body" => response.body},
        Err(error) => obj! {"status" => 0_u16, "body" => format!("threw: {error}")},
    };
    cases.push((name.to_owned(), outcome));
}

#[allow(clippy::too_many_lines)]
fn manual_request_trace() -> J {
    let mut store = TriggerStore::default();
    store.register_project("org_1", PROJECT_A, "revision-1");
    store.set_local_offset_minutes(local_offset_minutes());
    store.register_project("org_2", PROJECT_B, "revision-2");
    let mut recording: Vec<ManualEvent> = Vec::new();
    let mut cases: Vec<(String, J)> = Vec::new();
    let a = |overrides: &[(&str, Value)]| delivery(PROJECT_A, "org_1", overrides);
    let uuid_nine = "7f1b0c1e-2d3a-9b5c-8d6e-9f0a1b2c3d4e";

    record(
        &mut cases,
        &mut store,
        "accepted",
        Some(&mut recording),
        &a(&[]),
    );
    let accepted_evidence: Vec<J> = recording
        .iter()
        .map(|event| {
            obj! {
                "connectionId" => event.connection_id.clone(),
                "resourceId" => event.resource_id.clone(),
                "source" => &event.source,
                "deliveryId" => &event.delivery_id,
            }
        })
        .collect();
    record(
        &mut cases,
        &mut store,
        "duplicateSameOrganization",
        Some(&mut recording),
        &a(&[]),
    );
    let handled_after_duplicate = recording.len();
    record(
        &mut cases,
        &mut store,
        "sameDeliveryOtherOrganization",
        Some(&mut recording),
        &delivery(PROJECT_B, "org_2", &[]),
    );
    let handled_after_other = recording.len();
    record(
        &mut cases,
        &mut store,
        "noHandlerDropped",
        None,
        &a(&[("deliveryId", json!("manual-idle"))]),
    );
    record(
        &mut cases,
        &mut store,
        "withConnectionEvidence",
        Some(&mut recording),
        &a(&[
            ("deliveryId", json!("manual-evidence")),
            (
                "connectionId",
                json!("7f1b0c1e-2d3a-4b5c-8d6e-9f0a1b2c3d4e"),
            ),
            ("resourceId", json!("resource-1")),
        ]),
    );
    let last = recording.last().expect("handled event");
    let connection_evidence = obj! {"connectionId" => last.connection_id.clone(), "resourceId" => last.resource_id.clone()};
    let receipt_of = |store: &TriggerStore, org: &str, delivery: &str| {
        store.receipt(org, delivery).map_or(J::Null, |receipt| {
            obj! {
                "provider" => receipt.provider,
                "source" => &receipt.source,
                "droppedReason" => receipt.dropped_reason,
                "connectionId" => receipt.connection_id.clone(),
                "resourceId" => receipt.resource_id.clone(),
            }
        })
    };
    let receipts = obj! {
        "accepted" => receipt_of(&store, "org_1", "manual-1"),
        "idle" => receipt_of(&store, "org_1", "manual-idle"),
        "otherOrganization" => receipt_of(&store, "org_2", "manual-1"),
        "distinctReceiptIds" => store.receipt("org_1", "manual-1").map(|r| &r.id)
            != store.receipt("org_2", "manual-1").map(|r| &r.id),
    };

    let simple: Vec<(&str, Vec<u8>)> = vec![
        ("nonNamespacedSource", a(&[("source", json!("manual"))])),
        ("emptySource", a(&[("source", json!(""))])),
        ("emptyDeliveryId", a(&[("deliveryId", json!(""))])),
        ("emptyOrganizationId", a(&[("organizationId", json!(""))])),
        ("projectIdNotUuid", a(&[("projectId", json!("project-1"))])),
        ("invalidReceivedAt", a(&[("receivedAt", json!("not-a-date"))])),
        ("receivedAtNumber", a(&[("receivedAt", json!(1))])),
        (
            "validReceivedAt",
            a(&[
                ("deliveryId", json!("manual-received-at")),
                ("receivedAt", json!("2026-08-06T12:00:00.000Z")),
            ]),
        ),
        (
            "missingOrganizationId",
            json!({"projectId": PROJECT_A, "source": "discord.mention", "deliveryId": "manual-missing"})
                .to_string()
                .into_bytes(),
        ),
        ("connectionIdNotUuid", a(&[("connectionId", json!("connection-1"))])),
        ("connectionIdNumber", a(&[("connectionId", json!(7))])),
        (
            "connectionIdNull",
            a(&[
                ("deliveryId", json!("manual-null-connection")),
                ("connectionId", Value::Null),
                ("resourceId", Value::Null),
            ]),
        ),
        (
            "connectionIdNilUuid",
            a(&[
                ("deliveryId", json!("manual-nil-connection")),
                ("connectionId", json!("00000000-0000-0000-0000-000000000000")),
            ]),
        ),
        ("connectionIdVersionNine", a(&[("connectionId", json!(uuid_nine))])),
        ("resourceIdNumber", a(&[("resourceId", json!(7))])),
        (
            "sourceMissing",
            json!({"organizationId": "org_1", "projectId": PROJECT_A, "deliveryId": "manual-no-source"})
                .to_string()
                .into_bytes(),
        ),
        ("sourceNumber", a(&[("source", json!(7))])),
        ("deliveryIdNumber", a(&[("deliveryId", json!(7))])),
        (
            "projectIdMissing",
            json!({"organizationId": "org_1", "source": "discord.mention", "deliveryId": "manual-no-project"})
                .to_string()
                .into_bytes(),
        ),
        ("receivedAtNull", a(&[("receivedAt", Value::Null)])),
        (
            "receivedAtDateOnly",
            a(&[("deliveryId", json!("manual-date-only")), ("receivedAt", json!("2026-08-06"))]),
        ),
        (
            "receivedAtOffset",
            a(&[
                ("deliveryId", json!("manual-offset")),
                ("receivedAt", json!("2026-08-06T12:00:00+01:00")),
            ]),
        ),
        ("receivedAtImpossibleDate", a(&[("receivedAt", json!("2026-02-30"))])),
        (
            "multipleInvalidFields",
            a(&[
                ("organizationId", json!("")),
                ("projectId", json!("project-1")),
                ("source", json!("manual")),
            ]),
        ),
        (
            "payloadOmitted",
            json!({"organizationId": "org_1", "projectId": PROJECT_A, "source": "discord.mention", "deliveryId": "manual-no-payload"})
                .to_string()
                .into_bytes(),
        ),
        (
            "payloadNull",
            a(&[("deliveryId", json!("manual-null-payload")), ("payload", Value::Null)]),
        ),
    ];
    for (name, body) in &simple {
        record(&mut cases, &mut store, name, Some(&mut recording), body);
    }
    let mut bom = vec![0xef, 0xbb, 0xbf];
    bom.extend(a(&[("deliveryId", json!("manual-bom"))]));
    record(
        &mut cases,
        &mut store,
        "utf8BomBody",
        Some(&mut recording),
        &bom,
    );
    for (name, count, delivery_id) in [
        ("doubleBomBody", 2, "manual-double-bom"),
        ("tripleBomBody", 3, "manual-triple-bom"),
    ] {
        let mut body = [0xef, 0xbb, 0xbf].repeat(count);
        body.extend(a(&[("deliveryId", json!(delivery_id))]));
        record(&mut cases, &mut store, name, Some(&mut recording), &body);
    }
    record(
        &mut cases,
        &mut store,
        "unknownProject",
        Some(&mut recording),
        &a(&[
            ("deliveryId", json!("manual-unknown-project")),
            ("projectId", json!("7f1b0c1e-2d3a-4b5c-8d6e-9f0a1b2c3d4e")),
        ]),
    );
    record(
        &mut cases,
        &mut store,
        "invalidJson",
        Some(&mut recording),
        b"{not json",
    );
    record(
        &mut cases,
        &mut store,
        "arrayBody",
        Some(&mut recording),
        b"[]",
    );

    let mut grid: Vec<(String, J)> = Vec::new();
    for (index, received_at) in RECEIVED_AT_GRID.iter().enumerate() {
        let body = a(&[
            ("deliveryId", json!(format!("manual-grid-{index}"))),
            ("receivedAt", json!(received_at)),
        ]);
        let before = recording.len();
        let status = deliver(&mut store, Some(&mut recording), &body)
            .expect("grid intake")
            .status;
        let received_at_ms = (recording.len() > before)
            .then(|| recording.last().map(|event| event.received_at_ms))
            .flatten();
        grid.push((
            (*received_at).to_owned(),
            obj! {"status" => status, "receivedAtMs" => received_at_ms},
        ));
    }
    let before_omitted = recording.len();
    let omitted = a(&[("deliveryId", json!("manual-omitted-received-at"))]);
    deliver(&mut store, Some(&mut recording), &omitted).expect("omitted receivedAt");
    let omitted_is_now = recording[before_omitted].received_at_ms == NOW_MS;
    obj! {
        "cases" => J::Obj(cases),
        "receivedAtGrid" => J::Obj(grid),
        "omittedReceivedAtIsNow" => omitted_is_now,
        "handledAfterDuplicate" => handled_after_duplicate,
        "handledAfterOtherOrganization" => handled_after_other,
        "receipts" => receipts,
        "acceptedEvidence" => accepted_evidence,
        "connectionEvidence" => connection_evidence,
    }
}

fn manual_run_match_trace() -> J {
    let current = "11111111-1111-4111-8111-111111111111";
    let stale = "22222222-2222-4222-8222-222222222222";
    let trigger = |name: &str, users: &[&str]| RunTrigger {
        name: name.to_owned(),
        on: "manual.run".to_owned(),
        from_users: Some(users.iter().map(|user| (*user).to_owned()).collect()),
    };
    let mut revisions = BTreeMap::new();
    revisions.insert(
        current.to_owned(),
        RunConfiguration {
            revision_id: current.to_owned(),
            triggers: vec![
                trigger("deploy", &["*"]),
                trigger("rollback", &["alice"]),
                RunTrigger {
                    name: "open".to_owned(),
                    on: "manual.run".to_owned(),
                    from_users: None,
                },
                RunTrigger {
                    name: "cron".to_owned(),
                    on: "schedule.tick".to_owned(),
                    from_users: Some(vec!["*".to_owned()]),
                },
            ],
        },
    );
    let run =
        |revision: &str, trigger: &str, actor: &str, expected: Option<&str>, key: Option<&str>| {
            let payload = ManualRunPayload {
                expected_version_id: expected.map(str::to_owned),
                trigger: trigger.to_owned(),
                actor: actor.to_owned(),
                public_delivery_key: key.map(str::to_owned),
            };
            match match_manual_run(&revisions, revision, "delivery-1", &payload) {
                Ok(found) => obj! {
                    "outcome" => "matched",
                    "triggers" => vec![J::from(found.trigger_name)],
                    "deliveryId" => found.delivery_id,
                    "configurationRevisionIsCurrent" => found.configuration_revision_id == current,
                },
                Err(rejection) => obj! {"outcome" => "rejected", "code" => rejection.code()},
            }
        };
    obj! {
        "matched" => run(current, "deploy", "anyone", None, None),
        "publicDeliveryKey" => run(current, "deploy", "anyone", None, Some("public-key")),
        "expectedCurrent" => run(current, "deploy", "anyone", Some(current), None),
        "expectedStale" => run(current, "deploy", "anyone", Some(stale), None),
        "revisionMissing" => run(stale, "deploy", "anyone", None, None),
        "triggerMissing" => run(current, "absent", "anyone", None, None),
        "actorForbidden" => run(current, "rollback", "mallory", None, None),
        "actorAllowed" => run(current, "rollback", "alice", None, None),
        "noUserFilter" => run(current, "open", "alice", None, None),
        "wrongEventTrigger" => run(current, "cron", "alice", None, None),
    }
}

fn public_manual_run_trace() -> J {
    let dispatched = DispatchedRun {
        delivery_key: "delivery-1".to_owned(),
        provider_event_receipt_id: "845e9d26-7977-45e1-bc69-d80a7b55a9cc".to_owned(),
        trigger_run_id: "f83dc934-02a0-4849-8de7-699110be24ed".to_owned(),
        configured_trigger_name: "deploy".to_owned(),
        workflow_status: "running".to_owned(),
    };
    let body = br#"{"projectSlug":"project","trigger":"deploy","actor":"alice","deliveryKey":"delivery-1","input":{}}"#;
    let call =
        |auth: AuthOutcome, result: &ManualRunResult, content_type: Option<&str>, body: &[u8]| {
            let response = public_manual_run(auth, "request-1", content_type, body, result);
            obj! {
                "status" => response.status,
                "contentType" => response.content_type,
                "wwwAuthenticate" => response.www_authenticate,
                "body" => response.body,
            }
        };
    let ok = |result: &ManualRunResult| {
        call(
            AuthOutcome::Authorized,
            result,
            Some("application/json"),
            body,
        )
    };
    let dispatched_result = ManualRunResult::Dispatched(dispatched);
    let with_boms = |count: usize| {
        let prefixed = [[0xef, 0xbb, 0xbf].repeat(count).as_slice(), body].concat();
        call(
            AuthOutcome::Authorized,
            &dispatched_result,
            Some("application/json"),
            &prefixed,
        )
    };
    let mut lossy = br#"{"projectSlug":"project","trigger":"deploy","actor":"alice","deliveryKey":"delivery-1","input":""#.to_vec();
    lossy.extend([0xff, b'"', b'}']);
    obj! {
        "results" => obj! {
            "dispatched" => ok(&dispatched_result),
            "project_not_found" => ok(&ManualRunResult::ProjectNotFound),
            "actor_forbidden" => ok(&ManualRunResult::ActorForbidden),
            "daemon_offline" => ok(&ManualRunResult::DaemonOffline),
            "expected_configuration_not_current" => ok(&ManualRunResult::ExpectedConfigurationNotCurrent),
            "configuration_not_found" => ok(&ManualRunResult::ConfigurationNotFound),
            "trigger_not_found" => ok(&ManualRunResult::TriggerNotFound),
            "dispatch_conflict" => ok(&ManualRunResult::DispatchConflict),
            "invalid_input" => ok(&ManualRunResult::InvalidInput {
                trigger_run_id: "f83dc934-02a0-4849-8de7-699110be24ed".to_owned(),
            }),
            "infrastructure_unavailable" => ok(&ManualRunResult::InfrastructureUnavailable),
        },
        "unauthorized" => call(AuthOutcome::Unauthorized, &dispatched_result, Some("application/json"), body),
        "forbidden" => call(AuthOutcome::Forbidden, &dispatched_result, Some("application/json"), body),
        "authenticationUnavailable" => call(AuthOutcome::Unavailable, &dispatched_result, Some("application/json"), body),
        "invalidJson" => call(AuthOutcome::Authorized, &dispatched_result, Some("application/json"), b"{not json"),
        "wrongContentType" => call(AuthOutcome::Authorized, &dispatched_result, Some("text/plain"), body),
        "missingContentType" => call(AuthOutcome::Authorized, &dispatched_result, None, body),
        "bomBody" => with_boms(1),
        "doubleBomBody" => with_boms(2),
        "tripleBomBody" => with_boms(3),
        "invalidUtf8InString" => call(
            AuthOutcome::Authorized,
            &dispatched_result,
            Some("application/json"),
            &lossy,
        ),
    }
}

#[derive(Clone, Copy)]
enum Signature {
    Valid,
    None,
    WrongSecret,
    Truncated,
    RawHex,
    OfBody(&'static str),
}

struct Delivery {
    id: Option<String>,
    event_type: Option<String>,
    signature: Signature,
    body: Vec<u8>,
}

struct Case {
    name: &'static str,
    secret: Option<String>,
    handlers: usize,
    events_per_acceptance: usize,
    failure: Option<AcceptFailure>,
    deliveries: Vec<Delivery>,
}

const VALID: &str = r#"{"action":"created","installation":{"id":42},"repository":{"id":9001,"full_name":"acme/widgets"}}"#;
const REPOSITORY: &str = r#""repository":{"id":9001,"full_name":"acme/widgets"}"#;

fn padded(bytes: usize) -> String {
    let base =
        format!(r#"{{"action":"created","installation":{{"id":42}},{REPOSITORY},"pad":""}}"#);
    format!(
        r#"{{"action":"created","installation":{{"id":42}},{REPOSITORY},"pad":"{}"}}"#,
        "x".repeat(bytes - base.len())
    )
}

fn case(name: &'static str, body: Vec<u8>) -> Case {
    Case {
        name,
        secret: Some(GITHUB_SECRET.to_owned()),
        handlers: 1,
        events_per_acceptance: 1,
        failure: None,
        deliveries: vec![Delivery {
            id: Some("d-1".to_owned()),
            event_type: Some("issue_comment".to_owned()),
            signature: Signature::Valid,
            body,
        }],
    }
}

fn header_case(
    name: &'static str,
    delivery_id: Option<String>,
    event_type: Option<String>,
) -> Case {
    let mut built = case(name, VALID.as_bytes().to_vec());
    built.deliveries[0].id = delivery_id;
    built.deliveries[0].event_type = event_type;
    built
}

fn signature_case(name: &'static str, signature: Signature, body: &str) -> Case {
    let mut built = case(name, body.as_bytes().to_vec());
    built.deliveries[0].signature = signature;
    built
}

#[allow(clippy::too_many_lines)]
fn webhook_cases() -> Vec<Case> {
    let text = |name: &'static str, body: &str| case(name, body.as_bytes().to_vec());
    let with = |mut built: Case, edit: &dyn Fn(&mut Case)| {
        edit(&mut built);
        built
    };
    let mut bom = vec![0xef, 0xbb, 0xbf];
    bom.extend(VALID.as_bytes());
    let event = |name: &'static str, event_type: &str, body: &str| {
        let mut built = text(name, body);
        built.deliveries[0].event_type = Some(event_type.to_owned());
        built
    };
    let mut double_bom = vec![0xef, 0xbb, 0xbf, 0xef, 0xbb, 0xbf];
    double_bom.extend(VALID.as_bytes());
    let tampered = format!("{VALID} ");
    let mut replay = text("replayAndDistinct", VALID);
    replay.deliveries = ["d-1", "d-1", "d-2"]
        .iter()
        .map(|id| Delivery {
            id: Some((*id).to_owned()),
            event_type: Some("issue_comment".to_owned()),
            signature: Signature::Valid,
            body: VALID.as_bytes().to_vec(),
        })
        .collect();
    vec![
        text("validDispatch", VALID),
        with(text("unconfiguredSecret", VALID), &|c| c.secret = None),
        signature_case("missingSignature", Signature::None, VALID),
        signature_case("wrongSecretSignature", Signature::WrongSecret, VALID),
        signature_case("truncatedSignature", Signature::Truncated, VALID),
        signature_case("signatureWithoutPrefix", Signature::RawHex, VALID),
        signature_case("tamperedBody", Signature::OfBody(VALID), &tampered),
        with(text("longSecret", VALID), &|c| {
            c.secret = Some("s".repeat(100));
        }),
        with(text("emptySecret", VALID), &|c| {
            c.secret = Some(String::new());
        }),
        case("bodyAtLimit", padded(1_048_576).into_bytes()),
        case("bodyOverLimit", padded(1_048_577).into_bytes()),
        header_case(
            "missingDeliveryHeader",
            None,
            Some("issue_comment".to_owned()),
        ),
        header_case(
            "emptyDeliveryHeader",
            Some(String::new()),
            Some("issue_comment".to_owned()),
        ),
        header_case("missingEventHeader", Some("d-1".to_owned()), None),
        header_case(
            "emptyEventHeader",
            Some("d-1".to_owned()),
            Some(String::new()),
        ),
        header_case(
            "deliveryHeaderAtLimit",
            Some("d".repeat(128)),
            Some("issue_comment".to_owned()),
        ),
        header_case(
            "deliveryHeaderOverLimit",
            Some("d".repeat(129)),
            Some("issue_comment".to_owned()),
        ),
        header_case(
            "eventHeaderOverLimit",
            Some("d-1".to_owned()),
            Some("e".repeat(129)),
        ),
        text("malformedJson", "{not json"),
        case("invalidUtf8", vec![0x7b, 0x22, 0xff, 0x22, 0x7d]),
        case("utf8Bom", bom),
        case("doubleUtf8Bom", double_bom),
        text("arrayBody", "[]"),
        text("nullBody", "null"),
        text("stringBody", "\"text\""),
        text("numberBody", "7"),
        text("missingInstallation", &format!("{{{REPOSITORY}}}")),
        text(
            "installationIdString",
            &format!(r#"{{"action":"created","installation":{{"id":"42"}},{REPOSITORY}}}"#),
        ),
        text(
            "installationWithoutId",
            &format!(r#"{{"action":"created","installation":{{}},{REPOSITORY}}}"#),
        ),
        text(
            "installationIdFractional",
            &format!(r#"{{"action":"created","installation":{{"id":1.5}},{REPOSITORY}}}"#),
        ),
        text(
            "duplicateInstallationKey",
            r#"{"installation":{"id":"x"},"installation":{"id":7},"repository":{"id":1,"full_name":"a/b"}}"#,
        ),
        event(
            "lifecycleInstallation",
            "installation",
            r#"{"installation":{"id":42},"action":"created"}"#,
        ),
        event(
            "lifecycleInstallationRepositories",
            "installation_repositories",
            r#"{"installation":{"id":42},"action":"added"}"#,
        ),
        text("noRepository", r#"{"installation":{"id":42}}"#),
        text(
            "emptyRepositoryName",
            r#"{"installation":{"id":42},"repository":{"id":1,"full_name":""}}"#,
        ),
        text(
            "repositoryWithoutId",
            r#"{"installation":{"id":42},"repository":{"full_name":"a/b"}}"#,
        ),
        text(
            "repositoryIdString",
            r#"{"installation":{"id":42},"repository":{"id":"1","full_name":"a/b"}}"#,
        ),
        with(text("noHandlers", VALID), &|c| c.handlers = 0),
        with(text("twoHandlers", VALID), &|c| c.handlers = 2),
        with(text("fanOutTwoEvents", VALID), &|c| {
            c.events_per_acceptance = 2;
        }),
        with(text("storageUnavailable", VALID), &|c| {
            c.failure = Some(AcceptFailure::DatabaseUnavailable);
        }),
        with(text("storageFailure", VALID), &|c| {
            c.failure = Some(AcceptFailure::Unexpected);
        }),
        replay,
    ]
}

fn signature_for(signature: Signature, secret: &str, body: &[u8]) -> Option<String> {
    match signature {
        Signature::Valid => Some(github_signature(secret, body)),
        Signature::None => None,
        Signature::WrongSecret => Some(github_signature("other-secret", body)),
        Signature::Truncated => {
            let full = github_signature(secret, body);
            Some(full[..full.len() - 2].to_owned())
        }
        Signature::RawHex => Some(github_signature(secret, body)["sha256=".len()..].to_owned()),
        Signature::OfBody(signed) => Some(github_signature(secret, signed.as_bytes())),
    }
}

/// Recording acceptance boundary shared (as the same stub) with the baseline capture: the first
/// delivery is accepted, a repeat is a duplicate, and a drop reason makes it a dropped receipt.
#[derive(Default)]
struct Recording {
    seen: BTreeSet<String>,
    accepts: Vec<AcceptCall>,
    lifecycles: Vec<LifecycleCall>,
    dispatch_count: usize,
    events_per_acceptance: usize,
    failure: Option<AcceptFailure>,
}

impl WebhookBackend for Recording {
    fn accept(&mut self, call: &AcceptCall) -> Result<Acceptance, AcceptFailure> {
        self.accepts.push(call.clone());
        if let Some(failure) = self.failure {
            return Err(failure);
        }
        if !self.seen.insert(call.delivery_id.clone()) {
            return Ok(Acceptance::Duplicate);
        }
        if call.drop_reason.is_some() {
            return Ok(Acceptance::Dropped);
        }
        Ok(Acceptance::Accepted {
            events: self.events_per_acceptance,
        })
    }

    fn apply_lifecycle(&mut self, call: &LifecycleCall) {
        self.lifecycles.push(call.clone());
    }

    fn dispatch(&mut self, events: usize, handlers: usize) {
        self.dispatch_count += events * handlers;
    }
}

fn github_trace() -> J {
    let mut cases: Vec<(String, J)> = Vec::new();
    for spec in webhook_cases() {
        let mut endpoint = GitHubWebhook::new(
            spec.secret.as_deref(),
            Recording {
                events_per_acceptance: spec.events_per_acceptance,
                failure: spec.failure,
                ..Recording::default()
            },
        );
        for _ in 0..spec.handlers {
            endpoint.start_handler();
        }
        let responses: Vec<J> = spec
            .deliveries
            .iter()
            .map(|delivery| {
                let signature = signature_for(
                    delivery.signature,
                    spec.secret.as_deref().unwrap_or(GITHUB_SECRET),
                    &delivery.body,
                );
                let response = endpoint.handle(GitHubWebhookRequest {
                    delivery_id: delivery.id.as_deref(),
                    event_type: delivery.event_type.as_deref(),
                    signature: signature.as_deref(),
                    body: &delivery.body,
                });
                obj! {"status" => response.status, "body" => response.body}
            })
            .collect();
        let accepts: Vec<J> = endpoint
            .backend()
            .accepts
            .iter()
            .map(|call| {
                obj! {
                    "source" => &call.source,
                    "dropReason" => call.drop_reason,
                    "installationId" => call.installation_id.clone(),
                    "repositoryId" => call.repository_id.clone(),
                    "repo" => call.repo.clone(),
                    "signatureHash" => &call.signature_hash,
                }
            })
            .collect();
        let lifecycles: Vec<J> = endpoint
            .backend()
            .lifecycles
            .iter()
            .map(|call| {
                obj! {
                    "event" => &call.event,
                    "source" => &call.source,
                    "installationId" => call.installation_id.clone(),
                    "signatureHash" => &call.signature_hash,
                }
            })
            .collect();
        cases.push((
            spec.name.to_owned(),
            obj! {
                "responses" => responses,
                "accepts" => accepts,
                "lifecycles" => lifecycles,
                "dispatchCount" => endpoint.backend().dispatch_count,
            },
        ));
    }
    obj! {
        "signatureHashSample" => hash_signature(&github_signature(GITHUB_SECRET, b"{}")),
        "cases" => J::Obj(cases),
    }
}
