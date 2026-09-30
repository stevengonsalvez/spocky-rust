use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use paseo_hub_pilot::{
    AccountId, AuthorityError, Bootstrap, BrowserAccountStatus, EmbeddedFileStore, HubError,
    HubPilot, OrganizationId, PasswordChange,
};

struct TestDir(PathBuf);

static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "paseo-hub-session-selection-{}-{nonce}-{}",
            std::process::id(),
            TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        match fs::remove_dir_all(&self.0) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove test directory: {error}"),
        }
    }
}

#[test]
fn active_organization_selection_validates_membership_and_survives_restart() {
    let root = TestDir::new();
    let path = root.0.join("hub.json");
    let owner = AccountId::from("owner@example.test");
    let first = OrganizationId::from("organization-1");
    let second = OrganizationId::from("organization-2");
    let mut hub = HubPilot::open(EmbeddedFileStore::open(&path).unwrap()).unwrap();
    hub.bootstrap(Bootstrap {
        instance_secret: "session-selection-secret-at-least-32-characters".into(),
        owner: owner.clone(),
        organization: first.clone(),
        temporary_password: "temporary-password".into(),
    })
    .unwrap();
    hub.replace_password(&PasswordChange {
        account: owner.clone(),
        current_password: "temporary-password".into(),
        new_password: "replacement-password".into(),
    })
    .unwrap();
    let session = hub.sign_in(&owner, "replacement-password").unwrap();
    hub.complete_app_setup(&session).unwrap();
    assert_eq!(
        hub.active_organization_for_session(&session),
        Some(first.clone())
    );
    assert_eq!(
        hub.browser_account_status(Some(&session)),
        BrowserAccountStatus::Active
    );

    hub.create_organization_for_session(&session, second.clone())
        .unwrap();
    assert_eq!(hub.active_organization_for_session(&session), Some(second));
    hub.select_organization(&session, &first).unwrap();
    assert_eq!(
        hub.active_organization_for_session(&session),
        Some(first.clone())
    );
    assert_eq!(
        hub.select_organization(&session, &OrganizationId::from("foreign")),
        Err(HubError::Authority(AuthorityError::OrganizationUnavailable))
    );

    drop(hub);
    let restarted = HubPilot::open(EmbeddedFileStore::open(path).unwrap()).unwrap();
    assert_eq!(
        restarted.active_organization_for_session(&session),
        Some(first)
    );
    assert_eq!(
        restarted.browser_account_status(Some(&session)),
        BrowserAccountStatus::Active
    );
}

#[test]
fn old_single_membership_session_defaults_to_bootstrap_organization() {
    let root = TestDir::new();
    let path = root.0.join("hub.json");
    let owner = AccountId::from("owner@example.test");
    let organization = OrganizationId::from("organization-1");
    let mut hub = HubPilot::open(EmbeddedFileStore::open(&path).unwrap()).unwrap();
    hub.bootstrap(Bootstrap {
        instance_secret: "session-selection-secret-at-least-32-characters".into(),
        owner: owner.clone(),
        organization: organization.clone(),
        temporary_password: "temporary-password".into(),
    })
    .unwrap();
    hub.replace_password(&PasswordChange {
        account: owner.clone(),
        current_password: "temporary-password".into(),
        new_password: "replacement-password".into(),
    })
    .unwrap();
    let session = hub.sign_in(&owner, "replacement-password").unwrap();
    hub.complete_app_setup(&session).unwrap();
    drop(hub);

    let bytes = fs::read(&path).unwrap();
    let mut state: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    state
        .as_object_mut()
        .unwrap()
        .remove("active_browser_organizations");
    fs::write(&path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let restarted = HubPilot::open(EmbeddedFileStore::open(path).unwrap()).unwrap();
    assert_eq!(
        restarted.active_organization_for_session(&session),
        Some(organization)
    );
    assert_eq!(
        restarted.browser_account_status(Some(&session)),
        BrowserAccountStatus::Active
    );
}
