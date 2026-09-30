use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use spocky_hub_pilot::{
    AccountId, AuthorityError, Bootstrap, DaemonId, DaemonPermission, EmbeddedFileStore, HubPilot,
    OrganizationId, PasswordChange, RegistrationRequest, Role, SessionError,
};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Result<Self, std::io::Error> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "spocky-hub-runtime-evidence-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = TestDir::new()?;
    let state_path = root.path().join("hub.json");
    let owner = AccountId::from("owner@example.test");
    let member = AccountId::from("member@example.test");
    let organization = OrganizationId::from("organization-1");
    let daemon = DaemonId::from("daemon-1");
    let mut hub = HubPilot::open(EmbeddedFileStore::open(&state_path)?)?;

    let bootstrap = Bootstrap {
        instance_secret: "runtime-evidence-secret-at-least-32-characters".into(),
        owner: owner.clone(),
        organization: organization.clone(),
        temporary_password: "temporary-password".into(),
    };
    let first = hub.bootstrap(bootstrap.clone())?;
    let password_gate = hub.authorize(&owner, &organization);
    hub.replace_password(&PasswordChange {
        account: owner.clone(),
        current_password: "temporary-password".into(),
        new_password: "replacement-password".into(),
    })?;
    hub.add_member(&owner, member.clone(), organization.clone(), Role::Member)?;
    drop(hub);

    let mut restarted = HubPilot::open(EmbeddedFileStore::open(&state_path)?)?;
    let repeated = restarted.bootstrap(bootstrap)?;
    let owner_authorized = restarted.authorize(&owner, &organization).is_ok();
    let member_registration = restarted.register_daemon(
        &member,
        RegistrationRequest::new(
            organization.clone(),
            daemon.clone(),
            "member-enrollment",
            [DaemonPermission::HubExecute],
        ),
    );
    let request = RegistrationRequest::new(
        organization,
        daemon.clone(),
        "owner-enrollment",
        [DaemonPermission::HubExecute],
    );
    let registration = restarted.register_daemon(&owner, request.clone())?;
    let idempotent_registration = restarted.register_daemon(&owner, request)? == registration;
    let permission_mismatch = restarted.connect_daemon(&daemon, []);
    let connected = restarted
        .connect_daemon(&daemon, [DaemonPermission::HubExecute])
        .map_err(session_error)?;
    let continued = restarted
        .continue_session(&daemon, connected.generation, "continue-1")
        .map_err(session_error)?;
    let idempotent_continuation = restarted
        .continue_session(&daemon, connected.generation, "continue-1")
        .map_err(session_error)?
        == continued;
    let superseded = restarted.continue_session(&daemon, connected.generation, "continue-2");

    let evidence = json!({
        "schemaVersion": 1,
        "storeSemantics": "singleProcessFileSnapshot",
        "bootstrap": {
            "created": first.created,
            "passwordChangeRequired": first.password_change_required,
            "passwordGateRejected": password_gate == Err(AuthorityError::PasswordChangeRequired),
            "secretAbsentFromState": !restarted.state_contains_secret(
                "runtime-evidence-secret-at-least-32-characters"
            )
        },
        "restart": {
            "bootstrapRepeatedWithoutCreate": !repeated.created,
            "ownerAuthorized": owner_authorized
        },
        "authorizationFailures": {
            "memberRegistration": usize::from(
                member_registration == Err(AuthorityError::ManageResourcesRequired.into())
            )
        },
        "registration": {
            "generation": registration.registration_generation,
            "idempotent": idempotent_registration
        },
        "sessionFailures": {
            "permissionMismatch": usize::from(
                permission_mismatch == Err(SessionError::PermissionAgreementMismatch)
            ),
            "supersededGeneration": usize::from(
                superseded == Err(SessionError::SupersededGeneration)
            )
        },
        "sessions": {
            "connectedGeneration": connected.generation,
            "continuedGeneration": continued.generation,
            "continuationIdempotent": idempotent_continuation
        },
        "limitations": [
            "not PGlite",
            "not PostgreSQL",
            "no HTTP server",
            "no cross-process transactions"
        ]
    });
    println!("{}", serde_json::to_string_pretty(&evidence)?);
    Ok(())
}

fn session_error(error: SessionError) -> std::io::Error {
    std::io::Error::other(format!("{error:?}"))
}
