use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use paseo_hub_pilot::{
    AccountId, ApiKeyAuthorization, ApiKeyScope, AuthorityError, Bootstrap, EmbeddedFileStore,
    HubPilot, OrganizationId, PasswordChange, Role,
};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("paseo-hub-api-key-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove test directory");
    }
}

fn configured_hub(
    path: &Path,
) -> (
    HubPilot<EmbeddedFileStore>,
    AccountId,
    AccountId,
    OrganizationId,
) {
    let owner = AccountId::from("owner@example.test");
    let member = AccountId::from("member@example.test");
    let organization = OrganizationId::from("organization-a");
    let mut hub =
        HubPilot::open(EmbeddedFileStore::open(path).expect("open store")).expect("open hub");
    hub.bootstrap(Bootstrap {
        instance_secret: "api-key-runtime-secret-at-least-32-characters".into(),
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
    hub.add_member(&owner, member.clone(), organization.clone(), Role::Member)
        .expect("add member");
    (hub, owner, member, organization)
}

#[test]
fn api_key_secret_scope_last_use_revocation_and_restart_match_boundary() {
    let root = TestDir::new();
    let path = root.0.join("hub.json");
    let (mut hub, owner, member, organization) = configured_hub(&path);

    assert_eq!(
        hub.create_api_key(
            &member,
            &organization,
            "member key",
            [ApiKeyScope::RunsDispatch]
        ),
        Err(AuthorityError::ManageResourcesRequired.into())
    );
    let created = hub
        .create_api_key(
            &owner,
            &organization,
            "deployment A",
            [ApiKeyScope::ConfigurationInstall, ApiKeyScope::RunsDispatch],
        )
        .expect("owner creates key");
    assert!(created.secret.starts_with("paseo_pk_"));
    assert_eq!(created.summary.name, "deployment A");
    assert_eq!(created.summary.prefix.len(), "paseo_pk_".len() + 12);
    assert!(!created.summary.last_used);
    assert!(!created.summary.revoked);
    assert!(!hub.state_contains_secret(&created.secret));
    let snapshot = fs::read_to_string(&path).expect("read persisted key state");
    assert!(snapshot.contains("configuration:install"));
    assert!(snapshot.contains("runs:dispatch"));
    assert!(!snapshot.contains("configuration-install"));

    let listed = hub
        .list_api_keys(&owner, &organization)
        .expect("owner lists keys");
    assert_eq!(listed, vec![created.summary.clone()]);
    assert_eq!(
        hub.list_api_keys(&member, &organization),
        Err(AuthorityError::ManageResourcesRequired.into())
    );
    assert_eq!(
        hub.authorize_api_key(
            &format!("Bearer {}", created.secret),
            ApiKeyScope::DaemonsEnroll
        )
        .expect("authorize missing scope"),
        ApiKeyAuthorization::Forbidden
    );
    assert_eq!(
        hub.authorize_api_key("Bearer nope", ApiKeyScope::RunsDispatch)
            .expect("reject malformed token"),
        ApiKeyAuthorization::Unauthorized
    );
    let wrong = format!("{}wrong", created.secret);
    assert_eq!(
        hub.authorize_api_key(&format!("Bearer {wrong}"), ApiKeyScope::RunsDispatch)
            .expect("reject wrong secret"),
        ApiKeyAuthorization::Unauthorized
    );
    assert!(!hub.list_api_keys(&owner, &organization).unwrap()[0].last_used);

    let authorized = hub
        .authorize_api_key(
            &format!("Bearer {}", created.secret),
            ApiKeyScope::RunsDispatch,
        )
        .expect("authorize matching key");
    let ApiKeyAuthorization::Authorized(access) = authorized else {
        panic!("matching key must authorize");
    };
    assert_eq!(access.organization, organization);
    assert_eq!(access.credential_id, created.summary.id);
    assert!(access.scopes.contains(&ApiKeyScope::RunsDispatch));
    assert!(hub.list_api_keys(&owner, &organization).unwrap()[0].last_used);
    drop(hub);

    let mut restarted =
        HubPilot::open(EmbeddedFileStore::open(&path).expect("reopen store")).expect("restart hub");
    assert!(!restarted.state_contains_secret(&created.secret));
    assert_eq!(
        restarted
            .authorize_api_key(
                &format!("Bearer {}", created.secret),
                ApiKeyScope::RunsDispatch,
            )
            .expect("authorize after restart"),
        ApiKeyAuthorization::Authorized(access)
    );
    assert!(
        restarted
            .revoke_api_key(&owner, &organization, &created.summary.id)
            .expect("revoke key")
    );
    assert_eq!(
        restarted
            .authorize_api_key(
                &format!("Bearer {}", created.secret),
                ApiKeyScope::RunsDispatch,
            )
            .expect("revoked key is rejected"),
        ApiKeyAuthorization::Unauthorized
    );
    assert!(restarted.list_api_keys(&owner, &organization).unwrap()[0].revoked);
}
