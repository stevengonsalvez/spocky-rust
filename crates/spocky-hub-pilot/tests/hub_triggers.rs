use std::collections::BTreeSet;

use spocky_hub_pilot::triggers::timezone::HostTimeZone;
use spocky_hub_pilot::triggers::{
    AcceptCall, AcceptFailure, Acceptance, AcceptedRunInput, AuthOutcome, ExecutionRecord,
    ExecutionRequest, ExecutionReservation, ExecutionStatus, GitHubWebhook, GitHubWebhookRequest,
    LifecycleCall, ManualEvent, ManualRunResult, TriggerStore, WebhookBackend,
    durable_execution_id, github_signature, public_manual_run,
};

const PROJECT: &str = "11111111-1111-4111-8111-111111111111";
const OTHER_PROJECT: &str = "22222222-2222-4222-8222-222222222222";

fn manual_body(org: &str, project: &str, delivery: &str) -> Vec<u8> {
    format!(
        r#"{{"organizationId":"{org}","projectId":"{project}","source":"manual.run","deliveryId":"{delivery}","payload":{{}}}}"#
    )
    .into_bytes()
}

fn intake(store: &mut TriggerStore, events: &mut Vec<ManualEvent>, body: &[u8]) -> u16 {
    let mut sink = |event: ManualEvent| events.push(event);
    store
        .handle_manual_request(Some(&mut sink), 1_000, body)
        .expect("manual intake")
        .status
}

fn run_input(receipt_id: &str, trigger: &str, steps: &[&str]) -> AcceptedRunInput {
    AcceptedRunInput {
        receipt_id: receipt_id.to_owned(),
        project_id: PROJECT.to_owned(),
        configuration_revision_id: "revision-a".to_owned(),
        configured_trigger_name: trigger.to_owned(),
        step_ids: steps.iter().map(|step| (*step).to_owned()).collect(),
        deadline_at_ms: 10_000,
        created_at_ms: 0,
        run_id: None,
        step_run_ids: None,
    }
}

fn request(step_id: &str, ordinal: usize, started_at_ms: u64) -> ExecutionRequest {
    ExecutionRequest {
        step_id: step_id.to_owned(),
        ordinal,
        started_at_ms,
        deadline_at_ms: 12_000,
        idle_deadline_at_ms: 20_000,
    }
}

fn store_with_run() -> (TriggerStore, String) {
    let mut store = TriggerStore::with_time_zone(HostTimeZone::utc());
    store.register_project("org-a", PROJECT, "revision-a");
    let mut events = Vec::new();
    intake(
        &mut store,
        &mut events,
        &manual_body("org-a", PROJECT, "delivery-1"),
    );
    let run = store.create_accepted_run(&run_input(
        &events[0].receipt_id,
        "deploy",
        &["deploy-step"],
    ));
    (store, run.run_id)
}

fn created(reservation: Option<ExecutionReservation>) -> ExecutionRecord {
    match reservation {
        Some(ExecutionReservation::Created(record)) => record,
        other => panic!("expected a new execution, got {other:?}"),
    }
}

#[test]
fn manual_delivery_is_idempotent_inside_one_organization() {
    let mut store = TriggerStore::with_time_zone(HostTimeZone::utc());
    store.register_project("org-a", PROJECT, "revision-a");
    store.register_project("org-b", OTHER_PROJECT, "revision-b");
    let mut events = Vec::new();
    for body in [
        manual_body("org-a", PROJECT, "same"),
        manual_body("org-a", PROJECT, "same"),
        manual_body("org-b", OTHER_PROJECT, "same"),
    ] {
        assert_eq!(intake(&mut store, &mut events, &body), 200);
    }

    assert_eq!(store.receipt_count(), 2);
    assert_eq!(events[0].receipt_id, events[1].receipt_id);
    assert_ne!(events[0].receipt_id, events[2].receipt_id);

    let receipt = &events[0].receipt_id;
    let first = store.create_accepted_run(&run_input(receipt, "deploy", &["s"]));
    let replay = store.create_accepted_run(&run_input(receipt, "deploy", &["s"]));
    let fan_out = store.create_accepted_run(&run_input(receipt, "rollback", &["s"]));
    assert!(first.created && !replay.created);
    assert_eq!(first.run_id, replay.run_id);
    assert_ne!(first.run_id, fan_out.run_id);
}

