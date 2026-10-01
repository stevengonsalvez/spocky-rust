use spocky_hub_pilot::triggers::{
    AcceptFailure, AcceptedRunInput, AuthOutcome, ExecutionReservation, ExecutionStatus,
    GitHubWebhook, GitHubWebhookRequest, ManualRunResult, ManualSource, TriggerStore,
    durable_execution_id, github_signature, public_manual_run,
};

const PROJECT: &str = "11111111-1111-4111-8111-111111111111";

fn manual_body(org: &str, delivery: &str) -> Vec<u8> {
    format!(
        r#"{{"organizationId":"{org}","projectId":"{PROJECT}","source":"manual.run","deliveryId":"{delivery}","payload":{{}}}}"#
    )
    .into_bytes()
}

fn run_input(receipt_id: &str, trigger: &str) -> AcceptedRunInput {
    AcceptedRunInput {
        receipt_id: receipt_id.to_owned(),
        project_id: PROJECT.to_owned(),
        configuration_revision_id: "revision-a".to_owned(),
        configured_trigger_name: trigger.to_owned(),
        step_ids: vec!["deploy-step".to_owned()],
        deadline_at_ms: 10_000,
        created_at_ms: 0,
    }
}

fn store_with_run() -> (TriggerStore, String) {
    let mut store = TriggerStore::default();
    store.register_project("org-a", PROJECT, "revision-a");
    let mut source = ManualSource::default();
    source.start();
    store
        .handle_manual_request(&mut source, &manual_body("org-a", "delivery-1"))
        .expect("intake");
    let receipt = source.handled()[0].receipt_id.clone();
    let run = store.create_accepted_run(&run_input(&receipt, "deploy"));
    (store, run.run_id)
}

#[test]
fn manual_delivery_is_idempotent_inside_one_organization() {
    let mut store = TriggerStore::default();
    store.register_project("org-a", PROJECT, "revision-a");
    store.register_project(
        "org-b",
        "22222222-2222-4222-8222-222222222222",
        "revision-b",
    );
    let mut source = ManualSource::default();
    source.start();
    for body in [manual_body("org-a", "same"), manual_body("org-a", "same")] {
        assert_eq!(
            store
                .handle_manual_request(&mut source, &body)
                .expect("intake")
                .status,
            200
        );
    }
    let other = br#"{"organizationId":"org-b","projectId":"22222222-2222-4222-8222-222222222222","source":"manual.run","deliveryId":"same","payload":{}}"#;
    store
        .handle_manual_request(&mut source, other)
        .expect("intake");

    assert_eq!(store.receipt_count(), 2);
    let events = source.handled();
    assert_eq!(events[0].receipt_id, events[1].receipt_id);
    assert_ne!(events[0].receipt_id, events[2].receipt_id);

    let first = store.create_accepted_run(&run_input(&events[0].receipt_id, "deploy"));
    let replay = store.create_accepted_run(&run_input(&events[0].receipt_id, "deploy"));
    let fan_out = store.create_accepted_run(&run_input(&events[0].receipt_id, "rollback"));
    assert!(first.created && !replay.created);
    assert_eq!(first.run_id, replay.run_id);
    assert_ne!(first.run_id, fan_out.run_id);
}

#[test]
fn manual_request_rejects_invalid_payloads_with_the_baseline_message() {
    let mut store = TriggerStore::default();
    let mut source = ManualSource::default();
    let reject = |store: &mut TriggerStore, source: &mut ManualSource, body: &str| {
        let response = store
            .handle_manual_request(source, body.as_bytes())
            .expect("response");
        (response.status, response.body)
    };
    let (status, body) = reject(
        &mut store,
        &mut source,
        &format!(
            r#"{{"organizationId":"o","projectId":"{PROJECT}","source":"manual","deliveryId":"d","payload":{{}}}}"#
        ),
    );
    assert_eq!(status, 400);
    assert_eq!(
        body,
        r#"{"error":"source must be provider-namespaced, for example github.issue_comment"}"#
    );
    assert_eq!(reject(&mut store, &mut source, "{nope").0, 400);
}

