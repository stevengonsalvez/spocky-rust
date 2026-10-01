//! Routing, authentication, validation and response mapping of the scoped organization API
//! (`src/public-api/index.ts`).

use std::fmt;

use super::contracts::{
    configuration_resources_body, dispatched_run_body, enrollment_token_body,
    installed_configuration_body, installed_trigger_body, parse_dispatch_manual_run,
    parse_install_configuration, parse_trigger_yaml, projects_body, setup_resources_body,
    triggers_body, validated_configuration_body, validated_trigger_body,
};
use super::json::{Json, decode_request_json};
use super::manifest::{MANIFEST, OperationDefinition, OperationId, RequestSchema, definition};
use super::message::{ApiRequest, ApiResponse};
use super::openapi::document_text;
use super::operations::{
    AuthorizationOutcome, DispatchManualRunResult, InstallConfigurationResult,
    InstallTriggerResult, IssueEnrollmentTokenResult, ListConfigurationResourcesResult,
    ListProjectsResult, ListSetupResourcesResult, ListTriggersResult, OperationAuthenticator,
    OperationError, PublicAuthorization, PublicOperations, ValidateConfigurationResult,
    ValidateTriggerResult, scope_name,
};
use super::validation::{Issue, js_trim};

/// `PublicApiComposition`.
pub enum Composition {
    Enabled(Box<dyn OperationAuthenticator>),
    Unavailable,
}

/// Raised by [`PublicApi::new`] for an enabled composition without operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationsRequired;

impl fmt::Display for OperationsRequired {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("enabled public API requires application operations")
    }
}

impl std::error::Error for OperationsRequired {}

/// Source of generated request identifiers (`randomUUID`).
pub type IdSource = Box<dyn FnMut() -> String>;

/// Random version 4 UUIDs, the production identifier source.
#[must_use]
pub fn random_ids() -> IdSource {
    Box::new(|| uuid::Uuid::new_v4().to_string())
}

pub struct PublicApi {
    composition: Composition,
    operations: Option<Box<dyn PublicOperations>>,
    ids: IdSource,
}

impl PublicApi {
    /// # Errors
    ///
    /// Returns [`OperationsRequired`] when the composition is enabled and `operations` is `None`.
    pub fn new(
        composition: Composition,
        operations: Option<Box<dyn PublicOperations>>,
        ids: IdSource,
    ) -> Result<Self, OperationsRequired> {
        if matches!(composition, Composition::Enabled(_)) && operations.is_none() {
            return Err(OperationsRequired);
        }
        Ok(Self {
            composition,
            operations,
            ids,
        })
    }

    /// Routes by path and method, then runs the operation.
    pub fn handle(&mut self, request: &ApiRequest) -> ApiResponse {
        let request_id = self.request_id(request);
        let path = request.url.path();
        let path_routes: Vec<&OperationDefinition> =
            MANIFEST.iter().filter(|route| route.path == path).collect();
        if path_routes.is_empty() {
            return problem(
                &request_id,
                404,
                "not_found",
                "Not found",
                "No canonical API route matches this path.",
                None,
            );
        }
        let Some(route) = path_routes
            .iter()
            .find(|route| route.method.eq_ignore_ascii_case(&request.method))
        else {
            let mut response = problem(
                &request_id,
                405,
                "method_not_allowed",
                "Method not allowed",
                "Use one of the methods listed in the Allow response header.",
                None,
            );
            let allow: Vec<String> = path_routes
                .iter()
                .map(|route| route.method.to_ascii_uppercase())
                .collect();
            response.headers.set("allow", &allow.join(", "));
            return response;
        };
        self.execute_safely(route.id, request, &request_id)
    }

    /// Runs one operation without routing on the path or method.
    pub fn handle_operation(&mut self, id: OperationId, request: &ApiRequest) -> ApiResponse {
        let request_id = self.request_id(request);
        self.execute_safely(id, request, &request_id)
    }

    /// `GET /api/openapi.json`.
    #[must_use]
    pub fn openapi(&self) -> ApiResponse {
        ApiResponse::json(
            200,
            &document_text(),
            &[("cache-control", "public, max-age=300")],
        )
    }