#[test]
fn manual_request_rejects_invalid_payloads_with_the_baseline_message() {
    let mut store = TriggerStore::with_time_zone(HostTimeZone::utc());
    let mut reject = |body: &str| {
        let response = store
            .handle_manual_request(None, 0, body.as_bytes())
            .expect("response");
        (response.status, response.body)
    };
    let (status, body) = reject(&format!(
        r#"{{"organizationId":"o","projectId":"{PROJECT}","source":"manual","deliveryId":"d","payload":{{}}}}"#
    ));
    assert_eq!(status, 400);
    assert_eq!(
        body,
        r#"{"error":"source must be provider-namespaced, for example github.issue_comment"}"#
    );
    assert_eq!(reject("{nope").0, 400);
}

#[test]
fn manual_request_decodes_like_the_baseline_request_body_reader() {
    let mut store = TriggerStore::with_time_zone(HostTimeZone::utc());
    store.register_project("org-a", PROJECT, "revision-a");
    let mut events = Vec::new();
    let with_boms = |count: usize, delivery: &str| {
        let mut body = [0xef, 0xbb, 0xbf].repeat(count);
        body.extend(manual_body("org-a", PROJECT, delivery));
        body
    };
    assert_eq!(intake(&mut store, &mut events, &with_boms(1, "one")), 200);
    assert_eq!(intake(&mut store, &mut events, &with_boms(2, "two")), 200);
    assert_eq!(intake(&mut store, &mut events, &with_boms(3, "three")), 400);
}

#[test]
fn received_at_keeps_its_value_and_local_offset() {
    let mut store = TriggerStore::with_time_zone(HostTimeZone::utc());
    store.register_project("org-a", PROJECT, "revision-a");
    store.set_local_offset_minutes(330);
    let mut events = Vec::new();
    let body = |received_at: &str, delivery: &str| {
        format!(
            r#"{{"organizationId":"org-a","projectId":"{PROJECT}","source":"manual.run","deliveryId":"{delivery}","receivedAt":"{received_at}","payload":null}}"#
        )
        .into_bytes()
    };
    intake(
        &mut store,
        &mut events,
        &body("2026-08-06T12:00:00Z", "utc"),
    );
    intake(
        &mut store,
        &mut events,
        &body("2026-08-06T12:00:00", "local"),
    );
    intake(&mut store, &mut events, &body("2026-08-06", "date-only"));
    assert_eq!(events[0].received_at_ms, 1_786_017_600_000);
    assert_eq!(events[1].received_at_ms, 1_786_017_600_000 - 330 * 60_000);
    assert_eq!(events[2].received_at_ms, 1_785_974_400_000);
}

#[test]
fn expired_lease_recovers_the_pre_handoff_execution() {
    let (mut store, run_id) = store_with_run();
    let first_lease = store.claim_wakeup(1_000, 500).expect("initial lease");
    let first = created(store.reserve_execution(&run_id, &request("deploy-step", 0, 1_000)));
    assert_eq!(first.status, ExecutionStatus::Spawning);
    assert!(store.claim_wakeup(1_499, 500).is_none());

    let recovery = store.claim_wakeup(1_501, 500).expect("recovery lease");
    assert!(recovery.leased_before_claim);
    let Some(ExecutionReservation::Existing(recovered)) =
        store.reserve_execution(&run_id, &request("deploy-step", 0, 1_501))
    else {
        panic!("expected the reserved execution");
    };
    assert_eq!(recovered.id, first.id);
    assert_eq!(store.execution_count(), 1);

    store.release_wakeup(&first_lease, 1_502);
    assert!(
        store.claim_wakeup(1_502, 500).is_none(),
        "stale release must not free the lease"
    );
    store.release_wakeup(&recovery, 1_502);
    assert!(store.claim_wakeup(1_502, 500).is_some());
}

