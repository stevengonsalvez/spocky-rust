//! Rust side of the Hub public API, CLI device authorization and `OpenAPI` differential.
//!
//! The case list is `scripts/phase2/hub-api-cases.json`, the same file the pinned Hub capture
//! (`scripts/phase2/hub-api-original.integration.test.ts`) runs. Every run compares the Rust trace
//! byte for byte with the committed baseline trace (`evidence/phase2/hub-api-original.json`) and
//! the Rust `OpenAPI` document with the committed baseline document
//! (`evidence/phase2/hub-api-openapi-original.json`). `SPOCKY_HUB_API_BASELINE` and
//! `SPOCKY_HUB_API_OPENAPI_BASELINE` point at fresh captures instead; `SPOCKY_HUB_API_OUTPUT` and
//! `SPOCKY_HUB_API_OPENAPI_OUTPUT` also write the Rust results to files.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};
use spocky_hub_pilot::public_api::{
    AccessFailure, ApiKeyAuthorizer, ApiRequest, ApiResponse, AuthorizationOutcome, BrowserAccess,
    CliAuthorizations, Composition, ConfigurationResources, CredentialKind,
    DispatchManualRunResult, GithubResource, Headers, InstallConfigurationResult,
    InstallTriggerResult, Issue, IssueEnrollmentTokenResult, Json,
    ListConfigurationResourcesResult, ListProjectsResult, ListSetupResourcesResult,
    ListTriggersResult, MANIFEST, MemoryCliAuthorizations, OperationAuthenticator, OperationError,
    OperationId, OrganizationAccess, PathPart, PublicApi, PublicAuthorization,
    PublicCredentialAuthenticator, PublicOperations, PublicProject, PublicTrigger, SetupResources,
    TriggerFormat, ValidateConfigurationResult, ValidateTriggerResult, WorkflowStatus,
    document_text, parse_json, scope_name,
};
use spocky_hub_pilot::{
    AccountId, ApiKeyAuthorization, ApiKeyScope, Bootstrap, EmbeddedFileStore, HubPilot,
    OrganizationId, PasswordChange,
};

const COMMITTED_TRACE: &str = include_str!("../../../evidence/phase2/hub-api-original.json");
const COMMITTED_OPENAPI: &str =
    include_str!("../../../evidence/phase2/hub-api-openapi-original.json");
const CASES: &str = include_str!("../../../scripts/phase2/hub-api-cases.json");

const BASE: &str = "https://hub.test";
const START_URL: &str = "https://hub.test/api/v1/cli-authorizations";
const POLL_URL: &str = "https://hub.test/api/v1/cli-authorizations/poll";

fn text(value: &Json) -> &str {
    match value {
        Json::String(text) => text,
        other => panic!("expected a string, got {other:?}"),
    }
}

fn member<'a>(value: &'a Json, key: &str) -> &'a Json {
    value.get(key).unwrap_or(&Json::Null)
}

fn items(value: &Json) -> &[Json] {
    match value {
        Json::Array(items) => items,
        Json::Null => &[],
        other => panic!("expected an array, got {other:?}"),
    }
}

fn number(value: &Json) -> f64 {
    match value {
        Json::Number(number) => *number,
        other => panic!("expected a number, got {other:?}"),
    }
}

#[allow(clippy::cast_possible_truncation)]
fn integer(value: &Json) -> i64 {
    number(value) as i64
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// `JSON.stringify(value, null, 2)`.
fn pretty(value: &Json) -> String {
    fn write(value: &Json, depth: usize, out: &mut String) {
        let pad = |level: usize| "  ".repeat(level);
        match value {
            Json::Array(values) if values.is_empty() => out.push_str("[]"),
            Json::Object(fields) if fields.is_empty() => out.push_str("{}"),
            Json::Array(values) => {
                out.push_str("[\n");
                for (index, item) in values.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    write(item, depth + 1, out);
                    out.push_str(if index + 1 == values.len() {
                        "\n"
                    } else {
                        ",\n"
                    });
                }
                out.push_str(&pad(depth));
                out.push(']');
            }
            Json::Object(fields) => {
                out.push_str("{\n");
                for (index, (key, item)) in fields.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    out.push_str(&Json::string(key).stringify());
                    out.push_str(": ");
                    write(item, depth + 1, out);
                    out.push_str(if index + 1 == fields.len() {
                        "\n"
                    } else {
                        ",\n"
                    });
                }
                out.push_str(&pad(depth));
                out.push('}');
            }
            scalar => out.push_str(&scalar.stringify()),
        }
    }
    let mut out = String::new();
    write(value, 0, &mut out);
    out
}

fn counter_ids() -> Box<dyn FnMut() -> String> {
    let counter = Rc::new(Cell::new(0_u64));
    Box::new(move || {
        counter.set(counter.get() + 1);
        format!("00000000-0000-4000-8000-{:012x}", counter.get())
    })
}

fn deterministic_bytes() -> Box<dyn FnMut(usize) -> Vec<u8>> {
    let counter = Rc::new(Cell::new(0_u64));
    Box::new(move |size| {
        counter.set(counter.get() + 1);
        let digest = Sha256::digest(format!("spocky-hub-api-random:{}", counter.get()));
        digest[..size].to_vec()
    })
}

// ---- request construction ----

fn materialize(body: &Json, substitute: &dyn Fn(&str) -> String) -> Vec<u8> {
    if let Some(value) = body.get("text") {
        return text(value).as_bytes().to_vec();
    }
    if let Some(value) = body.get("base64") {
        return STANDARD.decode(text(value)).expect("valid base64 body");
    }
    if let Some(repeat) = body.get("repeat") {
        let count = usize::try_from(integer(member(repeat, "count"))).expect("count");
        return format!(
            "{}{}{}",
            text(member(repeat, "prefix")),
            text(member(repeat, "fill")).repeat(count),
            text(member(repeat, "suffix"))
        )
        .into_bytes();
    }
    if let Some(template) = body.get("template") {
        return substitute(&template.stringify()).into_bytes();
    }
    panic!("unknown body spec {body:?}");
}

