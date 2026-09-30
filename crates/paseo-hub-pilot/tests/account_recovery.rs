use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use paseo_hub_pilot::{AccountId, EmbeddedFileStore, HubError, HubPilot};

static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[test]
fn verification_and_password_reset_revoke_sessions_reject_replay_and_restart() {
    let root = temp_directory();
    let path = root.join("hub.json");
    let account = AccountId::from("verified@example.test");
    let mut hub = HubPilot::open(EmbeddedFileStore::open(&path).unwrap()).unwrap();

    let verification = hub
        .register_unverified_account(&account, "Verified User", "original-password")
        .unwrap();
    assert_eq!(
        hub.sign_in(&account, "original-password"),
        Err(HubError::EmailNotVerified)
    );
    hub.verify_account(&verification).unwrap();
    let session = hub.sign_in(&account, "original-password").unwrap();

    assert_eq!(
        hub.request_password_reset(&AccountId::from("missing@example.test"))
            .unwrap(),
        None
    );
    let reset = hub
        .request_password_reset(&account)
        .unwrap()
        .expect("existing account dispatch");
    hub.reset_password(&reset, "replacement-password").unwrap();
    assert!(hub.account_for_session(&session).is_none());
    assert_eq!(
        hub.sign_in(&account, "original-password"),
        Err(HubError::InvalidCredentials)
    );
    hub.sign_in(&account, "replacement-password").unwrap();
    assert_eq!(
        hub.reset_password(&reset, "another-password"),
        Err(HubError::InvalidRecoveryToken)
    );
    drop(hub);

    let mut restarted = HubPilot::open(EmbeddedFileStore::open(&path).unwrap()).unwrap();
    restarted
        .sign_in(&account, "replacement-password")
        .expect("replacement survives restart");
    assert!(!restarted.state_contains_secret("replacement-password"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn verification_and_reset_tokens_expire_across_restart() {
    let root = temp_directory();
    let path = root.join("hub.json");
    let account = AccountId::from("expiring@example.test");
    let mut hub = HubPilot::open_at(EmbeddedFileStore::open(&path).unwrap(), 1_000).unwrap();
    let verification = hub
        .register_unverified_account(&account, "Expiring User", "original-password")
        .unwrap();
    drop(hub);

    let mut expired = HubPilot::open_at(EmbeddedFileStore::open(&path).unwrap(), 4_601).unwrap();
    assert_eq!(
        expired.verify_account(&verification),
        Err(HubError::InvalidRecoveryToken)
    );
    drop(expired);

    let mut verified = HubPilot::open_at(EmbeddedFileStore::open(&path).unwrap(), 1_001).unwrap();
    let new_verification = verified
        .register_unverified_account(
            &AccountId::from("reset@example.test"),
            "Reset User",
            "original-password",
        )
        .unwrap();
    verified.verify_account(&new_verification).unwrap();
    let reset_account = AccountId::from("reset@example.test");
    let reset = verified
        .request_password_reset(&reset_account)
        .unwrap()
        .unwrap();
    drop(verified);

    let mut expired = HubPilot::open_at(EmbeddedFileStore::open(&path).unwrap(), 4_602).unwrap();
    assert_eq!(
        expired.reset_password(&reset, "replacement-password"),
        Err(HubError::InvalidRecoveryToken)
    );
    fs::remove_dir_all(root).unwrap();
}

fn temp_directory() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "paseo-hub-account-recovery-{}-{nonce}-{}",
        std::process::id(),
        TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    path
}