#[test]
fn lease_is_claimable_exactly_at_its_expiry() {
    let (mut store, _) = store_with_run();
    store.claim_wakeup(1_000, 500).expect("initial lease");
    assert!(store.claim_wakeup(1_499, 500).is_none());
    let at_expiry = store
        .claim_wakeup(1_500, 500)
        .expect("claimable at the expiry instant");
    assert!(at_expiry.leased_before_claim);
    assert_eq!(at_expiry.lease_expires_at_ms, 2_000);
}

#[test]
fn equal_availability_is_claimed_in_creation_order() {
    let mut store = TriggerStore::with_time_zone(HostTimeZone::utc());
    store.register_project("org-a", PROJECT, "revision-a");
    let mut events = Vec::new();
    intake(
        &mut store,
        &mut events,
        &manual_body("org-a", PROJECT, "fifo"),
    );
    let receipt = events[0].receipt_id.clone();
    let runs: Vec<String> = ["first", "second", "third"]
        .iter()
        .map(|name| {
            store
                .create_accepted_run(&run_input(&receipt, name, &["s"]))
                .run_id
        })
        .collect();
    let claimed: Vec<String> = (0..3)
        .map(|_| store.claim_wakeup(5, 1_000).expect("lease").run_id)
        .collect();
    assert_eq!(claimed, runs);
}

#[test]
fn step_is_selected_by_step_id_and_ordinal() {
    let mut store = TriggerStore::with_time_zone(HostTimeZone::utc());
    store.register_project("org-a", PROJECT, "revision-a");
    let mut events = Vec::new();
    intake(
        &mut store,
        &mut events,
        &manual_body("org-a", PROJECT, "steps"),
    );
    let run = store.create_accepted_run(&run_input(&events[0].receipt_id, "deploy", &["s", "s"]));
    let second = created(store.reserve_execution(&run.run_id, &request("s", 1, 100)));
    let first = created(store.reserve_execution(&run.run_id, &request("s", 0, 100)));
    assert_ne!(first.step_run_id, second.step_run_id);
    assert_eq!(
        Some(second.step_run_id.as_str()),
        store.step_run_id(&run.run_id, "s", 1)
    );
    assert!(
        store
            .reserve_execution(&run.run_id, &request("s", 2, 100))
            .is_none()
    );
    assert!(
        store
            .reserve_execution(&run.run_id, &request("other", 0, 100))
            .is_none()
    );
}

#[test]
fn idle_deadline_is_capped_by_the_execution_and_run_deadlines() {
    let (mut store, run_id) = store_with_run();
    let execution = created(store.reserve_execution(&run_id, &request("deploy-step", 0, 1_000)));
    // idle 20_000, execution deadline 12_000, run deadline 10_000
    assert_eq!(execution.idle_deadline_at_ms, Some(10_000));

    let mut tight = request("deploy-step", 0, 1_000);
    tight.deadline_at_ms = 5_000;
    tight.idle_deadline_at_ms = 9_000;
    let (mut other, other_run) = store_with_run();
    let capped = created(other.reserve_execution(&other_run, &tight));
    assert_eq!(capped.idle_deadline_at_ms, Some(5_000));
}

#[test]
fn execution_id_is_derived_from_run_revision_trigger_and_step() {
    assert_eq!(
        durable_execution_id("run-1", "revision-1", "deploy", Some("step-run-1")),
        "a4071e2c-c3ec-5c7a-9c38-8f19dabbf847"
    );
    assert_eq!(
        durable_execution_id("run-1", "revision-1", "deploy", None),
        "94b9d672-46a1-5ebf-93a1-87a5ad462f78"
    );
    let (mut store, run_id) = store_with_run();
    let execution = created(store.reserve_execution(&run_id, &request("deploy-step", 0, 1_000)));
    let step_run_id = store
        .step_run_id(&run_id, "deploy-step", 0)
        .expect("step run");
    assert_eq!(
        execution.id,
        durable_execution_id(&run_id, "revision-a", "deploy", Some(step_run_id))
    );
}

