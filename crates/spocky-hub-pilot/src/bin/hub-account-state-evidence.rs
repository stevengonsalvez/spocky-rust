use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use spocky_hub_pilot::http::{HttpRequest, HubHttpService};
use spocky_hub_pilot::{AccountId, Bootstrap, EmbeddedFileStore, HubPilot, OrganizationId};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Result<Self, std::io::Error> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "spocky-hub-account-state-evidence-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct InvitationAcceptance {
    signed_up_status: u16,
    accepted_status: u16,
    accepted_body: Value,
    invited_active: Value,
    owner_after_acceptance: Value,
}

struct AdmissionFailures {
    without_invitation: Value,
    unknown_invitation: Value,
    wrong_email: Value,
    invalid_email: Value,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = TestDir::new()?;
    let mut hub = HubPilot::open(EmbeddedFileStore::open(root.0.join("hub.json"))?)?;
    hub.bootstrap(Bootstrap {
        instance_secret: "account-state-candidate-secret-at-least-32-characters".into(),
        owner: AccountId::from("owner@example.test"),
        organization: OrganizationId::from("organization-1"),
        temporary_password: "temporary-password".into(),
    })?;
    let mut service = HubHttpService::new(hub);
    let signed_out = state(&mut service, None)?;
    let sign_in = service.handle(&request(
        "POST",
        "/api/auth/sign-in/email",
        None,
        Some(json!({
            "email": "owner@example.test",
            "password": "temporary-password"
        })),
    ));
    require_status(sign_in.status, 200)?;
    let cookie = sign_in
        .headers
        .get("set-cookie")
        .and_then(|value| value.split(';').next())
        .ok_or("sign-in did not issue a session cookie")?;
    let password_change_required = state(&mut service, Some(cookie))?;
    let changed = service.handle(&request(
        "POST",
        "/api/auth/paseo/change-password",
        Some(cookie),
        Some(json!({
            "currentPassword": "temporary-password",
            "newPassword": "replacement-password"
        })),
    ));
    require_status(changed.status, 200)?;
    let app_setup_required = state(&mut service, Some(cookie))?;
    let completed = service.handle(&request(
        "POST",
        "/api/auth/paseo/complete-app-setup",
        Some(cookie),
        Some(json!({})),
    ));
    require_status(completed.status, 200)?;
    let invitation = service.handle(&request(
        "POST",
        "/api/auth/paseo/create-invitation",
        Some(cookie),
        Some(json!({ "email": "member@example.test", "role": "member" })),
    ));
    require_status(invitation.status, 201)?;
    let canceled_invitation = service.handle(&request(
        "POST",
        "/api/auth/paseo/create-invitation",
        Some(cookie),
        Some(json!({ "email": "cancel@example.test", "role": "admin" })),
    ));
    require_status(canceled_invitation.status, 201)?;
    let canceled = service.handle(&request(
        "POST",
        "/api/auth/paseo/cancel-invitation",
        Some(cookie),
        Some(json!({ "invitationId": "invitation-2" })),
    ));
    require_status(canceled.status, 200)?;
    let canceled_body: Value = serde_json::from_slice(&canceled.body)?;
    let active = state(&mut service, Some(cookie))?;
    let admission = admission_failures(&mut service)?;
    let acceptance = accept_invitation(&mut service, cookie)?;

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schemaVersion": 1,
            "operations": {
                "createInvitationStatus": invitation.status,
                "cancelInvitationStatus": canceled.status,
                "cancelInvitationBody": canceled_body,
                "signUpInvitedStatus": acceptance.signed_up_status,
                "acceptInvitationStatus": acceptance.accepted_status,
                "acceptInvitationBody": acceptance.accepted_body,
                "admissionWithoutInvitation": admission.without_invitation,
                "admissionWithUnknownInvitation": admission.unknown_invitation,
                "admissionWithWrongEmail": admission.wrong_email,
                "admissionWithInvalidEmail": admission.invalid_email
            },
            "states": {
                "signedOut": signed_out,
                "passwordChangeRequired": password_change_required,
                "appSetupRequired": app_setup_required,
                "active": active,
                "invitedActive": acceptance.invited_active,
                "ownerAfterAcceptance": acceptance.owner_after_acceptance
            }
        }))?
    );
    Ok(())
}

