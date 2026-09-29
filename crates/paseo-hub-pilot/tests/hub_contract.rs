use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use paseo_hub_pilot::{
    AccountId, AuthorityError, Bootstrap, DaemonId, DaemonPermission, EmbeddedFileStore, HubPilot,
    OrganizationId, PasswordChange, RegistrationRequest, Role, SessionError, StoreSemantics,
};

struct TestDir(PathBuf);

static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "paseo-hub-pilot-{}-{nonce}-{}",
            std::process::id(),
            TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove test directory");
    }
}

fn bootstrap() -> Bootstrap {
    Bootstrap {
        instance_secret: "first-run-secret-at-least-32-characters".into(),
        owner: AccountId::from("owner@example.test"),
        organization: OrganizationId::from("organization-1"),
        temporary_password: "temporary-password".into(),
    }
}

#[test]
fn first_run_password_replacement_and_authority_survive_restart() {
    let root = TestDir::new();
    let store = EmbeddedFileStore::open(root.path().join("hub.json")).expect("open store");
    let mut hub = HubPilot::open(store).expect("open hub");

    let first = hub.bootstrap(bootstrap()).expect("bootstrap succeeds");
    assert!(first.created);
    assert!(first.password_change_required);
    assert!(!hub.state_contains_secret("first-run-secret-at-least-32-characters"));
    assert_eq!(
        hub.authorize(
            &AccountId::from("owner@example.test"),
            &OrganizationId::from("organization-1")
        ),
        Err(AuthorityError::PasswordChangeRequired)
    );

    hub.replace_password(&PasswordChange {
        account: AccountId::from("owner@example.test"),
        current_password: "temporary-password".into(),
        new_password: "replacement-password".into(),
    })
    .expect("temporary password replaced");
    hub.add_member(
        &AccountId::from("owner@example.test"),
        AccountId::from("member@example.test"),
        OrganizationId::from("organization-1"),
        Role::Member,
    )
    .expect("owner adds member");

    drop(hub);
    let store = EmbeddedFileStore::open(root.path().join("hub.json")).expect("reopen store");
    let mut restarted = HubPilot::open(store).expect("restart hub");
    let repeated = restarted
        .bootstrap(bootstrap())
        .expect("bootstrap is idempotent");
    assert!(!repeated.created);
    assert!(!repeated.password_change_required);
    assert!(
        restarted
            .authorize(
                &AccountId::from("owner@example.test"),
                &OrganizationId::from("organization-1")
            )
            .is_ok()
    );
    assert_eq!(
        restarted.register_daemon(
            &AccountId::from("member@example.test"),
            RegistrationRequest::new(
                OrganizationId::from("organization-1"),
                DaemonId::from("daemon-1"),
                "enroll-1",
                [DaemonPermission::HubExecute]
            )
        ),
        Err(AuthorityError::ManageResourcesRequired.into())
    );
    assert_eq!(
        restarted.authorize(
            &AccountId::from("member@example.test"),
            &OrganizationId::from("other-organization")
        ),
        Err(AuthorityError::OrganizationUnavailable)
    );
}

#[test]
fn direct_registration_permissions_generations_and_continuation_are_idempotent() {
    let root = TestDir::new();
    let mut hub =
        HubPilot::open(EmbeddedFileStore::open(root.path().join("hub.json")).expect("open store"))
            .expect("open hub");
    hub.bootstrap(bootstrap()).expect("bootstrap");
    hub.replace_password(&PasswordChange {
        account: AccountId::from("owner@example.test"),
        current_password: "temporary-password".into(),
        new_password: "replacement-password".into(),
    })
    .expect("replace password");

    let request = RegistrationRequest::new(
        OrganizationId::from("organization-1"),
        DaemonId::from("daemon-1"),
        "enroll-1",
        [DaemonPermission::HubExecute],
    );
    let registered = hub
        .register_daemon(&AccountId::from("owner@example.test"), request.clone())
        .expect("owner registers daemon");
    assert_eq!(
        hub.register_daemon(&AccountId::from("owner@example.test"), request)
            .expect("same enrollment is idempotent"),
        registered
    );

    assert_eq!(
        hub.connect_daemon(&DaemonId::from("daemon-1"), []),
        Err(SessionError::PermissionAgreementMismatch)
    );
    let first = hub
        .connect_daemon(&DaemonId::from("daemon-1"), [DaemonPermission::HubExecute])
        .expect("matching permissions connect");
    assert_eq!(first.generation, 1);

    let continued = hub
        .continue_session(&DaemonId::from("daemon-1"), first.generation, "continue-1")
        .expect("continuation replaces physical session");
    assert_eq!(continued.generation, 2);
    assert_eq!(
        hub.continue_session(&DaemonId::from("daemon-1"), first.generation, "continue-1")
            .expect("same continuation is idempotent"),
        continued
    );
    assert_eq!(
        hub.continue_session(&DaemonId::from("daemon-1"), first.generation, "continue-2"),
        Err(SessionError::SupersededGeneration)
    );
}

#[test]
fn embedded_store_contract_names_unproven_database_semantics() {
    assert_eq!(
        EmbeddedFileStore::SEMANTICS,
        StoreSemantics::SingleProcessFileSnapshot
    );
    assert!(EmbeddedFileStore::LIMITATIONS.contains("not PGlite"));
    assert!(EmbeddedFileStore::LIMITATIONS.contains("not PostgreSQL"));
    assert!(EmbeddedFileStore::LIMITATIONS.contains("no cross-process transactions"));
}