    fn request_id(&mut self, request: &ApiRequest) -> String {
        request
            .headers
            .get("x-request-id")
            .map(|value| js_trim(&value).to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| (self.ids)())
    }

    fn execute_safely(
        &mut self,
        id: OperationId,
        request: &ApiRequest,
        request_id: &str,
    ) -> ApiResponse {
        let Self {
            composition,
            operations,
            ..
        } = self;
        let (Composition::Enabled(authenticator), Some(operations)) = (composition, operations)
        else {
            return problem(
                request_id,
                503,
                "infrastructure_unavailable",
                "Service unavailable",
                "Public API authentication or storage is currently unavailable.",
                None,
            );
        };
        match execute(
            id,
            request,
            request_id,
            authenticator.as_mut(),
            operations.as_mut(),
        ) {
            Ok(response) => response,
            Err(OperationError::DatabaseUnavailable) => infrastructure_problem(request_id),
            Err(OperationError::Failed(_)) => problem(
                request_id,
                500,
                "internal_error",
                "Internal server error",
                "The operation failed unexpectedly. Contact the Hub operator with the request ID.",
                None,
            ),
        }
    }
}

fn read_json(request: &ApiRequest) -> Option<Json> {
    let content_type = request.headers.get("content-type")?;
    if !content_type.to_lowercase().contains("application/json") {
        return None;
    }
    decode_request_json(&request.body)
}

fn execute(
    id: OperationId,
    request: &ApiRequest,
    request_id: &str,
    authenticator: &mut dyn OperationAuthenticator,
    operations: &mut dyn PublicOperations,
) -> Result<ApiResponse, OperationError> {
    let route = definition(id);
    let scope = route.scope;
    let outcome = match authenticator.authorize(&request.headers, scope) {
        Ok(outcome) => outcome,
        Err(OperationError::DatabaseUnavailable) => {
            return Ok(problem(
                request_id,
                503,
                "authentication_unavailable",
                "Authentication unavailable",
                "Bearer-credential authentication is currently unavailable. Retry the request later.",
                None,
            ));
        }
        Err(error) => return Err(error),
    };
    let access = match outcome {
        AuthorizationOutcome::Unauthorized => {
            return Ok(problem(
                request_id,
                401,
                "unauthorized",
                "Authentication required",
                "Provide an active Paseo organization credential in the Authorization: Bearer header.",
                None,
            ));
        }
        AuthorizationOutcome::Forbidden => {
            return Ok(problem(
                request_id,
                403,
                "insufficient_scope",
                "Insufficient scope",
                &format!("This operation requires the {} scope.", scope_name(scope)),
                None,
            ));
        }
        AuthorizationOutcome::Authorized(access) => access,
    };
    invoke(route, request, request_id, &access, operations)
}