fn accept_invitation(
    service: &mut HubHttpService<EmbeddedFileStore>,
    owner_cookie: &str,
) -> Result<InvitationAcceptance, Box<dyn std::error::Error>> {
    let signed_up = service.handle(&request(
        "POST",
        "/api/auth/sign-up/email",
        None,
        Some(json!({
            "name": "Invited Member",
            "email": "member@example.test",
            "password": "member-password",
            "invitation": "invitation-1"
        })),
    ));
    require_status(signed_up.status, 200)?;
    let invited_sign_in = service.handle(&request(
        "POST",
        "/api/auth/sign-in/email",
        None,
        Some(json!({
            "email": "member@example.test",
            "password": "member-password"
        })),
    ));
    require_status(invited_sign_in.status, 200)?;
    let invited_cookie = invited_sign_in
        .headers
        .get("set-cookie")
        .and_then(|value| value.split(';').next())
        .ok_or("invited sign-in did not issue a session cookie")?;
    let accepted = service.handle(&request(
        "POST",
        "/api/auth/paseo/accept-invitation",
        Some(invited_cookie),
        Some(json!({ "invitationId": "invitation-1" })),
    ));
    require_status(accepted.status, 200)?;
    Ok(InvitationAcceptance {
        signed_up_status: signed_up.status,
        accepted_status: accepted.status,
        accepted_body: serde_json::from_slice(&accepted.body)?,
        invited_active: state(service, Some(invited_cookie))?,
        owner_after_acceptance: state(service, Some(owner_cookie))?,
    })
}

fn signup_failure(
    service: &mut HubHttpService<EmbeddedFileStore>,
    body: Value,
) -> Result<Value, Box<dyn std::error::Error>> {
    let response = service.handle(&request(
        "POST",
        "/api/auth/sign-up/email",
        None,
        Some(body),
    ));
    Ok(json!({
        "status": response.status,
        "body": serde_json::from_slice::<Value>(&response.body)?
    }))
}

fn admission_failures(
    service: &mut HubHttpService<EmbeddedFileStore>,
) -> Result<AdmissionFailures, Box<dyn std::error::Error>> {
    Ok(AdmissionFailures {
        without_invitation: signup_failure(
            service,
            json!({
                "name": "Member",
                "email": "member@example.test",
                "password": "member-password"
            }),
        )?,
        unknown_invitation: signup_failure(
            service,
            json!({
                "name": "Member",
                "email": "member@example.test",
                "password": "member-password",
                "invitation": "unknown"
            }),
        )?,
        wrong_email: signup_failure(
            service,
            json!({
                "name": "Wrong",
                "email": "wrong@example.test",
                "password": "member-password",
                "invitation": "invitation-1"
            }),
        )?,
        invalid_email: signup_failure(
            service,
            json!({
                "name": "Member",
                "email": "invalid",
                "password": "member-password",
                "invitation": "invitation-1"
            }),
        )?,
    })
}

fn state(
    service: &mut HubHttpService<EmbeddedFileStore>,
    cookie: Option<&str>,
) -> Result<Value, Box<dyn std::error::Error>> {
    let response = service.handle(&request("GET", "/api/auth/paseo/state", cookie, None));
    require_status(response.status, 200)?;
    Ok(serde_json::from_slice(&response.body)?)
}

fn request(method: &str, path: &str, cookie: Option<&str>, body: Option<Value>) -> HttpRequest {
    let mut headers = BTreeMap::new();
    if let Some(cookie) = cookie {
        headers.insert("cookie".into(), cookie.into());
    }
    if body.is_some() {
        headers.insert("content-type".into(), "application/json".into());
    }
    HttpRequest {
        method: method.into(),
        path: path.into(),
        headers,
        body: body.map_or_else(Vec::new, |value| serde_json::to_vec(&value).unwrap()),
    }
}

fn require_status(actual: u16, expected: u16) -> Result<(), Box<dyn std::error::Error>> {
    if actual == expected {
        Ok(())
    } else {
        Err(format!("expected HTTP {expected}, got {actual}").into())
    }
}