fn build_request(spec: &Json, substitute: &dyn Fn(&str) -> String) -> ApiRequest {
    let mut headers = Headers::new();
    for pair in items(member(spec, "headers")) {
        let pair = items(pair);
        headers.append(text(&pair[0]), &substitute(text(&pair[1])));
    }
    let body = match member(spec, "body") {
        Json::Null => Vec::new(),
        body => materialize(body, substitute),
    };
    ApiRequest::new(
        text(member(spec, "method")),
        text(member(spec, "url")),
        headers,
        body,
    )
    .expect("absolute request URL")
}

fn response_trace(response: &ApiResponse) -> Vec<(String, Json)> {
    vec![
        (
            "status".to_owned(),
            Json::integer(i64::from(response.status)),
        ),
        (
            "headers".to_owned(),
            Json::Object(
                response
                    .headers
                    .sorted()
                    .into_iter()
                    .map(|(name, value)| (name, Json::String(value)))
                    .collect(),
            ),
        ),
        ("body".to_owned(), Json::String(response.text())),
    ]
}

// ---- operation stubs ----

fn issues_of(value: &Json) -> Vec<Issue> {
    items(value)
        .iter()
        .map(|issue| Issue {
            path: items(member(issue, "path"))
                .iter()
                .map(|part| match part {
                    Json::String(key) => PathPart::Key(key.clone()),
                    other => PathPart::Index(usize::try_from(integer(other)).expect("index")),
                })
                .collect(),
            message: text(member(issue, "message")).to_owned(),
        })
        .collect()
}

fn string_pairs(value: &Json, first: &str, second: &str) -> Vec<(String, String)> {
    items(value)
        .iter()
        .map(|item| {
            (
                text(member(item, first)).to_owned(),
                text(member(item, second)).to_owned(),
            )
        })
        .collect()
}