fn invoke(
    route: &OperationDefinition,
    request: &ApiRequest,
    request_id: &str,
    access: &PublicAuthorization,
    operations: &mut dyn PublicOperations,
) -> Result<ApiResponse, OperationError> {
    let body = match route.request {
        None => None,
        Some(schema) => {
            let Some(value) = read_json(request) else {
                return Ok(problem(
                    request_id,
                    400,
                    "invalid_json",
                    "Invalid JSON",
                    "Send a JSON request body using Content-Type: application/json.",
                    None,
                ));
            };
            Some((schema, value))
        }
    };
    match route.id {
        OperationId::ListTriggers => {
            let result = operations.list_triggers(access)?;
            triggers_response(request_id, &result)
        }
        OperationId::ListProjects => {
            let result = operations.list_projects(access)?;
            projects_response(request_id, &result)
        }
        OperationId::ListConfigurationResources => {
            let result = operations.list_configuration_resources(access)?;
            configuration_resources_response(request_id, &result)
        }
        OperationId::ListSetupResources => {
            let result = operations.list_setup_resources(access)?;
            Ok(setup_resources_response(request_id, &result))
        }
        OperationId::IssueEnrollmentToken => {
            let result = operations.issue_enrollment_token(access)?;
            enrollment_response(request_id, &result)
        }
        OperationId::ValidateTrigger | OperationId::InstallTrigger => {
            let Some((schema, value)) = body else {
                return Err(OperationError::Failed("missing request schema".to_owned()));
            };
            debug_assert_eq!(schema, RequestSchema::TriggerYaml);
            let input = match parse_trigger_yaml(&value) {
                Ok(input) => input,
                Err(issues) => return Ok(validation_problem(request_id, &issues)),
            };
            if route.id == OperationId::ValidateTrigger {
                let result = operations.validate_trigger(access, &input)?;
                Ok(trigger_validation_response(request_id, result))
            } else {
                let result = operations.install_trigger(access, &input)?;
                trigger_installation_response(request_id, result)
            }
        }
        OperationId::ValidateConfiguration | OperationId::InstallConfiguration => {
            let Some((_, value)) = body else {
                return Err(OperationError::Failed("missing request schema".to_owned()));
            };
            let input = match parse_install_configuration(&value) {
                Ok(input) => input,
                Err(issues) => return Ok(validation_problem(request_id, &issues)),
            };
            if route.id == OperationId::ValidateConfiguration {
                let result = operations.validate_configuration(access, &input)?;
                Ok(validation_response(request_id, result))
            } else {
                let result = operations.install_configuration(access, &input)?;
                installation_response(request_id, result)
            }
        }
        OperationId::DispatchManualRun => {
            let Some((_, value)) = body else {
                return Err(OperationError::Failed("missing request schema".to_owned()));
            };
            let input = match parse_dispatch_manual_run(&value) {
                Ok(input) => input,
                Err(issues) => return Ok(validation_problem(request_id, &issues)),
            };
            let result = operations.dispatch_manual_run(access, &input)?;
            manual_run_response(request_id, result)
        }
    }
}

fn invalid_result(what: &str) -> OperationError {
    OperationError::Failed(format!("invalid {what} operation result"))
}

fn success(request_id: &str, status: u16, body: &Json) -> ApiResponse {
    ApiResponse::json(status, &body.stringify(), &[("x-request-id", request_id)])
}

fn triggers_response(
    request_id: &str,
    result: &ListTriggersResult,
) -> Result<ApiResponse, OperationError> {
    match result {
        ListTriggersResult::Listed(triggers) => {
            let body = triggers_body(triggers).ok_or_else(|| invalid_result("triggers"))?;
            Ok(success(request_id, 200, &body))
        }
        ListTriggersResult::InfrastructureUnavailable => Ok(infrastructure_problem(request_id)),
    }
}

fn trigger_validation_response(request_id: &str, result: ValidateTriggerResult) -> ApiResponse {
    match result {
        ValidateTriggerResult::Valid { name } => {
            success(request_id, 200, &validated_trigger_body(&name))
        }
        ValidateTriggerResult::InvalidTrigger(issues) => problem(
            request_id,
            422,
            "invalid_trigger",
            "Invalid trigger",
            "Correct the self-contained trigger YAML.",
            Some(&issues),
        ),
        ValidateTriggerResult::InfrastructureUnavailable => infrastructure_problem(request_id),
    }
}

fn trigger_installation_response(
    request_id: &str,
    result: InstallTriggerResult,
) -> Result<ApiResponse, OperationError> {
    match result {
        InstallTriggerResult::Installed {
            trigger_id,
            name,
            revision_id,
            version,
        } => {
            let body = installed_trigger_body(&trigger_id, &name, &revision_id, version)
                .ok_or_else(|| invalid_result("trigger installation"))?;
            Ok(success(request_id, 201, &body))
        }
        InstallTriggerResult::InvalidTrigger(issues) => Ok(problem(
            request_id,
            422,
            "invalid_trigger",
            "Invalid trigger",
            "Correct the self-contained trigger YAML and submit it again.",
            Some(&issues),
        )),
        InstallTriggerResult::InfrastructureUnavailable => Ok(infrastructure_problem(request_id)),
    }
}

fn projects_response(
    request_id: &str,
    result: &ListProjectsResult,
) -> Result<ApiResponse, OperationError> {
    match result {
        ListProjectsResult::Listed(projects) => {
            let body = projects_body(projects).ok_or_else(|| invalid_result("projects"))?;
            Ok(success(request_id, 200, &body))
        }
        ListProjectsResult::InfrastructureUnavailable => Ok(infrastructure_problem(request_id)),
    }
}