#[test]
fn first_terminal_execution_transition_wins() {
    let (mut store, run_id) = store_with_run();
    store.claim_wakeup(2_000, 500).expect("lease");
    let execution = created(store.reserve_execution(&run_id, &request("deploy-step", 0, 2_000)));
    let succeeded = store
        .transition_execution(&execution.id, ExecutionStatus::Succeeded, 2_020)
        .expect("terminal");
    let conflicting = store
        .transition_execution(&execution.id, ExecutionStatus::Failed, 2_030)
        .expect("no-op");
    assert!(succeeded.transitioned && !conflicting.transitioned);
    assert_eq!(conflicting.execution.status, ExecutionStatus::Succeeded);
    assert_eq!(conflicting.execution.completed_at_ms, Some(2_020));
    assert_eq!(conflicting.execution.idle_deadline_at_ms, None);
    assert!(store.succeed_run(&run_id).expect("run").transitioned);
    assert!(!store.succeed_run(&run_id).expect("run").transitioned);
}

/// Minimal acceptance boundary: first delivery accepted, repeats duplicate, optional failure.
#[derive(Default)]
struct Boundary {
    seen: BTreeSet<String>,
    dispatched: usize,
    failure: Option<AcceptFailure>,
}

impl WebhookBackend for Boundary {
    fn accept(&mut self, call: &AcceptCall) -> Result<Acceptance, AcceptFailure> {
        if let Some(failure) = self.failure {
            return Err(failure);
        }
        if !self.seen.insert(call.delivery_id.clone()) {
            return Ok(Acceptance::Duplicate);
        }
        Ok(if call.drop_reason.is_some() {
            Acceptance::Dropped
        } else {
            Acceptance::Accepted { events: 1 }
        })
    }

    fn apply_lifecycle(&mut self, _call: &LifecycleCall) {}

    fn dispatch(&mut self, events: usize, handlers: usize) {
        self.dispatched += events * handlers;
    }
}

fn webhook_status(
    endpoint: &mut GitHubWebhook<Boundary>,
    secret: &str,
    body: &[u8],
    signed: bool,
) -> u16 {
    let signature = github_signature(secret, body);
    endpoint
        .handle(GitHubWebhookRequest {
            delivery_id: Some("delivery-1"),
            event_type: Some("issue_comment"),
            signature: signed.then_some(signature.as_str()),
            body,
        })
        .status
}

#[test]
fn github_webhook_verifies_exact_body_and_suppresses_replay() {
    let secret = "github-secret";
    let body = br#"{"action":"created","installation":{"id":42},"repository":{"id":9001,"full_name":"acme/widgets"}}"#;
    let mut endpoint = GitHubWebhook::new(Some(secret), Boundary::default());
    endpoint.start_handler();

    assert_eq!(webhook_status(&mut endpoint, secret, body, true), 200);
    assert_eq!(webhook_status(&mut endpoint, secret, body, true), 200);
    assert_eq!(endpoint.backend().dispatched, 1);

    let signature = github_signature(secret, body);
    let tampered = endpoint.handle(GitHubWebhookRequest {
        delivery_id: Some("delivery-2"),
        event_type: Some("issue_comment"),
        signature: Some(&signature),
        body: br#"{"action":"closed","installation":{"id":42}}"#,
    });
    assert_eq!(tampered.status, 401);
    assert_eq!(webhook_status(&mut endpoint, secret, body, false), 401);
    assert_eq!(endpoint.backend().dispatched, 1);
}

