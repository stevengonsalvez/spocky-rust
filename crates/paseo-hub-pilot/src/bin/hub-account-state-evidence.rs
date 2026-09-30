use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use paseo_hub_pilot::http::{HttpRequest, HubHttpService};
use paseo_hub_pilot::{AccountId, Bootstrap, EmbeddedFileStore, HubPilot, OrganizationId};
use serde_json::{Value, json};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Result<Self, std::io::Error> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "paseo-hub-account-state-evidence-{}-{nonce}",
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

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "schemaVersion": 1,
            "operations": {
                "createInvitationStatus": invitation.status,
                "cancelInvitationStatus": canceled.status,
                "cancelInvitationBody": canceled_body
            },
            "states": {
                "signedOut": signed_out,
                "passwordChangeRequired": password_change_required,
                "appSetupRequired": app_setup_required,
                "active": active
            }
        }))?
    );
    Ok(())
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