fn configuration_resources_response(
    request_id: &str,
    result: &ListConfigurationResourcesResult,
) -> Result<ApiResponse, OperationError> {
    match result {
        ListConfigurationResourcesResult::Listed(resources) => {
            let body = configuration_resources_body(resources)
                .ok_or_else(|| invalid_result("configuration resources"))?;
            Ok(success(request_id, 200, &body))
        }
        ListConfigurationResourcesResult::InfrastructureUnavailable => {
            Ok(infrastructure_problem(request_id))
        }
    }
}

fn setup_resources_response(request_id: &str, result: &ListSetupResourcesResult) -> ApiResponse {
    match result {
        ListSetupResourcesResult::Listed(resources) => {
            success(request_id, 200, &setup_resources_body(resources))
        }
        ListSetupResourcesResult::InfrastructureUnavailable => infrastructure_problem(request_id),
    }
}

fn project_not_found(request_id: &str) -> ApiResponse {
    problem(
        request_id,
        404,
        "project_not_found",
        "Project not found",
        "No active project with that slug exists in the credential's organization.",
        None,
    )
}

fn validation_response(request_id: &str, result: ValidateConfigurationResult) -> ApiResponse {
    match result {
        ValidateConfigurationResult::Valid {
            project_slug,
            would_create_project,
        } => success(
            request_id,
            200,
            &validated_configuration_body(&project_slug, would_create_project),
        ),
        ValidateConfigurationResult::ProjectNotFound => project_not_found(request_id),
        ValidateConfigurationResult::InvalidBundle(issues) => problem(
            request_id,
            422,
            "invalid_configuration_bundle",
            "Invalid configuration bundle",
            "Correct the canonical Hub bundle files.",
            Some(&issues),
        ),
        ValidateConfigurationResult::InvalidConfiguration(issues) => problem(
            request_id,
            422,
            "invalid_configuration",
            "Invalid configuration",
            "See issues for configuration errors.",
            Some(&issues),
        ),
        ValidateConfigurationResult::InfrastructureUnavailable => {
            infrastructure_problem(request_id)
        }
    }
}

fn installation_response(
    request_id: &str,
    result: InstallConfigurationResult,
) -> Result<ApiResponse, OperationError> {
    match result {
        InstallConfigurationResult::Installed {
            project_slug,
            version_id,
            version,
            active,
        } => {
            let body = installed_configuration_body(&project_slug, &version_id, version, active)
                .ok_or_else(|| invalid_result("configuration"))?;
            Ok(success(request_id, 201, &body))
        }
        InstallConfigurationResult::ProjectNotFound => Ok(project_not_found(request_id)),
        InstallConfigurationResult::InvalidBundle(issues) => Ok(problem(
            request_id,
            422,
            "invalid_configuration_bundle",
            "Invalid configuration bundle",
            "Correct the canonical Hub bundle files and submit them again.",
            Some(&issues),
        )),
        InstallConfigurationResult::InvalidConfiguration { version_id, issues } => Ok(problem(
            request_id,
            422,
            "invalid_configuration",
            "Invalid configuration",
            &format!("Configuration revision {version_id} was recorded but not activated."),
            Some(&issues),
        )),
        InstallConfigurationResult::InfrastructureUnavailable => {
            Ok(infrastructure_problem(request_id))
        }
    }
}

