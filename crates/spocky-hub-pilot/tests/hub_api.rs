//! Behavioral tests for the scoped organization public API, CLI device authorization and the
//! `OpenAPI` document. The byte-for-byte differential against the pinned Hub lives in
//! `hub_api_evidence.rs`; these tests state the behavior in terms of flows and outcomes.

use std::cell::Cell;
use std::rc::Rc;

use spocky_hub_pilot::public_api::{
    AccessFailure, ApiKeyAuthorizer, ApiRequest, ApiResponse, AuthorizationOutcome, BrowserAccess,
    CliAuthorizations, Composition, CredentialKind, Headers, Json, ListProjectsResult,
    MemoryCliAuthorizations, OperationAuthenticator, OperationError, OrganizationAccess, PublicApi,
    PublicAuthorization, PublicCredentialAuthenticator, PublicOperations, PublicProject, document,
    parse_json, random_ids,
};
use spocky_hub_pilot::{ApiKeyAuthorization, ApiKeyScope};

/// Operations that only list one project; everything else is unreachable in these tests.
struct Projects;

macro_rules! unreachable_operations {
    ($($name:ident($($input:ty)?) -> $result:ty;)*) => {
        $(fn $name(
            &mut self,
            _authorization: &PublicAuthorization,
            $(_input: &$input,)?
        ) -> Result<$result, OperationError> {
            unreachable!("not used by this test")
        })*
    };
}

impl PublicOperations for Projects {
    fn list_projects(
        &mut self,
        authorization: &PublicAuthorization,
    ) -> Result<ListProjectsResult, OperationError> {
        assert_eq!(authorization.organization_id, "org-acme");
        Ok(ListProjectsResult::Listed(vec![PublicProject {
            id: "84af3583-23ff-4fcc-9838-ed3262499be2".to_owned(),
            name: "Payments".to_owned(),
            slug: "payments".to_owned(),
        }]))
    }

    unreachable_operations! {
        list_triggers() -> spocky_hub_pilot::public_api::ListTriggersResult;
        validate_trigger(spocky_hub_pilot::public_api::TriggerYamlInput) -> spocky_hub_pilot::public_api::ValidateTriggerResult;
        install_trigger(spocky_hub_pilot::public_api::TriggerYamlInput) -> spocky_hub_pilot::public_api::InstallTriggerResult;
        list_configuration_resources() -> spocky_hub_pilot::public_api::ListConfigurationResourcesResult;
        list_setup_resources() -> spocky_hub_pilot::public_api::ListSetupResourcesResult;
        validate_configuration(spocky_hub_pilot::public_api::InstallConfigurationInput) -> spocky_hub_pilot::public_api::ValidateConfigurationResult;
        install_configuration(spocky_hub_pilot::public_api::InstallConfigurationInput) -> spocky_hub_pilot::public_api::InstallConfigurationResult;
        dispatch_manual_run(spocky_hub_pilot::public_api::DispatchManualRunInput) -> spocky_hub_pilot::public_api::DispatchManualRunResult;
        issue_enrollment_token() -> spocky_hub_pilot::public_api::IssueEnrollmentTokenResult;
    }
}

/// No API keys exist: every API key bearer is unauthorized.
struct NoKeys;

impl ApiKeyAuthorizer for NoKeys {
    fn authorize_api_key(
        &mut self,
        _authorization: &str,
        _required_scope: ApiKeyScope,
    ) -> Result<ApiKeyAuthorization, OperationError> {
        Ok(ApiKeyAuthorization::Unauthorized)
    }
}

struct Owner;

impl BrowserAccess for Owner {
    fn reject_cookie_mutation(&self, _request: &ApiRequest) -> Option<ApiResponse> {
        None
    }

    fn resolve_organization_access(
        &self,
        _request: &ApiRequest,
    ) -> Result<OrganizationAccess, AccessFailure> {
        Ok(OrganizationAccess {
            session_id: "session-owner".to_owned(),
            account_id: "user-owner".to_owned(),
            organization_id: "org-acme".to_owned(),
            organization_name: "Acme".to_owned(),
            organization_slug: "acme".to_owned(),
            membership_id: "member-owner".to_owned(),
            manage_resources: true,
        })
    }
}

fn request(method: &str, url: &str, headers: &[(&str, &str)], body: &str) -> ApiRequest {
    let mut list = Headers::new();
    for (name, value) in headers {
        list.append(name, value);
    }
    ApiRequest::new(method, url, list, body.as_bytes().to_vec()).expect("absolute URL")
}

