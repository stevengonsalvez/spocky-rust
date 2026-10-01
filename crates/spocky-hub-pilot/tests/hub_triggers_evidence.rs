//! Writes the Rust side of the Hub trigger differential when `SPOCKY_HUB_TRIGGERS_OUTPUT` is set.
//!
//! The trace shape and case list mirror `scripts/phase2/hub-triggers-original.integration.test.ts`.

use std::collections::BTreeMap;
use std::fs;

use serde_json::{Map, Value, json};
use spocky_hub_pilot::triggers::{
    AcceptFailure, AcceptedRunInput, AuthOutcome, ExecutionReservation, ExecutionStatus,
    GitHubWebhook, GitHubWebhookRequest, ManualRunPayload, ManualRunResult, ManualSource,
    RunConfiguration, RunTrigger, TriggerStore, durable_execution_id, github_signature,
    hash_signature, match_manual_run, public_manual_run,
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

#[test]
fn writes_candidate_trace_when_requested() {
    let Some(output_path) = std::env::var_os("SPOCKY_HUB_TRIGGERS_OUTPUT") else {
        return;
    };
    let (manual, lease, execution) = store_trace();
    let trace = json!({
        "schemaVersion": 1,
        "manual": manual,
        "lease": lease,
        "execution": execution,
        "durableExecutionId": {
            "fixed": durable_execution_id("run-1", "revision-1", "deploy", Some("step-run-1")),
            "noStep": durable_execution_id("run-1", "revision-1", "deploy", None),
            "otherTrigger": durable_execution_id("run-1", "revision-1", "rollback", Some("step-run-1")),
        },
        "manualRequests": manual_request_trace(),
        "manualRunMatch": manual_run_match_trace(),
        "publicManualRun": public_manual_run_trace(),
        "github": github_trace(),
    });
    fs::write(
        output_path,
        format!(
            "{}\n",
            serde_json::to_string_pretty(&trace).expect("serialize trace")
        ),
    )
    .expect("write trace");
}

fn manual_body(org: &str, project: &str, delivery: &str) -> Vec<u8> {
    json!({
        "organizationId": org,
        "projectId": project,
        "source": "manual.run",
        "deliveryId": delivery,
        "payload": {},
    })
    .to_string()
    .into_bytes()
}

#[allow(clippy::too_many_lines)]
fn store_trace() -> (Value, Value, Value) {
    let mut store = TriggerStore::default();
    store.register_project("org-a", PROJECT_A, "revision-a");
    store.register_project("org-b", PROJECT_B, "revision-b");
    let mut source = ManualSource::default();
    source.start();
    for (org, project) in [
        ("org-a", PROJECT_A),
        ("org-a", PROJECT_A),
        ("org-b", PROJECT_B),
    ] {
        store
            .handle_manual_request(&mut source, &manual_body(org, project, "shared-delivery"))
            .expect("manual intake");
    }
    let receipt_of = |index: usize| source.handled()[index].receipt_id.clone();
    let run_input = |receipt: String, project: &str, revision: &str, name: &str| AcceptedRunInput {
        receipt_id: receipt,
        project_id: project.to_owned(),
        configuration_revision_id: revision.to_owned(),
        configured_trigger_name: name.to_owned(),
        step_ids: vec!["deploy-step".to_owned()],
        deadline_at_ms: 10_000,
        created_at_ms: 0,
    };
    let first_input = run_input(receipt_of(0), PROJECT_A, "revision-a", "deploy");
    let first_run = store.create_accepted_run(&first_input);
    let replay_run = store.create_accepted_run(&first_input);
    let step_run_id = store
        .step_run_id(&first_run.run_id, "deploy-step")
        .expect("step run")
        .to_owned();
    let stable_id = durable_execution_id(
        &first_run.run_id,
        "revision-a",
        "deploy",
        Some(&step_run_id),
    );

    let first_lease = store.claim_wakeup(1_000, 500).expect("first lease");
    let ExecutionReservation::Created(first_execution) = store
        .reserve_execution(&first_run.run_id, "deploy-step", 1_000, 10_000)
        .expect("reservation")
    else {
        panic!("expected created execution");
    };
    let blocked_before_expiry = store.claim_wakeup(1_499, 500).is_none();
    let recovery_lease = store.claim_wakeup(1_501, 500).expect("recovery lease");
    let ExecutionReservation::Existing(recovered_execution) = store
        .reserve_execution(&first_run.run_id, "deploy-step", 1_501, 10_000)
        .expect("recovered reservation")
    else {
        panic!("expected existing execution");
    };
    store.release_wakeup(&first_lease, 1_502);
    let stale_release_rejected = store.claim_wakeup(1_502, 500).is_none();
    store.release_wakeup(&recovery_lease, 1_502);
    let current_release_accepted = store.claim_wakeup(1_502, 500).is_some();

    let fan_out = store.create_accepted_run(&run_input(
        receipt_of(0),
        PROJECT_A,
        "revision-a",
        "rollback",
    ));
    let fan_out_replay = store.create_accepted_run(&run_input(
        receipt_of(0),
        PROJECT_A,
        "revision-a",
        "rollback",
    ));
    store.create_accepted_run(&run_input(receipt_of(2), PROJECT_B, "revision-b", "deploy"));

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

    let manual = json!({
        "firstCreated": first_run.created,
        "replayCreated": replay_run.created,
        "sameReceipt": receipt_of(1) == receipt_of(0),
        "crossOrgDistinct": receipt_of(2) != receipt_of(0),
        "receiptCount": store.receipt_count(),
        "runCount": store.run_count(),
        "fanOutDistinct": fan_out.run_id != first_run.run_id,
        "fanOutCreated": fan_out.created,
        "fanOutReplayCreated": fan_out_replay.created,
        "fanOutReplaySameRun": fan_out_replay.run_id == fan_out.run_id,
    });
    let lease = json!({
        "blockedBeforeExpiry": blocked_before_expiry,
        "recoveredAfterExpiry": recovery_lease.run_id == first_run.run_id,
        "leasedBeforeClaim": recovery_lease.leased_before_claim,
        "sameExecution": recovered_execution.id == first_execution.id,
        "executionIdIsDurable": first_execution.id == stable_id,
        "executionCount": store.execution_count(),
        "staleReleaseRejected": stale_release_rejected,
        "currentReleaseAccepted": current_release_accepted,
    });
    let execution = json!({
        "initial": first_execution.status.name(),
        "runningTransition": running.transitioned,
        "firstTerminal": succeeded.execution.status.name(),
        "conflictingTerminalTransition": conflicting.transitioned,
        "finalStatus": conflicting.execution.status.name(),
        "completedAtKept": conflicting.execution.completed_at_ms == succeeded.execution.completed_at_ms,
        "idleDeadlineSet": first_execution.idle_deadline_at_ms == Some(10_000),
        "idleDeadlineCleared": conflicting.execution.idle_deadline_at_ms.is_none(),
        "runStatus": store.run(&first_run.run_id).expect("run").status,
        "runSucceededTransition": run_succeeded.transitioned,
        "runSucceededAgainTransition": run_succeeded_again.transitioned,
    });
    (manual, lease, execution)
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

#[allow(clippy::too_many_lines)]
fn manual_request_trace() -> Value {
    let mut store = TriggerStore::default();
    store.register_project("org_1", PROJECT_A, "revision-1");
    store.register_project("org_2", PROJECT_B, "revision-2");
    let mut recording = ManualSource::default();
    recording.start();
    let mut idle = ManualSource::default();
    let mut cases = Map::new();
    let mut record =
        |store: &mut TriggerStore, name: &str, source: &mut ManualSource, body: &[u8]| {
            let outcome = match store.handle_manual_request(source, body) {
                Ok(response) => json!({"status": response.status, "body": response.body}),
                Err(error) => json!({"status": 0, "body": format!("threw: {error}")}),
            };
            cases.insert(name.to_owned(), outcome);
        };
    let a = |overrides: &[(&str, Value)]| delivery(PROJECT_A, "org_1", overrides);
    let uuid_nine = "7f1b0c1e-2d3a-9b5c-8d6e-9f0a1b2c3d4e";

    record(&mut store, "accepted", &mut recording, &a(&[]));
    let accepted_evidence: Vec<Value> = recording
        .handled()
        .iter()
        .map(|event| {
            json!({
                "connectionId": event.connection_id,
                "resourceId": event.resource_id,
                "source": event.source,
                "deliveryId": event.delivery_id,
            })
        })
        .collect();
    record(
        &mut store,
        "duplicateSameOrganization",
        &mut recording,
        &a(&[]),
    );
    let handled_after_duplicate = recording.handled().len();
    record(
        &mut store,
        "sameDeliveryOtherOrganization",
        &mut recording,
        &delivery(PROJECT_B, "org_2", &[]),
    );
    let handled_after_other = recording.handled().len();
    record(
        &mut store,
        "noHandlerDropped",
        &mut idle,
        &a(&[("deliveryId", json!("manual-idle"))]),
    );
    record(
        &mut store,
        "withConnectionEvidence",
        &mut recording,
        &a(&[
            ("deliveryId", json!("manual-evidence")),
            (
                "connectionId",
                json!("7f1b0c1e-2d3a-4b5c-8d6e-9f0a1b2c3d4e"),
            ),
            ("resourceId", json!("resource-1")),
        ]),
    );
    let last = recording.handled().last().expect("handled event");
    let connection_evidence =
        json!({"connectionId": last.connection_id, "resourceId": last.resource_id});
    let receipt_of = |store: &TriggerStore, org: &str, delivery: &str| {
        store.receipt(org, delivery).map_or(Value::Null, |receipt| {
            json!({
                "provider": receipt.provider,
                "source": receipt.source,
                "droppedReason": receipt.dropped_reason,
                "connectionId": receipt.connection_id,
                "resourceId": receipt.resource_id,
            })
        })
    };
    let receipts = json!({
        "accepted": receipt_of(&store, "org_1", "manual-1"),
        "idle": receipt_of(&store, "org_1", "manual-idle"),
        "otherOrganization": receipt_of(&store, "org_2", "manual-1"),
        "distinctReceiptIds": store.receipt("org_1", "manual-1").map(|r| &r.id)
            != store.receipt("org_2", "manual-1").map(|r| &r.id),
    });

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
        record(&mut store, name, &mut recording, body);
    }
    let mut bom = vec![0xef, 0xbb, 0xbf];
    bom.extend(a(&[("deliveryId", json!("manual-bom"))]));
    record(&mut store, "utf8BomBody", &mut recording, &bom);
    record(
        &mut store,
        "unknownProject",
        &mut recording,
        &a(&[
            ("deliveryId", json!("manual-unknown-project")),
            ("projectId", json!("7f1b0c1e-2d3a-4b5c-8d6e-9f0a1b2c3d4e")),
        ]),
    );
    record(&mut store, "invalidJson", &mut recording, b"{not json");
    record(&mut store, "arrayBody", &mut recording, b"[]");

    let mut grid = Map::new();
    for (index, received_at) in RECEIVED_AT_GRID.iter().enumerate() {
        let body = a(&[
            ("deliveryId", json!(format!("manual-grid-{index}"))),
            ("receivedAt", json!(received_at)),
        ]);
        let status = store
            .handle_manual_request(&mut recording, &body)
            .expect("grid intake")
            .status;
        grid.insert((*received_at).to_owned(), json!(status));
    }
    json!({
        "cases": cases,
        "receivedAtGrid": grid,
        "handledAfterDuplicate": handled_after_duplicate,
        "handledAfterOtherOrganization": handled_after_other,
        "receipts": receipts,
        "acceptedEvidence": accepted_evidence,
        "connectionEvidence": connection_evidence,
    })
}

fn manual_run_match_trace() -> Value {
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
                Ok(found) => json!({
                    "outcome": "matched",
                    "triggers": [found.trigger_name],
                    "deliveryId": found.delivery_id,
                    "configurationRevisionIsCurrent": found.configuration_revision_id == current,
                }),
                Err(rejection) => json!({"outcome": "rejected", "code": rejection.code()}),
            }
        };
    json!({
        "matched": run(current, "deploy", "anyone", None, None),
        "publicDeliveryKey": run(current, "deploy", "anyone", None, Some("public-key")),
        "expectedCurrent": run(current, "deploy", "anyone", Some(current), None),
        "expectedStale": run(current, "deploy", "anyone", Some(stale), None),
        "revisionMissing": run(stale, "deploy", "anyone", None, None),
        "triggerMissing": run(current, "absent", "anyone", None, None),
        "actorForbidden": run(current, "rollback", "mallory", None, None),
        "actorAllowed": run(current, "rollback", "alice", None, None),
        "noUserFilter": run(current, "open", "alice", None, None),
        "wrongEventTrigger": run(current, "cron", "alice", None, None),
    })
}

