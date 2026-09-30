use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use spocky_hub_pilot::http::{HttpRequest, HubHttpService, RegistrationMode};
use spocky_hub_pilot::{EmbeddedFileStore, HubPilot};

static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[test]
#[allow(clippy::too_many_lines)]
fn packet_level_verified_account_recovery_matches_pinned_boundary() {
    let root = temp_directory();
    let state_path = root.join("hub.json");
    let hub =
        HubPilot::open_at(EmbeddedFileStore::open(&state_path).unwrap(), 1_700_000_000).unwrap();
    let mut service = HubHttpService::with_registration(
        hub,
        RegistrationMode::OpenVerified,
        "http://localhost:3000",
    );

    let signup = service.handle(&post(
        "/api/auth/sign-up/email",
        r#"{"name":"Verified User","email":"verified@example.test","password":"original-password","callbackURL":"http://localhost:3000/?auth=email-verification"}"#,
    ));
    assert_eq!(signup.status, 200);
    assert!(!signup.headers.contains_key("set-cookie"));
    let verification = service
        .account_emails()
        .verifications
        .first()
        .unwrap()
        .clone();
    assert_eq!(verification.recipient, "verified@example.test");
    assert_eq!(
        verification.callback_url,
        "http://localhost:3000/?auth=email-verification"
    );
    assert_eq!(verification.message.subject, "Verify your Spocky Hub email");
    assert_eq!(verification.message.to, "verified@example.test");

    let unverified = service.handle(&post(
        "/api/auth/sign-in/email",
        r#"{"email":"verified@example.test","password":"original-password"}"#,
    ));
    assert_eq!(unverified.status, 403);
    assert_eq!(
        unverified.body,
        br#"{"message":"Email not verified","code":"EMAIL_NOT_VERIFIED"}"#
    );

    drop(service);
    let restarted =
        HubPilot::open_at(EmbeddedFileStore::open(state_path).unwrap(), 1_700_000_001).unwrap();
    let mut service = HubHttpService::with_registration(
        restarted,
        RegistrationMode::OpenVerified,
        "http://localhost:3000",
    );
    let verified = service.handle(&get(&verification.url, None));
    assert_eq!(verified.status, 302);
    assert_eq!(
        verified.headers.get("location").map(String::as_str),
        Some("http://localhost:3000/?auth=email-verification")
    );
    let cookie = verified.headers.get("set-cookie").unwrap();
    let session_cookie = cookie.split(';').next().unwrap();
    assert!(session_cookie.starts_with("better-auth.session_token="));

    let session = service.handle(&get("/api/auth/get-session", Some(session_cookie)));
    assert_eq!(session.status, 200);
    let session: serde_json::Value = serde_json::from_slice(&session.body).unwrap();
    assert_eq!(session["user"]["email"], "verified@example.test");

    assert_eq!(
        service
            .handle(&post(
                "/api/auth/request-password-reset",
                r#"{"email":"verified@example.test","redirectTo":"http://localhost:3000/?auth=password-reset"}"#,
            ))
            .status,
        200
    );
    assert_eq!(
        service
            .handle(&post(
                "/api/auth/request-password-reset",
                r#"{"email":"missing@example.test","redirectTo":"http://localhost:3000/?auth=password-reset"}"#,
            ))
            .status,
        200
    );
    assert_eq!(service.account_emails().password_resets.len(), 1);
    assert_eq!(
        service.account_emails().password_resets[0].message.subject,
        "Reset your Spocky Hub password"
    );
    let reset_url = service.account_emails().password_resets[0].url.clone();
    let reset_callback = service.handle(&get(&reset_url, None));
    assert_eq!(reset_callback.status, 302);
    let location = reset_callback.headers.get("location").unwrap();
    assert!(location.starts_with("http://localhost:3000/?auth=password-reset&token="));
    let token = location.split("token=").nth(1).unwrap();

    assert_eq!(
        service
            .handle(&post(
                "/api/auth/reset-password",
                &format!(r#"{{"token":"{token}","newPassword":"replacement-password"}}"#),
            ))
            .status,
        200
    );
    assert_eq!(
        service
            .handle(&get("/api/auth/get-session", Some(session_cookie)))
            .body,
        b"null"
    );
    assert_eq!(
        service
            .handle(&post(
                "/api/auth/sign-in/email",
                r#"{"email":"verified@example.test","password":"original-password"}"#,
            ))
            .status,
        401
    );
    assert_eq!(
        service
            .handle(&post(
                "/api/auth/sign-in/email",
                r#"{"email":"verified@example.test","password":"replacement-password"}"#,
            ))
            .status,
        200
    );
    let replay = service.handle(&post(
        "/api/auth/reset-password",
        &format!(r#"{{"token":"{token}","newPassword":"another-password"}}"#),
    ));
    assert_eq!(replay.status, 400);
    assert_eq!(replay.body, br#"{"code":"INVALID_TOKEN"}"#);

    fs::remove_dir_all(root).unwrap();
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

fn temp_directory() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "spocky-hub-account-recovery-http-{}-{nonce}-{}",
        std::process::id(),
        TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    path
}