fn post(url: &str, body: &str) -> ApiRequest {
    request("POST", url, &[("content-type", "application/json")], body)
}

struct Harness {
    clock: Rc<Cell<i64>>,
    store: Rc<MemoryCliAuthorizations>,
    cli: CliAuthorizations,
}

fn harness() -> Harness {
    let clock = Rc::new(Cell::new(1_786_017_600_000));
    let store = Rc::new(MemoryCliAuthorizations::new({
        let clock = Rc::clone(&clock);
        Rc::new(move || clock.get())
    }));
    let counter = Rc::new(Cell::new(0_u8));
    let cli = CliAuthorizations::new(
        store.clone(),
        Some(Box::new(Owner)),
        Some("https://hub.test".to_owned()),
        Box::new(move |size| {
            counter.set(counter.get() + 1);
            vec![counter.get(); size]
        }),
        random_ids(),
    );
    Harness { clock, store, cli }
}

fn body(response: &ApiResponse) -> Json {
    parse_json(&response.text()).expect("JSON response body")
}

fn field(value: &Json, key: &str) -> String {
    match value.get(key) {
        Some(Json::String(text)) => text.clone(),
        other => panic!("expected string {key}, got {other:?}"),
    }
}

#[test]
fn device_authorization_issues_one_credential_that_works_for_every_scope_until_revoked() {
    let mut hub = harness();
    let started = hub
        .cli
        .start(
            &post("https://hub.test/api/v1/cli-authorizations", "{}"),
            None,
        )
        .expect("start");
    assert_eq!(started.status, 201);
    let started = body(&started);
    let device_code = field(&started, "deviceCode");
    let user_code = field(&started, "userCode");
    assert_eq!(
        field(&started, "verificationUri"),
        "https://hub.test/cli-login"
    );
    assert_eq!(
        field(&started, "verificationUriComplete"),
        format!("https://hub.test/cli-login?code={user_code}")
    );
    assert_eq!(user_code.len(), 15);

    let poll = |hub: &mut Harness| {
        hub.cli.poll(&post(
            "https://hub.test/api/v1/cli-authorizations/poll",
            &Json::object([("deviceCode", Json::string(&device_code))]).stringify(),
        ))
    };
    assert_eq!(field(&body(&poll(&mut hub)), "status"), "pending");
    assert_eq!(field(&body(&poll(&mut hub)), "status"), "slow_down");

    let decision = Json::object([
        (
            "userCode",
            Json::string(&user_code.to_lowercase().replace('-', "")),
        ),
        ("decision", Json::string("approve")),
        ("organizationId", Json::string("org-acme")),
    ])
    .stringify();
    let decided = hub
        .cli
        .decide(&post(
            "https://hub.test/cli-authorizations/decision",
            &decision,
        ))
        .expect("decide");
    assert_eq!(field(&body(&decided), "status"), "approved");
    let again = hub
        .cli
        .decide(&post(
            "https://hub.test/cli-authorizations/decision",
            &decision,
        ))
        .expect("decide again");
    assert_eq!(again.status, 404);

    hub.clock.set(hub.clock.get() + 60_000);
    let authorized = body(&poll(&mut hub));
    assert_eq!(field(&authorized, "status"), "authorized");
    assert_eq!(field(&authorized, "organizationId"), "org-acme");
    let credential = field(&authorized, "credential");
    assert_eq!(field(&body(&poll(&mut hub)), "status"), "disclosed");

    let mut authenticator = PublicCredentialAuthenticator::new(NoKeys, Rc::clone(&hub.store));
    let mut headers = Headers::new();
    headers.append("authorization", &format!("Bearer {credential}"));
    for scope in spocky_hub_pilot::public_api::ALL_SCOPES {
        let outcome = authenticator.authorize(&headers, scope).expect("storage");
        let AuthorizationOutcome::Authorized(access) = outcome else {
            panic!("credential must be authorized for {scope:?}");
        };
        assert_eq!(access.kind, CredentialKind::CliCredential);
        assert_eq!(access.organization_id, "org-acme");
        assert_eq!(access.scopes.len(), 5);
    }
    assert!(hub.store.revoke_credential(&credential[..22]));
    assert_eq!(
        authenticator
            .authorize(&headers, ApiKeyScope::ProjectsRead)
            .expect("storage"),
        AuthorizationOutcome::Unauthorized
    );
}