#[test]
fn expired_lease_recovers_the_pre_handoff_execution() {
    let (mut store, run_id) = store_with_run();
    let first_lease = store.claim_wakeup(1_000, 500).expect("initial lease");
    let ExecutionReservation::Created(first) = store
        .reserve_execution(&run_id, "deploy-step", 1_000, 10_000)
        .expect("reservation")
    else {
        panic!("expected a new execution");
    };
    assert_eq!(first.status, ExecutionStatus::Spawning);
    assert!(store.claim_wakeup(1_499, 500).is_none());

    let recovery = store.claim_wakeup(1_501, 500).expect("recovery lease");
    assert!(recovery.leased_before_claim);
    let ExecutionReservation::Existing(recovered) = store
        .reserve_execution(&run_id, "deploy-step", 1_501, 10_000)
        .expect("recovered reservation")
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
fn execution_id_is_derived_from_run_revision_trigger_and_step() {
    assert_eq!(
        durable_execution_id("run-1", "revision-1", "deploy", Some("step-run-1")),
        "a4071e2c-c3ec-5c7a-9c38-8f19dabbf847"
    );
    assert_eq!(
        durable_execution_id("run-1", "revision-1", "deploy", None),
        "94b9d672-46a1-5ebf-93a1-87a5ad462f78"
    );
}

#[test]
fn first_terminal_execution_transition_wins() {
    let (mut store, run_id) = store_with_run();
    store.claim_wakeup(2_000, 500).expect("lease");
    let ExecutionReservation::Created(execution) = store
        .reserve_execution(&run_id, "deploy-step", 2_000, 10_000)
        .expect("reservation")
    else {
        panic!("expected a new execution");
    };
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

fn webhook_status(endpoint: &mut GitHubWebhook, secret: &str, body: &[u8], signed: bool) -> u16 {
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
    let mut endpoint = GitHubWebhook::new(Some(secret));
    endpoint.start_handler();

    assert_eq!(webhook_status(&mut endpoint, secret, body, true), 200);
    assert_eq!(webhook_status(&mut endpoint, secret, body, true), 200);
    assert_eq!(endpoint.dispatch_count(), 1);

    let signature = github_signature(secret, body);
    let tampered = endpoint.handle(GitHubWebhookRequest {
        delivery_id: Some("delivery-2"),
        event_type: Some("issue_comment"),
        signature: Some(&signature),
        body: br#"{"action":"closed","installation":{"id":42}}"#,
    });
    assert_eq!(tampered.status, 401);
    assert_eq!(webhook_status(&mut endpoint, secret, body, false), 401);
    assert_eq!(endpoint.dispatch_count(), 1);
}

#[test]
fn github_webhook_refuses_unsafe_requests() {
    let valid = br#"{"installation":{"id":42},"repository":{"id":1,"full_name":"a/b"}}"#;
    let mut unconfigured = GitHubWebhook::new(None);
    assert_eq!(webhook_status(&mut unconfigured, "any", valid, true), 503);

    let mut endpoint = GitHubWebhook::new(Some("s"));
    endpoint.start_handler();
    let oversized = vec![b' '; 1_048_577];
    assert_eq!(webhook_status(&mut endpoint, "s", &oversized, true), 413);
    assert_eq!(webhook_status(&mut endpoint, "s", b"{not json", true), 400);
    assert_eq!(
        webhook_status(&mut endpoint, "s", br#"{"repository":{}}"#, true),
        400
    );

    endpoint.set_accept_failure(Some(AcceptFailure::DatabaseUnavailable));
    assert_eq!(webhook_status(&mut endpoint, "s", valid, true), 503);
    assert_eq!(endpoint.dispatch_count(), 0);
}

#[test]
fn public_manual_run_maps_a_stale_expected_version_to_409() {
    let body = br#"{"trigger":"deploy"}"#;
    let response = public_manual_run(
        AuthOutcome::Authorized,
        Some("application/json"),
        body,
        &ManualRunResult::ExpectedConfigurationNotCurrent,
    );
    assert_eq!(
        (response.status, response.code),
        (409, Some("configuration_changed"))
    );
    let denied = public_manual_run(
        AuthOutcome::Unauthorized,
        Some("application/json"),
        body,
        &ManualRunResult::DaemonOffline,
    );
    assert_eq!(
        (denied.status, denied.www_authenticate),
        (401, Some("Bearer"))
    );
}