fn github_of(value: &Json) -> Vec<GithubResource> {
    items(value)
        .iter()
        .map(|item| GithubResource {
            slug: text(member(item, "slug")).to_owned(),
            account_login: text(member(item, "accountLogin")).to_owned(),
            account_type: text(member(item, "accountType")).to_owned(),
            repositories: items(member(item, "repositories"))
                .iter()
                .map(|name| text(name).to_owned())
                .collect(),
        })
        .collect()
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` to epoch milliseconds.
fn epoch_ms(iso: &str) -> i64 {
    let field = |from: usize, to: usize| iso[from..to].parse::<i64>().expect("ISO digits");
    let days = days_from_civil(field(0, 4), field(5, 7), field(8, 10));
    ((days * 24 + field(11, 13)) * 60 + field(14, 16)) * 60_000
        + field(17, 19) * 1000
        + field(20, 23)
}

fn issue_status(value: &Json) -> &str {
    text(member(value, "status"))
}

struct Stub {
    spec: Option<Json>,
    calls: Rc<RefCell<Vec<Json>>>,
}

impl Stub {
    fn respond<T>(
        &self,
        operation: &str,
        authorization: &PublicAuthorization,
        input: Option<&Json>,
        convert: impl FnOnce(&Json) -> T,
    ) -> Result<T, OperationError> {
        self.calls.borrow_mut().push(Json::object([
            ("operation", Json::string(operation)),
            ("authorization", authorization.to_json()),
            ("input", input_digest(input)),
        ]));
        let spec = self.spec.as_ref().expect("an operation was configured");
        if let Some(kind) = spec.get("throw") {
            return Err(if text(kind) == "database" {
                OperationError::DatabaseUnavailable
            } else {
                OperationError::Failed("operation exploded".to_owned())
            });
        }
        Ok(convert(member(spec, "result")))
    }
}

/// Long inputs are recorded as UTF-16 length and SHA-256, as the capture does.
fn input_digest(input: Option<&Json>) -> Json {
    let Some(input) = input else {
        return Json::Null;
    };
    let rendered = input.stringify();
    if utf16_len(&rendered) <= 2048 {
        return Json::String(rendered);
    }
    Json::String(format!(
        "sha256:{}:{}",
        utf16_len(&rendered),
        hex(&Sha256::digest(rendered.as_bytes()))
    ))
}

impl PublicOperations for Stub {
    fn list_triggers(
        &mut self,
        authorization: &PublicAuthorization,
    ) -> Result<ListTriggersResult, OperationError> {
        self.respond("listTriggers", authorization, None, |result| {
            if issue_status(result) == "listed" {
                ListTriggersResult::Listed(
                    items(member(result, "triggers"))
                        .iter()
                        .map(|trigger| PublicTrigger {
                            id: text(member(trigger, "id")).to_owned(),
                            name: text(member(trigger, "name")).to_owned(),
                            enabled: member(trigger, "enabled") == &Json::Bool(true),
                            format: if text(member(trigger, "format")) == "single_run" {
                                TriggerFormat::SingleRun
                            } else {
                                TriggerFormat::LegacyMultistep
                            },
                            yaml: text(member(trigger, "yaml")).to_owned(),
                        })
                        .collect(),
                )
            } else {
                ListTriggersResult::InfrastructureUnavailable
            }
        })
    }

    fn validate_trigger(
        &mut self,
        authorization: &PublicAuthorization,
        input: &spocky_hub_pilot::public_api::TriggerYamlInput,
    ) -> Result<ValidateTriggerResult, OperationError> {
        self.respond(
            "validateTrigger",
            authorization,
            Some(&input.to_json()),
            |result| match issue_status(result) {
                "valid" => ValidateTriggerResult::Valid {
                    name: text(member(result, "name")).to_owned(),
                },
                "invalid_trigger" => {
                    ValidateTriggerResult::InvalidTrigger(issues_of(member(result, "issues")))
                }
                _ => ValidateTriggerResult::InfrastructureUnavailable,
            },
        )
    }

    fn install_trigger(
        &mut self,
        authorization: &PublicAuthorization,
        input: &spocky_hub_pilot::public_api::TriggerYamlInput,
    ) -> Result<InstallTriggerResult, OperationError> {
        self.respond(
            "installTrigger",
            authorization,
            Some(&input.to_json()),
            |result| match issue_status(result) {
                "installed" => InstallTriggerResult::Installed {
                    trigger_id: text(member(result, "triggerId")).to_owned(),
                    name: text(member(result, "name")).to_owned(),
                    revision_id: text(member(result, "revisionId")).to_owned(),
                    version: integer(member(result, "version")),
                },
                "invalid_trigger" => {
                    InstallTriggerResult::InvalidTrigger(issues_of(member(result, "issues")))
                }
                _ => InstallTriggerResult::InfrastructureUnavailable,
            },
        )
    }

    fn list_projects(
        &mut self,
        authorization: &PublicAuthorization,
    ) -> Result<ListProjectsResult, OperationError> {
        self.respond("listProjects", authorization, None, |result| {
            if issue_status(result) == "listed" {
                ListProjectsResult::Listed(
                    items(member(result, "projects"))
                        .iter()
                        .map(|project| PublicProject {
                            id: text(member(project, "id")).to_owned(),
                            name: text(member(project, "name")).to_owned(),
                            slug: text(member(project, "slug")).to_owned(),
                        })
                        .collect(),
                )
            } else {
                ListProjectsResult::InfrastructureUnavailable
            }
        })
    }

    fn list_configuration_resources(
        &mut self,
        authorization: &PublicAuthorization,
    ) -> Result<ListConfigurationResourcesResult, OperationError> {
        self.respond(
            "listConfigurationResources",
            authorization,
            None,
            |result| {
                if issue_status(result) == "listed" {
                    ListConfigurationResourcesResult::Listed(ConfigurationResources {
                        daemons: string_pairs(member(result, "daemons"), "id", "slug"),
                        github: github_of(member(result, "github")),
                        discord: string_pairs(member(result, "discord"), "slug", "guildName"),
                        slack: string_pairs(member(result, "slack"), "slug", "teamName"),
                        linear: string_pairs(member(result, "linear"), "slug", "organizationName"),
                    })
                } else {
                    ListConfigurationResourcesResult::InfrastructureUnavailable
                }
            },
        )
    }

    fn list_setup_resources(
        &mut self,
        authorization: &PublicAuthorization,
    ) -> Result<ListSetupResourcesResult, OperationError> {
        self.respond("listSetupResources", authorization, None, |result| {
            if issue_status(result) == "listed" {
                ListSetupResourcesResult::Listed(SetupResources {
                    github: github_of(member(result, "github")),
                    discord: string_pairs(member(result, "discord"), "guildId", "guildName"),
                    slack: string_pairs(member(result, "slack"), "teamId", "teamName"),
                })
            } else {
                ListSetupResourcesResult::InfrastructureUnavailable
            }
        })
    }

    fn validate_configuration(
        &mut self,
        authorization: &PublicAuthorization,
        input: &spocky_hub_pilot::public_api::InstallConfigurationInput,
    ) -> Result<ValidateConfigurationResult, OperationError> {
        self.respond(
            "validateConfiguration",
            authorization,
            Some(&input.to_json()),
            |result| match issue_status(result) {
                "valid" => ValidateConfigurationResult::Valid {
                    project_slug: text(member(result, "projectSlug")).to_owned(),
                    would_create_project: member(result, "wouldCreateProject") == &Json::Bool(true),
                },
                "project_not_found" => ValidateConfigurationResult::ProjectNotFound,
                "invalid_bundle" => {
                    ValidateConfigurationResult::InvalidBundle(issues_of(member(result, "issues")))
                }
                "invalid_configuration" => ValidateConfigurationResult::InvalidConfiguration(
                    issues_of(member(result, "issues")),
                ),
                _ => ValidateConfigurationResult::InfrastructureUnavailable,
            },
        )
    }

    fn install_configuration(
        &mut self,
        authorization: &PublicAuthorization,
        input: &spocky_hub_pilot::public_api::InstallConfigurationInput,
    ) -> Result<InstallConfigurationResult, OperationError> {
        self.respond(
            "installConfiguration",
            authorization,
            Some(&input.to_json()),
            |result| match issue_status(result) {
                "installed" => InstallConfigurationResult::Installed {
                    project_slug: text(member(result, "projectSlug")).to_owned(),
                    version_id: text(member(result, "versionId")).to_owned(),
                    version: integer(member(result, "version")),
                    active: member(result, "active") == &Json::Bool(true),
                },
                "project_not_found" => InstallConfigurationResult::ProjectNotFound,
                "invalid_bundle" => {
                    InstallConfigurationResult::InvalidBundle(issues_of(member(result, "issues")))
                }
                "invalid_configuration" => InstallConfigurationResult::InvalidConfiguration {
                    version_id: text(member(result, "versionId")).to_owned(),
                    issues: issues_of(member(result, "issues")),
                },
                _ => InstallConfigurationResult::InfrastructureUnavailable,
            },
        )
    }

    fn dispatch_manual_run(
        &mut self,
        authorization: &PublicAuthorization,
        input: &spocky_hub_pilot::public_api::DispatchManualRunInput,
    ) -> Result<DispatchManualRunResult, OperationError> {
        self.respond(
            "dispatchManualRun",
            authorization,
            Some(&input.to_json()),
            |result| match issue_status(result) {
                "dispatched" => DispatchManualRunResult::Dispatched {
                    delivery_key: text(member(result, "deliveryKey")).to_owned(),
                    provider_event_receipt_id: text(member(result, "providerEventReceiptId"))
                        .to_owned(),
                    trigger_run_id: text(member(result, "triggerRunId")).to_owned(),
                    configured_trigger_name: text(member(result, "configuredTriggerName"))
                        .to_owned(),
                    workflow_status: match text(member(result, "workflowStatus")) {
                        "running" => WorkflowStatus::Running,
                        "succeeded" => WorkflowStatus::Succeeded,
                        "failed" => WorkflowStatus::Failed,
                        _ => WorkflowStatus::TimedOut,
                    },
                },
                "project_not_found" => DispatchManualRunResult::ProjectNotFound,
                "actor_forbidden" => DispatchManualRunResult::ActorForbidden,
                "daemon_offline" => DispatchManualRunResult::DaemonOffline,
                "expected_configuration_not_current" => {
                    DispatchManualRunResult::ExpectedConfigurationNotCurrent
                }
                "configuration_not_found" => DispatchManualRunResult::ConfigurationNotFound,
                "trigger_not_found" => DispatchManualRunResult::TriggerNotFound,
                "invalid_input" => DispatchManualRunResult::InvalidInput {
                    trigger_run_id: text(member(result, "triggerRunId")).to_owned(),
                    issues: issues_of(member(result, "issues")),
                },
                "dispatch_conflict" => DispatchManualRunResult::DispatchConflict,
                _ => DispatchManualRunResult::InfrastructureUnavailable,
            },
        )
    }

    fn issue_enrollment_token(
        &mut self,
        authorization: &PublicAuthorization,
    ) -> Result<IssueEnrollmentTokenResult, OperationError> {
        self.respond(
            "issueEnrollmentToken",
            authorization,
            None,
            |result| match issue_status(result) {
                "issued" => IssueEnrollmentTokenResult::Issued {
                    token: text(member(result, "token")).to_owned(),
                    expires_at_ms: epoch_ms(text(member(result, "expiresAt"))),
                },
                "credential_revoked" => IssueEnrollmentTokenResult::CredentialRevoked,
                _ => IssueEnrollmentTokenResult::InfrastructureUnavailable,
            },
        )
    }
}

struct StubAuthenticator {
    outcome: String,
    calls: Rc<RefCell<Vec<String>>>,
}

impl OperationAuthenticator for StubAuthenticator {
    fn authorize(
        &mut self,
        _headers: &Headers,
        required_scope: ApiKeyScope,
    ) -> Result<AuthorizationOutcome, OperationError> {
        self.calls
            .borrow_mut()
            .push(scope_name(required_scope).to_owned());
        match self.outcome.as_str() {
            "unavailable" => Err(OperationError::DatabaseUnavailable),
            "throw" => Err(OperationError::Failed("authenticator exploded".to_owned())),
            "unauthorized" => Ok(AuthorizationOutcome::Unauthorized),
            "forbidden" => Ok(AuthorizationOutcome::Forbidden),
            _ => Ok(AuthorizationOutcome::Authorized(PublicAuthorization {
                kind: CredentialKind::ApiKey,
                credential_id: "key-1".to_owned(),
                organization_id: "organization-1".to_owned(),
                scopes: vec![required_scope],
            })),
        }
    }
}

fn run_case(spec: &Json) -> Json {
    let authorize_calls = Rc::new(RefCell::new(Vec::new()));
    let operation_calls = Rc::new(RefCell::new(Vec::new()));
    let stub = Stub {
        spec: spec
            .get("operation")
            .filter(|value| **value != Json::Null)
            .cloned(),
        calls: Rc::clone(&operation_calls),
    };
    let composition = if member(spec, "composition") == &Json::string("unavailable") {
        Composition::Unavailable
    } else {
        Composition::Enabled(Box::new(StubAuthenticator {
            outcome: text(member(spec, "auth")).to_owned(),
            calls: Rc::clone(&authorize_calls),
        }))
    };
    let mut api = PublicApi::new(composition, Some(Box::new(stub)), counter_ids())
        .expect("operations are provided");
    let request = build_request(member(spec, "request"), &|text| text.to_owned());
    let via = text(member(spec, "via"));
    let response = match via.strip_prefix("operation:") {
        Some(name) => {
            api.handle_operation(OperationId::parse(name).expect("operation id"), &request)
        }
        None => api.handle(&request),
    };
    let mut fields = response_trace(&response);
    fields.push((
        "authorizeCalls".to_owned(),
        Json::Array(
            authorize_calls
                .borrow()
                .iter()
                .map(|scope| Json::string(scope))
                .collect(),
        ),
    ));
    fields.push((
        "operationCalls".to_owned(),
        Json::Array(operation_calls.borrow().clone()),
    ));
    Json::Object(fields)
}

// ---- scenarios ----

struct Keys(Option<Rc<RefCell<HubPilot<EmbeddedFileStore>>>>);

impl ApiKeyAuthorizer for Keys {
    fn authorize_api_key(
        &mut self,
        authorization: &str,
        required_scope: ApiKeyScope,
    ) -> Result<ApiKeyAuthorization, OperationError> {
        match &self.0 {
            None => Ok(ApiKeyAuthorization::Unauthorized),
            Some(hub) => hub
                .borrow_mut()
                .authorize_api_key(authorization, required_scope)
                .map_err(|error| OperationError::Failed(error.to_string())),
        }
    }
}

struct Access {
    kind: String,
}

impl BrowserAccess for Access {
    fn reject_cookie_mutation(&self, _request: &ApiRequest) -> Option<ApiResponse> {
        (self.kind == "reject-cookie")
            .then(|| ApiResponse::json(403, "{\"error\":\"invalid_origin\"}", &[]))
    }

    fn resolve_organization_access(
        &self,
        _request: &ApiRequest,
    ) -> Result<OrganizationAccess, AccessFailure> {
        let product = |status, code: &str| AccessFailure::Product {
            status,
            code: code.to_owned(),
        };
        match self.kind.as_str() {
            "product-401" => Err(product(401, "unauthenticated")),
            "product-403" => Err(product(403, "forbidden_org")),
            "product-500-custom" => Err(product(500, "custom_failure")),
            "throws" => Err(AccessFailure::Failed("access exploded".to_owned())),
            kind => {
                let other = kind == "other-org";
                Ok(OrganizationAccess {
                    session_id: "session-owner".to_owned(),
                    account_id: "user-owner".to_owned(),
                    organization_id: if other { "org-other" } else { "org-acme" }.to_owned(),
                    organization_name: if other { "Other" } else { "Acme" }.to_owned(),
                    organization_slug: if other { "other" } else { "acme" }.to_owned(),
                    membership_id: "member-owner".to_owned(),
                    manage_resources: kind != "member",
                })
            }
        }
    }
}

fn user_code_transform(code: &str, transform: Option<&str>) -> String {
    let exotic: BTreeMap<char, char> = [
        ('A', '\u{ff21}'),
        ('B', '\u{212c}'),
        ('C', '\u{2102}'),
        ('D', '\u{2145}'),
        ('E', '\u{2130}'),
        ('F', '\u{2131}'),
        ('G', '\u{210a}'),
        ('H', '\u{210b}'),
        ('I', '\u{2110}'),
        ('J', '\u{1d409}'),
        ('K', '\u{212a}'),
        ('L', '\u{2112}'),
        ('M', '\u{2133}'),
        ('N', '\u{2115}'),
        ('O', '\u{1d546}'),
        ('P', '\u{2119}'),
        ('Q', '\u{211a}'),
        ('R', '\u{211d}'),
        ('S', '\u{017f}'),
        ('T', '\u{1d413}'),
        ('U', '\u{1d4e4}'),
        ('V', '\u{2164}'),
        ('W', '\u{1d54e}'),
        ('X', '\u{2169}'),
        ('Y', '\u{1d418}'),
        ('Z', '\u{2124}'),
        ('2', '\u{00b2}'),
        ('3', '\u{00b3}'),
        ('4', '\u{2074}'),
        ('5', '\u{2075}'),
        ('6', '\u{2076}'),
        ('7', '\u{2077}'),
    ]
    .into_iter()
    .collect();
    let offset = |base: u32, origin: u32, ch: char| {
        char::from_u32(base + ch as u32 - origin).expect("code point")
    };
    let map = |apply: &dyn Fn(char) -> char| code.chars().map(apply).collect::<String>();
    match transform {
        None => code.to_owned(),
        Some("lower") => code.to_lowercase(),
        Some("strip-dashes") => code.replace('-', ""),
        Some("spaces") => code.replace('-', "  "),
        Some("compat") => map(&|ch| match ch {
            'K' => '\u{212a}',
            'S' => '\u{017f}',
            'C' => '\u{2102}',
            'A' => '\u{ff21}',
            '2' => '\u{00b2}',
            other => other,
        }),
        Some("fullwidth") => map(&|ch| {
            if matches!(ch, 'A'..='Z' | '2'..='7') {
                char::from_u32(ch as u32 + 0xfee0).expect("code point")
            } else {
                ch
            }
        }),
        Some("junk") => format!("!!{code}??"),
        Some("digits-inserted") => code.replace('-', "0-1"),
        Some("truncate") => code[..code.len() - 1].to_owned(),
        Some("sharp-s-prefix") => format!("\u{df}{code}"),
        Some("ligature-prefix") => format!("\u{fb01}{code}"),
        Some("roman-prefix") => format!("\u{2166}{code}"),
        Some("circled-prefix") => format!("\u{24b6}\u{24b7}{code}"),
        Some("circled") => map(&|ch| match ch {
            'A'..='Z' => offset(0x24b6, 'A' as u32, ch),
            '2'..='7' => offset(0x2461, '2' as u32, ch),
            other => other,
        }),
        Some("parenthesized") => map(&|ch| match ch {
            'A'..='Z' => offset(0x249c, 'A' as u32, ch),
            '2'..='7' => offset(0x2474, '1' as u32, ch),
            other => other,
        }),
        Some("exotic") => map(&|ch| exotic.get(&ch).copied().unwrap_or(ch)),
        Some(other) => panic!("unknown transform {other}"),
    }
}

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "spocky-hub-api-evidence-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove test directory");
    }
}

fn open_hub(path: &Path) -> (HubPilot<EmbeddedFileStore>, AccountId, OrganizationId) {
    let owner = AccountId::from("owner@example.test");
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
    (hub, owner, organization)
}

fn scope_of(name: &str) -> ApiKeyScope {
    match name {
        "projects:read" => ApiKeyScope::ProjectsRead,
        "configuration:validate" => ApiKeyScope::ConfigurationValidate,
        "configuration:install" => ApiKeyScope::ConfigurationInstall,
        "runs:dispatch" => ApiKeyScope::RunsDispatch,
        "daemons:enroll" => ApiKeyScope::DaemonsEnroll,
        other => panic!("unknown scope {other}"),
    }
}

struct Scenario {
    store: Rc<MemoryCliAuthorizations>,
    clock: Rc<Cell<i64>>,
    authorizations: CliAuthorizations,
    authenticator: PublicCredentialAuthenticator<Keys, Rc<MemoryCliAuthorizations>>,
    keys: BTreeMap<String, (String, String)>,
    started: BTreeMap<String, (String, String)>,
    credentials: BTreeMap<String, String>,
}

impl Scenario {
    fn substitute(&self, input: &str) -> String {
        let mut out = String::new();
        let mut rest = input;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let Some(close) = rest[open..].find('}') else {
                out.push_str(&rest[open..]);
                return out;
            };
            let token = &rest[open + 1..open + close];
            match self.resolve(token) {
                Some(value) => out.push_str(&value),
                None => out.push_str(&rest[open..=open + close]),
            }
            rest = &rest[open + close + 1..];
        }
        out.push_str(rest);
        out
    }

    fn resolve(&self, token: &str) -> Option<String> {
        let parts: Vec<&str> = token.split('.').collect();
        match parts.as_slice() {
            ["userCode", name] => self.started.get(*name).map(|(_, user)| user.clone()),
            ["key", name] => self.keys.get(*name).map(|(secret, _)| secret.clone()),
            ["key", name, "prefix"] => self.keys.get(*name).map(|(_, prefix)| prefix.clone()),
            ["poll", name, "credential"] => Some(
                self.credentials
                    .get(*name)
                    .cloned()
                    .unwrap_or_else(|| "missing".to_owned()),
            ),
            ["poll", name, "credential", "prefix"] => {
                Some(self.credentials.get(*name).map_or_else(
                    || "missing".to_owned(),
                    |credential| credential[..22].to_owned(),
                ))
            }
            ["poll", name, "credential", "secret"] => {
                Some(self.credentials.get(*name).map_or_else(
                    || "missing".to_owned(),
                    |credential| credential[23..].to_owned(),
                ))
            }
            _ => None,
        }
    }

    fn post(&self, url: &str, headers: &Json, body: &Json) -> ApiRequest {
        let spec = Json::object([
            ("method", Json::string("POST")),
            ("url", Json::string(url)),
            ("headers", headers.clone()),
            ("body", body.clone()),
        ]);
        build_request(&spec, &|input| self.substitute(input))
    }
}

fn thrown_or_response(
    result: Result<ApiResponse, spocky_hub_pilot::public_api::HandlerError>,
) -> Vec<(String, Json)> {
    match result {
        Ok(response) => response_trace(&response),
        Err(error) => vec![("threw".to_owned(), Json::String(error.0))],
    }
}

fn scenario_fields(first: Vec<(&str, Json)>, rest: Vec<(String, Json)>) -> Json {
    let mut fields: Vec<(String, Json)> = first
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect();
    fields.extend(rest);
    Json::Object(fields)
}

fn optional_text(value: &Json) -> Json {
    match value {
        Json::Null => Json::Null,
        other => other.clone(),
    }
}

#[allow(clippy::too_many_lines)]
fn run_scenario(spec: &Json) -> Json {
    let config = member(spec, "config");
    let clock = Rc::new(Cell::new(epoch_ms(text(member(config, "startAt")))));
    let store = Rc::new(MemoryCliAuthorizations::new({
        let clock = Rc::clone(&clock);
        Rc::new(move || clock.get())
    }));
    let access_kind = text(member(config, "access")).to_owned();
    let access: Option<Box<dyn BrowserAccess>> = (access_kind != "none").then(|| {
        Box::new(Access {
            kind: access_kind.clone(),
        }) as Box<dyn BrowserAccess>
    });
    let public_base_url = match member(config, "publicBaseUrl") {
        Json::String(url) => Some(url.clone()),
        _ => None,
    };
    let root = TestDir::new();
    let key_specs = items(member(config, "apiKeys"));
    let hub = (!key_specs.is_empty()).then(|| {
        let (mut hub, owner, organization) = open_hub(&root.0.join("hub.json"));
        let mut created = Vec::new();
        for key in key_specs {
            let scopes: Vec<ApiKeyScope> = items(member(key, "scopes"))
                .iter()
                .map(|scope| scope_of(text(scope)))
                .collect();
            let key_created = hub
                .create_api_key(&owner, &organization, text(member(key, "name")), scopes)
                .expect("create key");
            if member(key, "revoked") == &Json::Bool(true) {
                assert!(
                    hub.revoke_api_key(&owner, &organization, &key_created.summary.id)
                        .expect("revoke key")
                );
            }
            created.push((text(member(key, "name")).to_owned(), key_created));
        }
        (Rc::new(RefCell::new(hub)), created)
    });
    let mut keys = BTreeMap::new();
    if let Some((_, created)) = &hub {
        for (name, key) in created {
            keys.insert(
                name.clone(),
                (key.secret.clone(), key.summary.prefix.clone()),
            );
        }
    }
    // The baseline draws two random byte strings and one UUID per API key it creates, before the
    // first step. Rust creates its keys with the system generator, so the deterministic sequences
    // skip the same draws to stay aligned.
    let mut random_bytes = deterministic_bytes();
    let mut ids = counter_ids();
    for _ in &keys {
        random_bytes(9);
        random_bytes(32);
        ids();
    }
    let mut scenario = Scenario {
        authorizations: CliAuthorizations::new(
            store.clone(),
            access,
            public_base_url,
            random_bytes,
            ids,
        ),
        authenticator: PublicCredentialAuthenticator::new(
            Keys(hub.map(|(hub, _)| hub)),
            Rc::clone(&store),
        ),
        store,
        clock,
        keys,
        started: BTreeMap::new(),
        credentials: BTreeMap::new(),
    };
    let json_headers = Json::Array(vec![Json::Array(vec![
        Json::string("content-type"),
        Json::string("application/json"),
    ])]);
    let mut trace = Vec::new();
    for step in items(member(spec, "steps")) {
        let action = text(member(step, "do"));
        match action {
            "start" => {
                let headers = step.get("headers").unwrap_or(&json_headers);
                let body = step
                    .get("body")
                    .cloned()
                    .unwrap_or_else(|| Json::object([("text", Json::string("{}"))]));
                let request = scenario.post(text(member(step, "url")), headers, &body);
                let result = scenario.authorizations.start(&request);
                if let (Ok(response), Some(name)) = (&result, step.get("as"))
                    && response.status == 201
                {
                    let parsed = parse_json(&response.text()).expect("start body");
                    scenario.started.insert(
                        text(name).to_owned(),
                        (
                            text(member(&parsed, "deviceCode")).to_owned(),
                            text(member(&parsed, "userCode")).to_owned(),
                        ),
                    );
                }
                trace.push(scenario_fields(
                    vec![
                        ("do", Json::string("start")),
                        ("as", optional_text(member(step, "as"))),
                    ],
                    thrown_or_response(result),
                ));
            }
            "startMany" => {
                let count = integer(member(step, "count"));
                let mut statuses: BTreeMap<u16, i64> = BTreeMap::new();
                for index in 0..count {
                    let headers = Json::Array(vec![Json::Array(vec![
                        Json::string("x-paseo-client-address"),
                        Json::String(format!(
                            "{}{index}",
                            text(member(step, "fingerprintPrefix"))
                        )),
                    ])]);
                    let request = scenario.post(
                        START_URL,
                        &headers,
                        &Json::object([("text", Json::string("{}"))]),
                    );
                    let response = scenario.authorizations.start(&request).expect("start");
                    *statuses.entry(response.status).or_default() += 1;
                }
                trace.push(Json::object([
                    ("do", Json::string("startMany")),
                    ("count", Json::integer(count)),
                    (
                        "statuses",
                        Json::Object(
                            statuses
                                .into_iter()
                                .map(|(status, total)| (status.to_string(), Json::integer(total)))
                                .collect(),
                        ),
                    ),
                ]));
            }
            "poll" => {
                let device = step
                    .get("device")
                    .map(|value| text(value).to_owned())
                    .or_else(|| {
                        step.get("of")
                            .and_then(|name| scenario.started.get(text(name)))
                            .map(|(device, _)| device.clone())
                    });
                let body = step.get("body").cloned().unwrap_or_else(|| {
                    Json::object([(
                        "text",
                        Json::String(match &device {
                            Some(device) => {
                                Json::object([("deviceCode", Json::string(device))]).stringify()
                            }
                            None => "{}".to_owned(),
                        }),
                    )])
                });
                let url = step.get("url").map_or(POLL_URL, text);
                let headers = step.get("headers").unwrap_or(&json_headers);
                let request = scenario.post(url, headers, &body);
                let response = scenario.authorizations.poll(&request);
                if response.status == 200
                    && let Some(name) = step.get("of")
                {
                    let parsed = parse_json(&response.text()).expect("poll body");
                    if member(&parsed, "status") == &Json::string("authorized") {
                        scenario.credentials.insert(
                            text(name).to_owned(),
                            text(member(&parsed, "credential")).to_owned(),
                        );
                    }
                }
                trace.push(scenario_fields(
                    vec![
                        ("do", Json::string("poll")),
                        ("of", optional_text(member(step, "of"))),
                    ],
                    response_trace(&response),
                ));
            }
            "inspect" | "decide" => {
                let name = text(member(step, "of"));
                let code =
                    user_code_transform(&scenario.started[name].1, step.get("transform").map(text));
                let user_code = step
                    .get("userCode")
                    .map_or(code, |value| text(value).to_owned());
                let body = step.get("body").cloned().unwrap_or_else(|| {
                    let fields = if action == "inspect" {
                        Json::object([("userCode", Json::String(user_code.clone()))])
                    } else {
                        Json::object([
                            ("userCode", Json::String(user_code.clone())),
                            ("decision", member(step, "decision").clone()),
                            ("organizationId", member(step, "organizationId").clone()),
                        ])
                    };
                    Json::object([("text", Json::String(fields.stringify()))])
                });
                let url = format!(
                    "{BASE}/cli-authorizations/{}",
                    if action == "inspect" {
                        "inspect"
                    } else {
                        "decision"
                    }
                );
                let request = scenario.post(&url, &json_headers, &body);
                let result = if action == "inspect" {
                    scenario.authorizations.inspect(&request)
                } else {
                    scenario.authorizations.decide(&request)
                };
                trace.push(scenario_fields(
                    vec![
                        ("do", Json::string(action)),
                        ("of", optional_text(member(step, "of"))),
                    ],
                    thrown_or_response(result),
                ));
            }
            "advance" => {
                scenario
                    .clock
                    .set(scenario.clock.get() + integer(member(step, "seconds")) * 1000);
                trace.push(Json::object([
                    ("do", Json::string("advance")),
                    ("seconds", member(step, "seconds").clone()),
                ]));
            }
            "revokeCli" => {
                let credential = &scenario.credentials[text(member(step, "of"))];
                assert!(scenario.store.revoke_credential(&credential[..22]));
                trace.push(Json::object([
                    ("do", Json::string("revokeCli")),
                    ("of", member(step, "of").clone()),
                ]));
            }
            "authorize" => {
                let mut headers = Headers::new();
                headers.append(
                    "authorization",
                    &scenario.substitute(text(member(step, "header"))),
                );
                for pair in items(member(step, "headers")) {
                    let pair = items(pair);
                    headers.append(text(&pair[0]), &scenario.substitute(text(&pair[1])));
                }
                let scope = text(member(step, "scope"));
                let outcome = scenario
                    .authenticator
                    .authorize(&headers, scope_of(scope))
                    .expect("credential storage");
                let header = text(member(step, "header"));
                let shown = if utf16_len(header) > 80 {
                    format!("{}...", header.chars().take(80).collect::<String>())
                } else {
                    header.to_owned()
                };
                let (status, kind, organization, scopes) = match &outcome {
                    AuthorizationOutcome::Authorized(access) => (
                        "authorized",
                        Json::string(access.kind.as_str()),
                        Json::string(&access.organization_id),
                        Json::Array(
                            access
                                .scopes
                                .iter()
                                .map(|scope| Json::string(scope_name(*scope)))
                                .collect(),
                        ),
                    ),
                    AuthorizationOutcome::Unauthorized => {
                        ("unauthorized", Json::Null, Json::Null, Json::Null)
                    }
                    AuthorizationOutcome::Forbidden => {
                        ("forbidden", Json::Null, Json::Null, Json::Null)
                    }
                };
                trace.push(Json::object([
                    ("do", Json::string("authorize")),
                    ("header", Json::String(shown)),
                    ("scope", Json::string(scope)),
                    ("status", Json::string(status)),
                    ("kind", kind),
                    ("organizationId", organization),
                    ("scopes", scopes),
                ]));
            }
            other => panic!("unknown step {other}"),
        }
    }
    Json::Array(trace)
}

// ---- trace ----

fn manifest_trace() -> Json {
    Json::Array(
        MANIFEST
            .iter()
            .map(|definition| {
                Json::object([
                    ("id", Json::string(definition.id.as_str())),
                    ("method", Json::string(definition.method)),
                    ("path", Json::string(definition.path)),
                    ("scope", Json::string(scope_name(definition.scope))),
                    (
                        "successStatus",
                        Json::integer(i64::from(definition.success_status)),
                    ),
                    ("resultMapping", Json::string(definition.result_mapping)),
                    ("tag", Json::string(definition.tag)),
                    ("summary", Json::string(definition.summary)),
                    ("description", Json::string(definition.description)),
                    ("hasRequestSchema", Json::Bool(definition.request.is_some())),
                    (
                        "responses",
                        Json::Object(
                            definition
                                .responses
                                .iter()
                                .map(|(status, description)| {
                                    (status.to_string(), Json::string(description))
                                })
                                .collect(),
                        ),
                    ),
                ])
            })
            .collect(),
    )
}

/// Fixture counts are part of the evidence: a shrunken case list or an empty baseline fails.
const CASE_COUNT: usize = 459;
const SCENARIO_COUNT: usize = 60;

fn build_trace(openapi: &str) -> String {
    let spec = parse_json(CASES).expect("case list is JSON");
    assert_eq!(items(member(&spec, "cases")).len(), CASE_COUNT);
    assert_eq!(items(member(&spec, "scenarios")).len(), SCENARIO_COUNT);
    assert_eq!(MANIFEST.len(), 10);
    let construction = match PublicApi::new(
        Composition::Enabled(Box::new(StubAuthenticator {
            outcome: "authorized".to_owned(),
            calls: Rc::new(RefCell::new(Vec::new())),
        })),
        None,
        counter_ids(),
    ) {
        Ok(_) => "no error".to_owned(),
        Err(error) => format!("threw: {error}"),
    };
    let api = PublicApi::new(Composition::Unavailable, None, counter_ids()).expect("unavailable");
    let response = api.openapi();
    assert_eq!(response.text(), openapi);
    let cases: Vec<(String, Json)> = items(member(&spec, "cases"))
        .iter()
        .map(|case| (text(member(case, "name")).to_owned(), run_case(case)))
        .collect();
    let scenarios: Vec<(String, Json)> = items(member(&spec, "scenarios"))
        .iter()
        .map(|scenario| {
            (
                text(member(scenario, "name")).to_owned(),
                run_scenario(scenario),
            )
        })
        .collect();
    let trace = Json::object([
        ("schemaVersion", Json::integer(1)),
        (
            "construction",
            Json::object([("enabledWithoutOperations", Json::String(construction))]),
        ),
        ("manifest", manifest_trace()),
        (
            "openapi",
            Json::object([
                ("status", Json::integer(i64::from(response.status))),
                (
                    "headers",
                    Json::Object(
                        response
                            .headers
                            .sorted()
                            .into_iter()
                            .map(|(name, value)| (name, Json::String(value)))
                            .collect(),
                    ),
                ),
                (
                    "bytes",
                    Json::integer(i64::try_from(openapi.len()).expect("size")),
                ),
                (
                    "sha256",
                    Json::String(hex(&Sha256::digest(openapi.as_bytes()))),
                ),
            ]),
        ),
        ("cases", Json::Object(cases)),
        ("scenarios", Json::Object(scenarios)),
    ]);
    format!("{}\n", pretty(&trace))
}

fn baseline(variable: &str, committed: &str) -> String {
    std::env::var_os(variable).map_or_else(
        || committed.to_owned(),
        |path| fs::read_to_string(path).expect("read baseline file"),
    )
}

fn first_difference(left: &str, right: &str) -> Option<(usize, String, String)> {
    left.lines()
        .zip(right.lines())
        .enumerate()
        .find(|(_, (one, two))| one != two)
        .map(|(line, (one, two))| {
            (
                line + 1,
                one.chars().take(300).collect(),
                two.chars().take(300).collect(),
            )
        })
}

#[test]
fn rust_trace_equals_the_pinned_hub_trace_byte_for_byte() {
    let openapi = document_text();
    let expected_openapi = baseline("SPOCKY_HUB_API_OPENAPI_BASELINE", COMMITTED_OPENAPI);
    let expected_trace = baseline("SPOCKY_HUB_API_BASELINE", COMMITTED_TRACE);
    let trace = build_trace(&openapi);
    assert!(
        expected_trace.matches("\"authorizeCalls\"").count() >= CASE_COUNT - 40,
        "the baseline trace must contain the HTTP cases"
    );
    assert!(expected_openapi.starts_with("{\"openapi\":\"3.1.0\""));
    if let Some(path) = std::env::var_os("SPOCKY_HUB_API_OPENAPI_OUTPUT") {
        fs::write(path, &openapi).expect("write openapi document");
    }
    if let Some(path) = std::env::var_os("SPOCKY_HUB_API_OUTPUT") {
        fs::write(path, &trace).expect("write trace");
    }
    assert!(
        openapi == expected_openapi,
        "Rust OpenAPI document differs from the baseline document ({} and {} bytes); first differing byte {:?}",
        openapi.len(),
        expected_openapi.len(),
        openapi
            .bytes()
            .zip(expected_openapi.bytes())
            .position(|(one, two)| one != two)
    );
    assert!(
        trace == expected_trace,
        "Rust trace differs from the baseline trace; first differing line: {:?}",
        first_difference(&trace, &expected_trace)
    );
}

/// The only inputs the capture and the Rust run are given instead of fresh randomness and the wall
/// clock: request and record identifiers `00000000-0000-4000-8000-<n>`, random bytes taken from
/// SHA-256 of a counter, and a fixed clock. Nothing in either trace is rewritten afterwards. This
/// test checks the committed baseline values against those sequences.
#[test]
fn generated_values_in_the_baseline_trace_follow_the_documented_sequences() {
    let trace = parse_json(COMMITTED_TRACE).expect("baseline trace is JSON");
    let steps = match member(member(&trace, "scenarios"), "approve-disclose-once") {
        Json::Array(steps) => steps,
        other => panic!("scenario steps expected, got {other:?}"),
    };
    let started = parse_json(text(member(&steps[0], "body"))).expect("start body");
    let first_draw = deterministic_bytes()(32);
    assert_eq!(
        text(member(&started, "deviceCode")),
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(first_draw)
    );
    // The clock starts at 2026-08-06T12:00:00.000Z and a request lives ten minutes.
    assert_eq!(
        text(member(&started, "expiresAt")),
        "2026-08-06T12:10:00.000Z"
    );
    assert_eq!(counter_ids()(), "00000000-0000-4000-8000-000000000001");
    assert!(
        member(
            member(member(&trace, "cases"), "request-id/success/absent"),
            "headers"
        )
        .get("x-request-id")
            == Some(&Json::string("00000000-0000-4000-8000-000000000001"))
    );
}
