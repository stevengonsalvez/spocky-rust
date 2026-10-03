//! `ApiKeySummary.scopes` keeps the creation order of `[...new Set(scopes)]` in the baseline
//! (`src/auth/api-keys.ts`), not the declaration order of the scope enum.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use spocky_hub_pilot::{
    AccountId, ApiKeyScope, Bootstrap, EmbeddedFileStore, HubPilot, OrganizationId, PasswordChange,
};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        // A counter, not the clock: parallel tests must never share a directory.
        let path = std::env::temp_dir().join(format!(
            "spocky-hub-scope-order-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn created_and_listed_summaries_keep_the_order_the_scopes_were_requested_in() {
    let root = TestDir::new();
    let path = root.0.join("hub.json");
    let owner = AccountId::from("owner@example.test");
    let organization = OrganizationId::from("organization-a");
    let mut hub =
        HubPilot::open(EmbeddedFileStore::open(&path).expect("open store")).expect("open hub");
    hub.bootstrap(Bootstrap {
        instance_secret: "scope-order-runtime-secret-at-least-32-characters".into(),
        owner: owner.clone(),
        organization: organization.clone(),
        temporary_password: "temporary-password".into(),
    })
    .expect("bootstrap");
    hub.replace_password(&PasswordChange {
        account: owner.clone(),
        current_password: "temporary-password".into(),
        new_password: "replacement-password".into(),
    })
    .expect("replace password");

    let requested = [
        ApiKeyScope::RunsDispatch,
        ApiKeyScope::ProjectsRead,
        ApiKeyScope::RunsDispatch,
        ApiKeyScope::DaemonsEnroll,
    ];
    let created = hub
        .create_api_key(&owner, &organization, "ordered", requested)
        .expect("create key");
    let expected = vec![
        ApiKeyScope::RunsDispatch,
        ApiKeyScope::ProjectsRead,
        ApiKeyScope::DaemonsEnroll,
    ];
    assert_eq!(created.summary.scopes, expected);
    let listed = hub.list_api_keys(&owner, &organization).expect("list keys");
    assert_eq!(listed[0].scopes, expected);

    drop(hub);
    let reopened =
        HubPilot::open(EmbeddedFileStore::open(&path).expect("reopen store")).expect("reopen hub");
    let after_restart = reopened
        .list_api_keys(&owner, &organization)
        .expect("list keys after restart");
    assert_eq!(after_restart[0].scopes, expected);
}
