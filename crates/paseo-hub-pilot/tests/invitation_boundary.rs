use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use paseo_hub_pilot::{
    AccountId, Bootstrap, EmbeddedFileStore, HubError, HubPilot, InvitationRole, OrganizationId,
    PasswordChange, Role,
};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "paseo-hub-invitations-{}-{nonce}",
            std::process::id()
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

fn configured(path: &Path) -> HubPilot<EmbeddedFileStore> {
    let mut hub =
        HubPilot::open(EmbeddedFileStore::open(path).expect("open store")).expect("open hub");
    hub.bootstrap(Bootstrap {
        instance_secret: "first-run-secret-at-least-32-characters".into(),
        owner: AccountId::from("owner@example.test"),
        organization: OrganizationId::from("organization-1"),
        temporary_password: "temporary-password".into(),
    })
    .expect("bootstrap");
    hub.replace_password(&PasswordChange {
        account: AccountId::from("owner@example.test"),
        current_password: "temporary-password".into(),
        new_password: "replacement-password".into(),
    })
    .expect("replace password");
    hub
}

#[test]
fn normalized_reinvite_reuses_one_live_credential_and_survives_restart() {
    let root = TestDir::new();
    let state = root.path().join("hub.json");
    let owner = AccountId::from("owner@example.test");
    let organization = OrganizationId::from("organization-1");
    let mut hub = configured(&state);

    let first = hub
        .create_invitation(
            &owner,
            &organization,
            " Bob@Example.Test ",
            InvitationRole::Admin,
        )
        .expect("create invitation");
    let repeated = hub
        .create_invitation(
            &owner,
            &organization,
            "bob@example.test",
            InvitationRole::Member,
        )
        .expect("reuse pending invitation");
    assert_eq!(repeated, first);
    assert_eq!(first.email, "bob@example.test");
    assert!(first.link.ends_with(&format!("/?invitation={}", first.id)));
    assert_eq!(
        hub.pending_invitations(&owner, &organization).unwrap(),
        vec![first.clone()]
    );

    drop(hub);
    let restarted =
        HubPilot::open(EmbeddedFileStore::open(state).expect("reopen store")).expect("restart hub");
    assert_eq!(
        restarted
            .pending_invitations(&owner, &organization)
            .expect("list after restart"),
        vec![first]
    );
}

#[test]
fn manager_gate_flag_and_seat_cap_precede_new_invitation() {
    let root = TestDir::new();
    let mut hub = configured(&root.path().join("hub.json"));
    let owner = AccountId::from("owner@example.test");
    let member = AccountId::from("member@example.test");
    let organization = OrganizationId::from("organization-1");
    hub.add_member(&owner, member.clone(), organization.clone(), Role::Member)
        .expect("add member");

    assert_eq!(
        hub.create_invitation(
            &member,
            &organization,
            "blocked@example.test",
            InvitationRole::Member,
        ),
        Err(HubError::InvitationManagementRequired)
    );
    hub.set_invitation_entitlements(&owner, &organization, false, None)
        .expect("disable invitations");
    assert_eq!(
        hub.create_invitation(
            &owner,
            &organization,
            "blocked@example.test",
            InvitationRole::Member,
        ),
        Err(HubError::InvitationsDisabled)
    );
    hub.set_invitation_entitlements(&owner, &organization, true, Some(2))
        .expect("cap seats");
    assert_eq!(
        hub.create_invitation(
            &owner,
            &organization,
            "blocked@example.test",
            InvitationRole::Member,
        ),
        Err(HubError::SeatLimitReached)
    );
    assert!(
        hub.pending_invitations(&owner, &organization)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn cancel_replaces_credential_and_acceptance_is_email_bound_and_one_shot() {
    let root = TestDir::new();
    let mut hub = configured(&root.path().join("hub.json"));
    let owner = AccountId::from("owner@example.test");
    let bob = AccountId::from("bob@example.test");
    let eve = AccountId::from("eve@example.test");
    let organization = OrganizationId::from("organization-1");

    let canceled = hub
        .create_invitation(&owner, &organization, bob.as_str(), InvitationRole::Member)
        .expect("invite Bob");
    hub.cancel_invitation(&owner, &organization, &canceled.id)
        .expect("cancel invitation");
    assert_eq!(
        hub.accept_invitation(&bob, &canceled.id),
        Err(HubError::InvitationUnavailable)
    );
    let replacement = hub
        .create_invitation(&owner, &organization, bob.as_str(), InvitationRole::Member)
        .expect("replace invitation");
    assert_ne!(replacement.id, canceled.id);
    assert_eq!(
        hub.accept_invitation(&eve, &replacement.id),
        Err(HubError::InvitationUnavailable)
    );
    assert_eq!(
        hub.accept_invitation(&bob, &replacement.id)
            .expect("Bob accepts"),
        organization
    );
    assert_eq!(
        hub.accept_invitation(&bob, &replacement.id),
        Err(HubError::InvitationUnavailable)
    );
    assert!(
        hub.pending_invitations(&owner, &organization)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn current_members_are_rejected_without_leaving_pending_credentials() {
    let root = TestDir::new();
    let mut hub = configured(&root.path().join("hub.json"));
    let owner = AccountId::from("owner@example.test");
    let member = AccountId::from("member@example.test");
    let organization = OrganizationId::from("organization-1");
    hub.add_member(&owner, member.clone(), organization.clone(), Role::Member)
        .expect("add member");
    assert_eq!(
        hub.create_invitation(
            &owner,
            &organization,
            member.as_str(),
            InvitationRole::Member,
        ),
        Err(HubError::AlreadyMember)
    );
    assert!(
        hub.pending_invitations(&owner, &organization)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn expiration_releases_reserved_seat_and_replaces_the_credential() {
    let root = TestDir::new();
    let state = root.path().join("hub.json");
    let owner = AccountId::from("owner@example.test");
    let bob = AccountId::from("bob@example.test");
    let organization = OrganizationId::from("organization-1");
    let mut hub = HubPilot::open_at(
        EmbeddedFileStore::open(&state).expect("open store"),
        1_000_000,
    )
    .expect("open fixed-time hub");
    hub.bootstrap(Bootstrap {
        instance_secret: "first-run-secret-at-least-32-characters".into(),
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
    hub.set_invitation_entitlements(&owner, &organization, true, Some(2))
        .expect("cap seats");
    let expired = hub
        .create_invitation(&owner, &organization, bob.as_str(), InvitationRole::Member)
        .expect("create invitation");
    drop(hub);

    let mut later = HubPilot::open_at(
        EmbeddedFileStore::open(state).expect("reopen store"),
        expired.expires_at_epoch_seconds,
    )
    .expect("open at exact expiry");
    assert!(
        later
            .pending_invitations(&owner, &organization)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        later.accept_invitation(&bob, &expired.id),
        Err(HubError::InvitationUnavailable)
    );
    let replacement = later
        .create_invitation(&owner, &organization, bob.as_str(), InvitationRole::Member)
        .expect("expired seat is reusable");
    assert_ne!(replacement.id, expired.id);
}