#[test]
fn github_webhook_refuses_unsafe_requests() {
    let valid = br#"{"installation":{"id":42},"repository":{"id":1,"full_name":"a/b"}}"#;
    let mut unconfigured = GitHubWebhook::new(None, Boundary::default());
    assert_eq!(webhook_status(&mut unconfigured, "any", valid, true), 503);

    let mut endpoint = GitHubWebhook::new(Some("s"), Boundary::default());
    endpoint.start_handler();
    let oversized = vec![b' '; 1_048_577];
    assert_eq!(webhook_status(&mut endpoint, "s", &oversized, true), 413);
    assert_eq!(webhook_status(&mut endpoint, "s", b"{not json", true), 400);
    assert_eq!(
        webhook_status(&mut endpoint, "s", br#"{"repository":{}}"#, true),
        400
    );

    endpoint.backend_mut().failure = Some(AcceptFailure::DatabaseUnavailable);
    assert_eq!(webhook_status(&mut endpoint, "s", valid, true), 503);
    assert_eq!(endpoint.backend().dispatched, 0);
}

#[test]
fn public_manual_run_maps_a_stale_expected_version_to_409() {
    let body = br#"{"trigger":"deploy"}"#;
    let response = public_manual_run(
        AuthOutcome::Authorized,
        "request-1",
        Some("application/json"),
        body,
        &ManualRunResult::ExpectedConfigurationNotCurrent,
    );
    assert_eq!(response.status, 409);
    assert_eq!(
        response.body,
        r#"{"type":"https://paseo.sh/problems/configuration-changed","title":"Configuration changed","status":409,"detail":"expectedVersionId is not the configuration version selected for this delivery.","code":"configuration_changed","requestId":"request-1"}"#
    );
    let denied = public_manual_run(
        AuthOutcome::Unauthorized,
        "request-1",
        Some("application/json"),
        body,
        &ManualRunResult::DaemonOffline,
    );
    assert_eq!(
        (denied.status, denied.www_authenticate),
        (401, Some("Bearer"))
    );
    let bom_body = [&[0xef, 0xbb, 0xbf][..], body].concat();
    let accepted = public_manual_run(
        AuthOutcome::Authorized,
        "request-1",
        Some("application/json"),
        &bom_body,
        &ManualRunResult::DaemonOffline,
    );
    assert_eq!(
        accepted.status, 409,
        "a byte order mark must not make the body invalid JSON"
    );
}

/// `receivedAt` as the store resolves it for a date-time written without an offset.
fn local_received_at_ms(zone: &HostTimeZone, received_at: &str) -> i64 {
    let mut store = TriggerStore::with_time_zone(zone.clone());
    store.register_project("org-a", PROJECT, "revision-a");
    let mut events = Vec::new();
    let body = format!(
        r#"{{"organizationId":"org-a","projectId":"{PROJECT}","source":"manual.run","deliveryId":"local","receivedAt":"{received_at}","payload":null}}"#
    );
    assert_eq!(intake(&mut store, &mut events, body.as_bytes()), 200);
    events[0].received_at_ms
}

fn named(zone: &str) -> HostTimeZone {
    HostTimeZone::named(zone).unwrap_or_else(|| panic!("zoneinfo for {zone}"))
}

#[test]
fn local_date_times_follow_the_daylight_saving_rules_of_the_host_zone() {
    let london = named("Europe/London");
    // January is GMT (offset 0), August is BST (offset 60).
    assert_eq!(
        local_received_at_ms(&london, "2026-01-15T12:00:00"),
        1_768_478_400_000
    );
    assert_eq!(
        local_received_at_ms(&london, "2026-08-06T12:00:00"),
        1_786_014_000_000
    );
    // Spring forward 2026-03-29 01:00 GMT: 01:30 local does not exist and uses the offset before the
    // transition; 02:00 local is the first BST instant.
    assert_eq!(
        local_received_at_ms(&london, "2026-03-29T00:59:59"),
        1_774_745_999_000
    );
    assert_eq!(
        local_received_at_ms(&london, "2026-03-29T01:30:00"),
        1_774_747_800_000
    );
    assert_eq!(
        local_received_at_ms(&london, "2026-03-29T02:00:00"),
        1_774_746_000_000
    );
    // Fall back 2026-10-25 01:00 UTC: 01:30 local happens twice and resolves to the first, BST,
    // instant; 02:00 local is GMT again.
    assert_eq!(
        local_received_at_ms(&london, "2026-10-25T01:30:00"),
        1_792_888_200_000
    );
    assert_eq!(
        local_received_at_ms(&london, "2026-10-25T02:00:00"),
        1_792_893_600_000
    );
    // An explicit offset or Z never consults the host zone.
    assert_eq!(
        local_received_at_ms(&london, "2026-08-06T12:00:00Z"),
        1_786_017_600_000
    );
}

#[test]
fn host_zones_read_from_zoneinfo_apply_their_own_offsets() {
    let noon = |zone: &str, received_at: &str| local_received_at_ms(&named(zone), received_at);
    let utc_noon_january = 1_768_478_400_000_i64;
    let utc_noon_july = 1_783_425_600_000_i64;
    assert_eq!(
        noon("Asia/Kolkata", "2026-01-15T12:00:00"),
        utc_noon_january - 330 * 60_000
    );
    // Southern hemisphere: daylight time spans the new year.
    assert_eq!(
        noon("Australia/Sydney", "2026-01-15T12:00:00"),
        utc_noon_january - 660 * 60_000
    );
    assert_eq!(
        noon("Australia/Sydney", "2026-07-07T12:00:00"),
        utc_noon_july - 600 * 60_000
    );
    assert_eq!(
        noon("America/New_York", "2026-01-15T12:00:00"),
        utc_noon_january + 300 * 60_000
    );
    assert_eq!(
        noon("America/New_York", "2026-07-07T12:00:00"),
        utc_noon_july + 240 * 60_000
    );
    assert!(HostTimeZone::named("Not/AZone").is_none());
    assert!(HostTimeZone::named("../etc/passwd").is_none());
    assert!(HostTimeZone::named("").is_none());
}

#[test]
fn a_default_store_reads_the_host_zone_when_it_is_built() {
    let default = TriggerStore::default();
    let host = TriggerStore::with_time_zone(HostTimeZone::from_env());
    for local_ms in [1_768_435_200_000_i64, 1_785_974_400_000, 1_774_747_800_000] {
        assert_eq!(
            default.local_offset_minutes_at(local_ms),
            host.local_offset_minutes_at(local_ms)
        );
    }
    let mut pinned = TriggerStore::with_time_zone(named("Europe/London"));
    assert_eq!(pinned.local_offset_minutes_at(1_768_435_200_000), 0);
    assert_eq!(pinned.local_offset_minutes_at(1_785_974_400_000), 60);
    pinned.set_local_offset_minutes(330);
    assert_eq!(pinned.local_offset_minutes_at(1_785_974_400_000), 330);
}

#[test]
fn caller_chosen_run_and_step_run_ids_are_kept() {
    let mut store = TriggerStore::with_time_zone(HostTimeZone::utc());
    store.register_project("org-a", PROJECT, "revision-a");
    let mut events = Vec::new();
    intake(
        &mut store,
        &mut events,
        &manual_body("org-a", PROJECT, "delivery-1"),
    );
    let mut input = run_input(&events[0].receipt_id, "deploy", &["first", "second"]);
    input.run_id = Some("fixed-run".to_owned());
    input.step_run_ids = Some(vec!["fixed-step-0".to_owned(), "fixed-step-1".to_owned()]);
    let run = store.create_accepted_run(&input);
    assert_eq!(run.run_id, "fixed-run");
    assert_eq!(
        store.step_run_id("fixed-run", "first", 0),
        Some("fixed-step-0")
    );
    assert_eq!(
        store.step_run_id("fixed-run", "second", 1),
        Some("fixed-step-1")
    );
    let execution = created(store.reserve_execution("fixed-run", &request("second", 1, 1_000)));
    assert_eq!(execution.step_run_id, "fixed-step-1");
}

#[test]
fn idle_deadline_follows_an_execution_deadline_before_the_run_deadline() {
    let (mut store, run_id) = store_with_run();
    let mut earliest = request("deploy-step", 0, 1_000);
    // execution 5_000 < run 10_000 < idle 20_000
    earliest.deadline_at_ms = 5_000;
    earliest.idle_deadline_at_ms = 20_000;
    let execution = created(store.reserve_execution(&run_id, &earliest));
    assert_eq!(execution.deadline_at_ms, 5_000);
    assert_eq!(execution.idle_deadline_at_ms, Some(5_000));

    // run 10_000 < execution 12_000 < idle 20_000: the run deadline wins for both.
    let (mut other, other_run) = store_with_run();
    let capped = created(other.reserve_execution(&other_run, &request("deploy-step", 0, 1_000)));
    assert_eq!(capped.deadline_at_ms, 10_000);
    assert_eq!(capped.idle_deadline_at_ms, Some(10_000));
}

/// Expected values come from `new Date(text).getTime()` in Node (v26.7.0) run with `TZ` set to the
/// zone: gaps use the offset before the transition, overlaps the first instant.
#[test]
fn local_date_times_match_node_in_historic_gap_overlap_and_year_wrap_cases() {
    let cases: [(&str, &str, i64); 17] = [
        // Lord Howe changes by 30 minutes.
        (
            "Australia/Lord_Howe",
            "2026-10-04T01:59:59",
            1_791_041_399_000,
        ),
        (
            "Australia/Lord_Howe",
            "2026-10-04T02:15:00",
            1_791_042_300_000,
        ),
        (
            "Australia/Lord_Howe",
            "2026-10-04T02:30:00",
            1_791_041_400_000,
        ),
        (
            "Australia/Lord_Howe",
            "2026-04-05T01:45:00",
            1_775_313_900_000,
        ),
        (
            "Australia/Lord_Howe",
            "2026-04-05T02:15:00",
            1_775_317_500_000,
        ),
        // Local mean time before standard time, with seconds in the offset (-3:06:28).
        (
            "America/Sao_Paulo",
            "1800-01-01T00:00:00",
            -5_364_651_212_000,
        ),
        (
            "America/Sao_Paulo",
            "1900-01-01T00:00:00",
            -2_208_977_612_000,
        ),
        (
            "America/Sao_Paulo",
            "2018-11-04T00:30:00",
            1_541_302_200_000,
        ),
        (
            "America/Sao_Paulo",
            "2018-02-17T23:30:00",
            1_518_917_400_000,
        ),
        // Southern hemisphere: daylight time wraps the new year.
        ("Australia/Sydney", "2026-01-01T00:00:00", 1_767_186_000_000),
        ("Australia/Sydney", "2026-12-31T23:59:59", 1_798_721_999_000),
        ("Australia/Sydney", "2026-04-05T02:30:00", 1_775_316_600_000),
        ("Australia/Sydney", "2026-10-04T02:30:00", 1_791_045_000_000),
        ("Europe/London", "1800-01-01T00:00:00", -5_364_662_325_000),
        ("America/New_York", "2026-03-08T02:30:00", 1_772_955_000_000),
        ("America/New_York", "2026-11-01T01:30:00", 1_793_511_000_000),
        ("Pacific/Apia", "2011-12-30T12:00:00", 1_325_282_400_000),
    ];
    for (zone, text, expected) in cases {
        assert_eq!(
            local_received_at_ms(&named(zone), text),
            expected,
            "{zone} {text}"
        );
    }
}

#[test]
fn tz_values_follow_the_node_rules() {
    // `TZ=":Europe/London"` reads the same zone as `TZ=Europe/London`.
    let colon = HostTimeZone::from_tz_value(":Europe/London").expect("colon form");
    assert_eq!(colon, named("Europe/London"));
    assert_eq!(
        local_received_at_ms(&colon, "2026-08-06T12:00:00"),
        1_786_014_000_000
    );
    assert!(HostTimeZone::from_tz_value("Nope/Zone").is_none());
    assert!(HostTimeZone::from_tz_value(":").is_none());
}

#[test]
fn tzif_files_without_64_bit_data_are_not_read() {
    // TZif version 1: a header, one local time type and one abbreviation character.
    let mut version_one = b"TZif\0".to_vec();
    version_one.extend([0_u8; 15]);
    for count in [0_u32, 0, 0, 0, 1, 1] {
        version_one.extend(count.to_be_bytes());
    }
    version_one.extend([0, 0, 0, 0, 0, 0, 0]);
    assert!(HostTimeZone::from_tzif(&version_one).is_none());
    assert!(HostTimeZone::from_tzif(b"TZif2").is_none());
    assert!(HostTimeZone::from_tzif(b"").is_none());
    assert!(HostTimeZone::from_tzif(&real_london_truncated()).is_none());
}

fn real_london_truncated() -> Vec<u8> {
    let mut bytes = std::fs::read("/usr/share/zoneinfo/Europe/London").expect("London zoneinfo");
    bytes.truncate(100);
    bytes
}