fn public_manual_run_trace() -> Value {
    let dispatched = json!({
        "deliveryKey": "delivery-1",
        "providerEventReceiptId": "845e9d26-7977-45e1-bc69-d80a7b55a9cc",
        "triggerRunId": "f83dc934-02a0-4849-8de7-699110be24ed",
        "configuredTriggerName": "deploy",
        "workflowStatus": "running",
    });
    let body = br#"{"projectSlug":"project","trigger":"deploy","actor":"alice","deliveryKey":"delivery-1","input":{}}"#;
    let call =
        |auth: AuthOutcome, result: &ManualRunResult, content_type: Option<&str>, body: &[u8]| {
            let response = public_manual_run(auth, content_type, body, result);
            json!({
                "status": response.status,
                "code": response.code,
                "contentType": response.content_type,
                "wwwAuthenticate": response.www_authenticate,
                "body": response.body,
            })
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
    json!({
        "results": {
            "dispatched": ok(&dispatched_result),
            "project_not_found": ok(&ManualRunResult::ProjectNotFound),
            "actor_forbidden": ok(&ManualRunResult::ActorForbidden),
            "daemon_offline": ok(&ManualRunResult::DaemonOffline),
            "expected_configuration_not_current": ok(&ManualRunResult::ExpectedConfigurationNotCurrent),
            "configuration_not_found": ok(&ManualRunResult::ConfigurationNotFound),
            "trigger_not_found": ok(&ManualRunResult::TriggerNotFound),
            "dispatch_conflict": ok(&ManualRunResult::DispatchConflict),
            "invalid_input": ok(&ManualRunResult::InvalidInput),
            "infrastructure_unavailable": ok(&ManualRunResult::InfrastructureUnavailable),
        },
        "unauthorized": call(AuthOutcome::Unauthorized, &dispatched_result, Some("application/json"), body),
        "forbidden": call(AuthOutcome::Forbidden, &dispatched_result, Some("application/json"), body),
        "authenticationUnavailable": call(AuthOutcome::Unavailable, &dispatched_result, Some("application/json"), body),
        "invalidJson": call(AuthOutcome::Authorized, &dispatched_result, Some("application/json"), b"{not json"),
        "wrongContentType": call(AuthOutcome::Authorized, &dispatched_result, Some("text/plain"), body),
        "missingContentType": call(AuthOutcome::Authorized, &dispatched_result, None, body),
    })
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

fn github_trace() -> Value {
    let mut cases = Map::new();
    for spec in webhook_cases() {
        let mut endpoint = GitHubWebhook::new(spec.secret.as_deref());
        for _ in 0..spec.handlers {
            endpoint.start_handler();
        }
        endpoint.set_events_per_acceptance(spec.events_per_acceptance);
        endpoint.set_accept_failure(spec.failure);
        let responses: Vec<Value> = spec
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
                json!({"status": response.status, "body": response.body})
            })
            .collect();
        let accepts: Vec<Value> = endpoint
            .accepts()
            .iter()
            .map(|call| {
                json!({
                    "source": call.source,
                    "dropReason": call.drop_reason,
                    "installationId": call.installation_id,
                    "repositoryId": call.repository_id,
                    "repo": call.repo,
                    "signatureHash": call.signature_hash,
                })
            })
            .collect();
        let lifecycles: Vec<Value> = endpoint
            .lifecycles()
            .iter()
            .map(|call| {
                json!({
                    "event": call.event,
                    "source": call.source,
                    "installationId": call.installation_id,
                    "signatureHash": call.signature_hash,
                })
            })
            .collect();
        cases.insert(
            spec.name.to_owned(),
            json!({
                "responses": responses,
                "accepts": accepts,
                "lifecycles": lifecycles,
                "dispatchCount": endpoint.dispatch_count(),
            }),
        );
    }
    json!({
        "signatureHashSample": hash_signature(&github_signature(GITHUB_SECRET, b"{}")),
        "cases": cases,
    })
}