#[test]
fn device_authorization_limits_requests_per_client_and_expires_after_ten_minutes() {
    let mut hub = harness();
    let start = |hub: &mut Harness, client: &str| {
        hub.cli
            .start(
                &post("https://hub.test/api/v1/cli-authorizations", "{}"),
                Some(client),
            )
            .expect("start")
    };
    for _ in 0..5 {
        assert_eq!(start(&mut hub, "198.51.100.1").status, 201);
    }
    let limited = start(&mut hub, "198.51.100.1");
    assert_eq!(limited.status, 429);
    assert_eq!(limited.headers.get("retry-after").as_deref(), Some("5"));
    assert_eq!(
        limited.text(),
        "{\"status\":\"retry_later\",\"interval\":5}"
    );
    assert_eq!(start(&mut hub, "198.51.100.2").status, 201);

    hub.clock.set(hub.clock.get() + 600_000);
    assert_eq!(start(&mut hub, "198.51.100.1").status, 201);
}

#[test]
fn device_authorization_without_browser_access_cannot_inspect_or_decide() {
    let store = Rc::new(MemoryCliAuthorizations::new(Rc::new(|| 0)));
    let mut cli = CliAuthorizations::new(
        store,
        None,
        None,
        Box::new(|size| vec![7; size]),
        random_ids(),
    );
    let inspected = cli
        .inspect(&post(
            "https://hub.test/cli-authorizations/inspect",
            "{\"userCode\":\"A\"}",
        ))
        .expect("inspect");
    assert_eq!(
        (inspected.status, inspected.text()),
        (503, "{\"error\":\"auth_unavailable\"}".to_owned())
    );
}

struct Allowing;

impl OperationAuthenticator for Allowing {
    fn authorize(
        &mut self,
        headers: &Headers,
        required_scope: ApiKeyScope,
    ) -> Result<AuthorizationOutcome, OperationError> {
        Ok(match headers.get("authorization").as_deref() {
            Some("Bearer ok") => AuthorizationOutcome::Authorized(PublicAuthorization {
                kind: CredentialKind::ApiKey,
                credential_id: "key".to_owned(),
                organization_id: "org-acme".to_owned(),
                scopes: vec![required_scope],
            }),
            Some("Bearer narrow") => AuthorizationOutcome::Forbidden,
            _ => AuthorizationOutcome::Unauthorized,
        })
    }
}

fn api() -> PublicApi {
    PublicApi::new(
        Composition::Enabled(Box::new(Allowing)),
        Some(Box::new(Projects)),
        random_ids(),
    )
    .expect("operations are provided")
}

#[test]
fn routes_authenticates_and_maps_results_with_one_request_identity() {
    let mut api = api();
    let listed = api.handle(&request(
        "GET",
        "https://hub.test/api/v1/projects?ignored=1",
        &[
            ("authorization", "Bearer ok"),
            ("x-request-id", "  caller-1  "),
        ],
        "",
    ));
    assert_eq!(listed.status, 200);
    assert_eq!(
        listed.headers.get("x-request-id").as_deref(),
        Some("caller-1")
    );
    assert_eq!(
        listed.text(),
        "{\"projects\":[{\"id\":\"84af3583-23ff-4fcc-9838-ed3262499be2\",\"name\":\"Payments\",\"slug\":\"payments\"}]}"
    );

    let missing = api.handle(&request("GET", "https://hub.test/api/v1/nothing", &[], ""));
    assert_eq!(missing.status, 404);
    assert_eq!(
        missing.headers.get("content-type").as_deref(),
        Some("application/problem+json")
    );
    let wrong_method = api.handle(&request(
        "POST",
        "https://hub.test/api/v1/projects",
        &[],
        "",
    ));
    assert_eq!(wrong_method.status, 405);
    assert_eq!(wrong_method.headers.get("allow").as_deref(), Some("GET"));

    let unauthorized = api.handle(&request("GET", "https://hub.test/api/v1/projects", &[], ""));
    assert_eq!(unauthorized.status, 401);
    assert_eq!(
        unauthorized.headers.get("www-authenticate").as_deref(),
        Some("Bearer")
    );
    let forbidden = api.handle(&request(
        "GET",
        "https://hub.test/api/v1/projects",
        &[("authorization", "Bearer narrow")],
        "",
    ));
    assert_eq!(forbidden.status, 403);
    assert!(
        forbidden
            .text()
            .contains("This operation requires the projects:read scope.")
    );
}

