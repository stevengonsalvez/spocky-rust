use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use paseo_hub_pilot::http::{HttpRequest, HubHttpService, serve_one};
use paseo_hub_pilot::{
    AccountId, Bootstrap, BrowserAccountStatus, EmbeddedFileStore, HubPilot, InvitationRole,
    OrganizationId, PasswordChange,
};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("paseo-hub-http-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }
}

#[test]
fn packet_level_organization_selection_is_membership_bound_and_durable() {
    let root = TestDir::new();
    let path = root.0.join("selection.json");
    let owner = AccountId::from("owner@example.test");
    let first = OrganizationId::from("organization-1");
    let second = OrganizationId::from("organization-2");
    let mut hub = HubPilot::open(EmbeddedFileStore::open(&path).unwrap()).unwrap();
    hub.bootstrap(Bootstrap {
        instance_secret: "http-selection-secret-at-least-32-characters".into(),
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
    hub.create_organization_for_session(&session, second)
        .unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut service = HubHttpService::new(hub);
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            serve_one(&mut stream, &mut service).unwrap();
        }
    });
    let cookie = format!("paseo_session={}", session.as_str());
    let selected = request(
        address,
        "POST",
        "/api/auth/paseo/select-organization",
        Some(&cookie),
        Some(r#"{"organizationId":"organization-1"}"#),
    );
    assert!(selected.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(selected.ends_with(r#"{"organizationId":"organization-1"}"#));
    let unavailable = request(
        address,
        "POST",
        "/api/auth/paseo/select-organization",
        Some(&cookie),
        Some(r#"{"organizationId":"foreign"}"#),
    );
    assert!(unavailable.starts_with("HTTP/1.1 404 Not Found\r\n"));
    assert!(unavailable.ends_with(r#"{"error":"organization_unavailable"}"#));
    server.join().unwrap();

    let restarted = HubPilot::open(EmbeddedFileStore::open(path).unwrap()).unwrap();
    assert_eq!(
        restarted.active_organization_for_session(&session),
        Some(first)
    );
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove test directory");
    }
}

#[test]
fn active_account_state_includes_pending_manager_invitations() {
    let root = TestDir::new();
    let owner = AccountId::from("owner@example.test");
    let organization = OrganizationId::from("organization-1");
    let mut hub = HubPilot::open_at(
        EmbeddedFileStore::open(root.0.join("invitations.json")).unwrap(),
        1_700_000_000,
    )
    .unwrap();
    hub.bootstrap(Bootstrap {
        instance_secret: "invitation-state-secret-at-least-32-characters".into(),
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
    let invitation = hub
        .create_invitation(
            &owner,
            &organization,
            "MEMBER@EXAMPLE.TEST",
            InvitationRole::Member,
        )
        .unwrap();
    let session = hub.sign_in(&owner, "replacement-password").unwrap();
    hub.complete_app_setup(&session).unwrap();
    let response = HubHttpService::new(hub).handle(&HttpRequest {
        method: "GET".into(),
        path: "/api/auth/paseo/state".into(),
        headers: std::collections::BTreeMap::from([(
            "cookie".into(),
            format!("paseo_session={}", session.as_str()),
        )]),
        body: Vec::new(),
    });
    let state: serde_json::Value = serde_json::from_slice(&response.body).unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(
        state["team"]["invitations"],
        serde_json::json!([{
            "id": invitation.id,
            "email": "member@example.test",
            "role": "member",
            "expiresAt": "2023-11-16T22:13:20.000Z",
            "link": format!("https://hub.example.test/?invitation={}", invitation.id)
        }])
    );
}

#[test]
fn packet_level_state_requires_organization_when_multi_membership_session_has_no_selection() {
    let root = TestDir::new();
    let path = root.0.join("organization-required.json");
    let owner = AccountId::from("owner@example.test");
    let mut hub = HubPilot::open(EmbeddedFileStore::open(&path).unwrap()).unwrap();
    hub.bootstrap(Bootstrap {
        instance_secret: "organization-required-secret-at-least-32-characters".into(),
        owner: owner.clone(),
        organization: OrganizationId::from("organization-1"),
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
    hub.create_organization_for_session(&session, OrganizationId::from("organization-2"))
        .unwrap();
    drop(hub);

    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["active_browser_organizations"] = serde_json::json!({});
    fs::write(&path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();

    let hub = HubPilot::open(EmbeddedFileStore::open(path).unwrap()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut service = HubHttpService::new(hub);
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        serve_one(&mut stream, &mut service).unwrap();
    });
    let response = request(
        address,
        "GET",
        "/api/auth/paseo/state",
        Some(&format!("paseo_session={}", session.as_str())),
        None,
    );
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.ends_with(
        r#"{"status":"organizationRequired","account":{"id":"owner@example.test","name":"owner","email":"owner@example.test"},"memberships":[{"id":"organization-1","name":"organization-1","slug":"organization-1","membershipId":"membership:organization-1:owner@example.test","role":"owner"},{"id":"organization-2","name":"organization-2","slug":"organization-2","membershipId":"membership:organization-2:owner@example.test","role":"owner"}],"canCreateOrganization":false}"#
    ));
    server.join().unwrap();
}

#[test]
#[allow(clippy::too_many_lines)]
fn packet_level_auth_gate_matches_status_body_cookie_and_restart_state() {
    let root = TestDir::new();
    let path = root.0.join("hub.json");
    let mut hub = HubPilot::open_at(
        EmbeddedFileStore::open(&path).expect("open store"),
        1_700_000_000,
    )
    .expect("open hub");
    hub.bootstrap(Bootstrap {
        instance_secret: "http-runtime-secret-at-least-32-characters".into(),
        owner: AccountId::from("owner@example.test"),
        organization: OrganizationId::from("organization-1"),
        temporary_password: "temporary-password".into(),
    })
    .expect("bootstrap");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind random local port");
    let address = listener.local_addr().expect("listener address");
    let server = thread::spawn(move || {
        let mut service = HubHttpService::new(hub);
        for _ in 0..9 {
            let (mut stream, _) = listener.accept().expect("accept request");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("bound read");
            serve_one(&mut stream, &mut service).expect("serve request");
        }
    });

    let signed_out = request(address, "GET", "/api/auth/paseo/state", None, None);
    assert!(signed_out.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(signed_out.ends_with(r#"{"status":"signedOut","registration":"invite_only"}"#));

    let signed_in = request(
        address,
        "POST",
        "/api/auth/sign-in/email",
        None,
        Some(r#"{"email":"owner@example.test","password":"temporary-password"}"#),
    );
    assert!(signed_in.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(
        signed_in.contains(
            "set-cookie: paseo_session=hub-session-1; Path=/; HttpOnly; SameSite=Lax\r\n"
        )
    );
    assert!(signed_in.ends_with(r#"{"status":"passwordChangeRequired"}"#));
    let cookie = "paseo_session=hub-session-1";

    let password_change_state =
        request(address, "GET", "/api/auth/paseo/state", Some(cookie), None);
    assert!(password_change_state.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(password_change_state.ends_with(
        r#"{"status":"passwordChangeRequired","account":{"id":"owner@example.test","name":"owner","email":"owner@example.test"}}"#
    ));

    let gated = request(
        address,
        "POST",
        "/api/auth/paseo/complete-app-setup",
        Some(cookie),
        Some("{}"),
    );
    assert!(gated.starts_with("HTTP/1.1 403 Forbidden\r\n"));
    assert!(gated.ends_with(r#"{"error":"password_change_required"}"#));

    let changed = request(
        address,
        "POST",
        "/api/auth/paseo/change-password",
        Some(cookie),
        Some(r#"{"currentPassword":"temporary-password","newPassword":"replacement-password"}"#),
    );
    assert!(changed.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(changed.ends_with(r#"{"status":"appSetupRequired"}"#));

    let app_setup_state = request(address, "GET", "/api/auth/paseo/state", Some(cookie), None);
    assert!(app_setup_state.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(app_setup_state.ends_with(
        r#"{"status":"appSetupRequired","account":{"id":"owner@example.test","name":"owner","email":"owner@example.test"},"organization":{"id":"organization-1","name":"organization-1","slug":"organization-1"},"memberships":[{"id":"organization-1","name":"organization-1","slug":"organization-1","membershipId":"membership:organization-1:owner@example.test","role":"owner"}],"capabilities":{"view":true,"manageMembers":true,"manageOwners":true,"manageResources":true}}"#
    ));

    let invitation = request(
        address,
        "POST",
        "/api/auth/paseo/create-invitation",
        Some(cookie),
        Some(r#"{"email":"MEMBER@EXAMPLE.TEST","role":"member"}"#),
    );
    assert!(invitation.starts_with("HTTP/1.1 201 Created\r\n"));
    assert!(invitation.ends_with(
        r#"{"id":"invitation-1","email":"member@example.test","role":"member","expiresAt":"2023-11-16T22:13:20.000Z","link":"https://hub.example.test/?invitation=invitation-1"}"#
    ));

    let completed = request(
        address,
        "POST",
        "/api/auth/paseo/complete-app-setup",
        Some(cookie),
        Some("{}"),
    );
    assert!(completed.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(completed.ends_with(r#"{"status":"active"}"#));

    let active_state = request(address, "GET", "/api/auth/paseo/state", Some(cookie), None);
    assert!(active_state.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(active_state.ends_with(
        r#"{"status":"active","account":{"id":"owner@example.test","name":"owner","email":"owner@example.test"},"memberships":[{"id":"organization-1","name":"organization-1","slug":"organization-1","membershipId":"membership:organization-1:owner@example.test","role":"owner"}],"organization":{"id":"organization-1","name":"organization-1","slug":"organization-1"},"membership":{"id":"membership:organization-1:owner@example.test","role":"owner"},"capabilities":{"view":true,"manageMembers":true,"manageOwners":true,"manageResources":true},"isInstanceOperator":true,"canCreateOrganization":false,"team":{"members":[{"id":"membership:organization-1:owner@example.test","userId":"owner@example.test","name":"owner","email":"owner@example.test","role":"owner"}],"invitations":[{"id":"invitation-1","email":"member@example.test","role":"member","expiresAt":"2023-11-16T22:13:20.000Z","link":"https://hub.example.test/?invitation=invitation-1"}]}}"#
    ));
    server.join().expect("server thread");

    let mut old_state: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    old_state
        .as_object_mut()
        .unwrap()
        .remove("instance_operator");
    fs::write(&path, serde_json::to_vec_pretty(&old_state).unwrap()).unwrap();

    let restarted =
        HubPilot::open(EmbeddedFileStore::open(&path).expect("reopen store")).expect("restart hub");
    assert_eq!(
        restarted
            .browser_account_status(Some(&paseo_hub_pilot::SessionToken::from("hub-session-1"))),
        BrowserAccountStatus::Active
    );
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let mut service = HubHttpService::new(restarted);
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        serve_one(&mut stream, &mut service).unwrap();
    });
    let old_state_response = request(address, "GET", "/api/auth/paseo/state", Some(cookie), None);
    assert!(old_state_response.contains(r#""isInstanceOperator":true"#));
    server.join().unwrap();
}

fn request(
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: Option<&str>,
) -> String {
    let body = body.unwrap_or_default();
    let mut stream = TcpStream::connect(address).expect("connect to local Hub pilot");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("bound response read");
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nhost: {address}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n",
        body.len()
    )
    .expect("write request headers");
    if let Some(cookie) = cookie {
        write!(stream, "cookie: {cookie}\r\n").expect("write cookie");
    }
    write!(stream, "connection: close\r\n\r\n{body}").expect("write request body");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    response
}
