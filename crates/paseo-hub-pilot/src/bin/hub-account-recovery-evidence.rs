use std::collections::BTreeMap;
use std::env;
use std::error::Error;

use paseo_hub_pilot::http::{HttpRequest, HubHttpService, RegistrationMode};
use paseo_hub_pilot::{EmbeddedFileStore, HubPilot};
use serde_json::{Value, json};

fn main() -> Result<(), Box<dyn Error>> {
    let root = env::temp_dir().join(format!(
        "paseo-hub-account-recovery-evidence-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root)?;
    let result = capture(&root);
    std::fs::remove_dir_all(&root)?;
    println!("{}", serde_json::to_string_pretty(&result?)?);
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn capture(root: &std::path::Path) -> Result<Value, Box<dyn Error>> {
    let hub = HubPilot::open_at(
        EmbeddedFileStore::open(root.join("hub.json"))?,
        1_700_000_000,
    )?;
    let mut service = HubHttpService::with_registration(
        hub,
        RegistrationMode::OpenVerified,
        "http://localhost:3000",
    );
    let signup = service.handle(&post(
        "/api/auth/sign-up/email",
        r#"{"name":"Verified User","email":"verified@example.test","password":"original-password","callbackURL":"http://localhost:3000/?auth=email-verification"}"#,
    ));
    let verification_email = service.account_emails().verifications[0].clone();
    let unverified = service.handle(&post(
        "/api/auth/sign-in/email",
        r#"{"email":"verified@example.test","password":"original-password"}"#,
    ));
    let verification = service.handle(&get(&verification_email.url, None));
    let set_cookie = verification.headers.get("set-cookie").cloned().unwrap();
    let cookie = set_cookie.split(';').next().unwrap();
    let session = service.handle(&get("/api/auth/get-session", Some(cookie)));
    let session_body: Value = serde_json::from_slice(&session.body)?;

    service.handle(&post(
        "/api/auth/request-password-reset",
        r#"{"email":"verified@example.test","redirectTo":"http://localhost:3000/?auth=password-reset"}"#,
    ));
    service.handle(&post(
        "/api/auth/request-password-reset",
        r#"{"email":"missing@example.test","redirectTo":"http://localhost:3000/?auth=password-reset"}"#,
    ));
    let reset_email = service.account_emails().password_resets[0].clone();
    let reset_callback = service.handle(&get(&reset_email.url, None));
    let reset_location = reset_callback.headers.get("location").cloned().unwrap();
    let reset_token = reset_location.split("token=").nth(1).unwrap();
    service.handle(&post(
        "/api/auth/reset-password",
        &format!(r#"{{"token":"{reset_token}","newPassword":"replacement-password"}}"#),
    ));
    let revoked = service.handle(&get("/api/auth/get-session", Some(cookie)));
    let old_password = service.handle(&post(
        "/api/auth/sign-in/email",
        r#"{"email":"verified@example.test","password":"original-password"}"#,
    ));
    let new_password = service.handle(&post(
        "/api/auth/sign-in/email",
        r#"{"email":"verified@example.test","password":"replacement-password"}"#,
    ));
    let replay = service.handle(&post(
        "/api/auth/reset-password",
        &format!(r#"{{"token":"{reset_token}","newPassword":"another-password"}}"#),
    ));
    let replay_body: Value = serde_json::from_slice(&replay.body)?;

    Ok(json!({
        "baseline": "rust-candidate",
        "signup": {
            "status": signup.status,
            "setCookie": signup.headers.get("set-cookie"),
        },
        "verificationEmail": {
            "url": verification_email.url,
            "token": verification_email.token.as_str(),
            "email": verification_email.recipient,
        },
        "unverifiedSignIn": {
            "status": unverified.status,
            "body": serde_json::from_slice::<Value>(&unverified.body)?,
        },
        "verification": {
            "status": verification.status,
            "location": verification.headers.get("location"),
            "setCookie": set_cookie,
        },
        "session": {
            "status": session.status,
            "email": session_body["user"]["email"],
        },
        "passwordResetEmails": [{
            "url": reset_email.url,
            "token": reset_email.token.as_str(),
            "email": reset_email.recipient,
        }],
        "resetCallback": {
            "status": reset_callback.status,
            "location": reset_location,
        },
        "revokedSession": {
            "status": revoked.status,
            "body": String::from_utf8(revoked.body)?,
        },
        "oldPasswordStatus": old_password.status,
        "newPasswordStatus": new_password.status,
        "replayCode": replay_body["code"],
    }))
}

fn post(path: &str, body: &str) -> HttpRequest {
    HttpRequest {
        method: "POST".into(),
        path: path.into(),
        headers: BTreeMap::from([
            ("content-type".into(), "application/json".into()),
            ("origin".into(), "http://localhost:3000".into()),
        ]),
        body: body.as_bytes().to_vec(),
    }
}

fn get(path: &str, cookie: Option<&str>) -> HttpRequest {
    let mut headers = BTreeMap::new();
    if let Some(cookie) = cookie {
        headers.insert("cookie".into(), cookie.into());
    }
    HttpRequest {
        method: "GET".into(),
        path: path.into(),
        headers,
        body: Vec::new(),
    }
}