fn manual_run_response(
    request_id: &str,
    result: DispatchManualRunResult,
) -> Result<ApiResponse, OperationError> {
    let fail = |status, code, title, detail: &str| {
        Ok(problem(request_id, status, code, title, detail, None))
    };
    match result {
        DispatchManualRunResult::Dispatched {
            delivery_key,
            provider_event_receipt_id,
            trigger_run_id,
            configured_trigger_name,
            workflow_status,
        } => {
            let body = dispatched_run_body(
                &delivery_key,
                &provider_event_receipt_id,
                &trigger_run_id,
                &configured_trigger_name,
                workflow_status.as_str(),
            )
            .ok_or_else(|| invalid_result("manual-run"))?;
            Ok(success(request_id, 200, &body))
        }
        DispatchManualRunResult::ProjectNotFound => Ok(project_not_found(request_id)),
        DispatchManualRunResult::ActorForbidden => fail(
            403,
            "actor_forbidden",
            "Actor forbidden",
            "The configured manual trigger does not allow this actor.",
        ),
        DispatchManualRunResult::ConfigurationNotFound => fail(
            404,
            "configuration_not_found",
            "Configuration not found",
            "The requested configuration revision is not available.",
        ),
        DispatchManualRunResult::TriggerNotFound => fail(
            404,
            "trigger_not_found",
            "Trigger not found",
            "The active configuration has no matching manual trigger.",
        ),
        DispatchManualRunResult::ExpectedConfigurationNotCurrent => fail(
            409,
            "configuration_changed",
            "Configuration changed",
            "expectedVersionId is not the configuration version selected for this delivery.",
        ),
        DispatchManualRunResult::DaemonOffline => fail(
            409,
            "daemon_offline",
            "Daemon offline",
            "The selected daemon is not connected. Reconnect it before retrying.",
        ),
        DispatchManualRunResult::InvalidInput {
            trigger_run_id,
            issues,
        } => Ok(problem(
            request_id,
            400,
            "invalid_input",
            "Invalid trigger input",
            &format!("Run {trigger_run_id} rejected the submitted input."),
            Some(&issues),
        )),
        DispatchManualRunResult::DispatchConflict => fail(
            409,
            "dispatch_conflict",
            "Run not dispatched",
            "The durable event exists but no matching run is available yet. Retry with the same deliveryKey.",
        ),
        DispatchManualRunResult::InfrastructureUnavailable => {
            Ok(infrastructure_problem(request_id))
        }
    }
}

fn enrollment_response(
    request_id: &str,
    result: &IssueEnrollmentTokenResult,
) -> Result<ApiResponse, OperationError> {
    match result {
        IssueEnrollmentTokenResult::Issued {
            token,
            expires_at_ms,
        } => {
            let body = enrollment_token_body(token, *expires_at_ms)
                .ok_or_else(|| invalid_result("enrollment"))?;
            Ok(success(request_id, 201, &body))
        }
        IssueEnrollmentTokenResult::CredentialRevoked => Ok(problem(
            request_id,
            401,
            "unauthorized",
            "Authentication required",
            "The organization credential was revoked before the enrollment token could be issued.",
            None,
        )),
        IssueEnrollmentTokenResult::InfrastructureUnavailable => {
            Ok(infrastructure_problem(request_id))
        }
    }
}

fn infrastructure_problem(request_id: &str) -> ApiResponse {
    problem(
        request_id,
        503,
        "infrastructure_unavailable",
        "Service unavailable",
        "The operation could not reach durable storage. Retry the request later.",
        None,
    )
}

fn validation_problem(request_id: &str, issues: &[Issue]) -> ApiResponse {
    problem(
        request_id,
        400,
        "invalid_request",
        "Invalid request",
        "The request body contains invalid fields.",
        Some(issues),
    )
}

/// An RFC 9457 problem document: key order, content type and the 401 challenge header.
fn problem(
    request_id: &str,
    status: u16,
    code: &str,
    title: &str,
    detail: &str,
    issues: Option<&[Issue]>,
) -> ApiResponse {
    let mut fields = vec![
        (
            "type".to_owned(),
            Json::String(format!(
                "https://paseo.sh/problems/{}",
                code.replace('_', "-")
            )),
        ),
        ("title".to_owned(), Json::string(title)),
        ("status".to_owned(), Json::integer(i64::from(status))),
        ("detail".to_owned(), Json::string(detail)),
        ("code".to_owned(), Json::string(code)),
        ("requestId".to_owned(), Json::string(request_id)),
    ];
    if let Some(issues) = issues {
        fields.push((
            "issues".to_owned(),
            Json::Array(issues.iter().map(Issue::to_json).collect()),
        ));
    }
    let mut headers = vec![
        ("content-type", "application/problem+json"),
        ("x-request-id", request_id),
    ];
    if status == 401 {
        headers.push(("www-authenticate", "Bearer"));
    }
    ApiResponse::json(status, &Json::Object(fields).stringify(), &headers)
}