#[test]
fn validates_request_bodies_before_running_the_operation() {
    let mut api = api();
    let auth = [
        ("authorization", "Bearer ok"),
        ("content-type", "application/json"),
    ];
    let invalid = api.handle(&request(
        "POST",
        "https://hub.test/api/v1/configurations/install",
        &auth,
        "{\"projectSlug\":\"\",\"unexpected\":true}",
    ));
    assert_eq!(invalid.status, 400);
    let problem = body(&invalid);
    let paths: Vec<String> = match problem.get("issues") {
        Some(Json::Array(issues)) => issues
            .iter()
            .map(|issue| issue.get("path").map(Json::stringify).unwrap_or_default())
            .collect(),
        other => panic!("issues expected, got {other:?}"),
    };
    assert_eq!(paths, ["[\"projectSlug\"]", "[\"files\"]", "[]"]);
    let not_json = api.handle(&request(
        "POST",
        "https://hub.test/api/v1/manual-runs",
        &[
            ("authorization", "Bearer ok"),
            ("content-type", "text/plain"),
        ],
        "{}",
    ));
    assert_eq!(field(&body(&not_json), "code"), "invalid_json");
}

#[test]
fn unavailable_composition_and_missing_operations_follow_the_baseline() {
    assert_eq!(
        PublicApi::new(Composition::Enabled(Box::new(Allowing)), None, random_ids())
            .err()
            .map(|error| error.to_string()),
        Some("enabled public API requires application operations".to_owned())
    );
    let mut api =
        PublicApi::new(Composition::Unavailable, None, random_ids()).expect("unavailable");
    let response = api.handle(&request("GET", "https://hub.test/api/v1/projects", &[], ""));
    assert_eq!(response.status, 503);
    assert_eq!(
        field(&body(&response), "code"),
        "infrastructure_unavailable"
    );
}

#[test]
fn openapi_document_lists_every_operation_with_its_scope_and_documented_statuses() {
    let document = document();
    let Some(Json::Object(paths)) = document.get("paths") else {
        panic!("paths expected");
    };
    assert_eq!(paths.len(), 12);
    for definition in spocky_hub_pilot::public_api::MANIFEST {
        let item = paths
            .iter()
            .find(|(path, _)| path == definition.path)
            .map(|(_, item)| item)
            .expect("manifest path is documented");
        let operation = item.get(definition.method).expect("method is documented");
        assert_eq!(
            operation.get("operationId"),
            Some(&Json::string(definition.id.as_str()))
        );
        assert_eq!(
            operation.get("x-required-scopes"),
            Some(&Json::Array(vec![Json::string(
                spocky_hub_pilot::public_api::scope_name(definition.scope)
            )]))
        );
        let Some(Json::Object(responses)) = operation.get("responses") else {
            panic!("responses expected");
        };
        let documented: Vec<&str> = responses
            .iter()
            .map(|(status, _)| status.as_str())
            .collect();
        let expected: Vec<String> = definition
            .responses
            .iter()
            .map(|(status, _)| status.to_string())
            .collect();
        assert_eq!(documented, expected);
    }
    let served = api().openapi();
    assert_eq!(served.status, 200);
    assert_eq!(
        served.headers.get("cache-control").as_deref(),
        Some("public, max-age=300")
    );
    assert_eq!(served.text(), document.stringify());
}

#[test]
fn a_client_supplied_client_address_header_does_not_choose_the_capacity_key() {
    let mut hub = harness();
    for _ in 0..5 {
        let response = hub
            .cli
            .start(
                &request(
                    "POST",
                    "https://hub.test/api/v1/cli-authorizations",
                    &[("x-paseo-client-address", "forged-1")],
                    "{}",
                ),
                Some("198.51.100.9"),
            )
            .expect("start");
        assert_eq!(response.status, 201);
    }
    // Same peer address, a different forged header: still the same bucket.
    let limited = hub
        .cli
        .start(
            &request(
                "POST",
                "https://hub.test/api/v1/cli-authorizations",
                &[("x-paseo-client-address", "forged-2")],
                "{}",
            ),
            Some("198.51.100.9"),
        )
        .expect("start");
    assert_eq!(limited.status, 429);
}
