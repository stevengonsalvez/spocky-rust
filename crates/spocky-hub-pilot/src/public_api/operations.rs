//! The operation and authentication seams of the public API boundary.
//!
//! These mirror `src/public-operations/types.ts` and `src/auth/operation-auth.ts` in the pinned
//! Hub. The boundary owns routing, validation and response mapping; the operations behind it are
//! injected, as they are in the baseline's own tests.

use std::fmt;

use super::json::Json;
use super::message::Headers;
use super::validation::Issue;
use crate::ApiKeyScope;

/// Every scope, in the order the Hub declares them.
pub const ALL_SCOPES: [ApiKeyScope; 5] = [
    ApiKeyScope::ProjectsRead,
    ApiKeyScope::ConfigurationValidate,
    ApiKeyScope::ConfigurationInstall,
    ApiKeyScope::RunsDispatch,
    ApiKeyScope::DaemonsEnroll,
];

/// The wire name of a scope, as it appears in documents and problem details.
#[must_use]
pub fn scope_name(scope: ApiKeyScope) -> &'static str {
    match scope {
        ApiKeyScope::ProjectsRead => "projects:read",
        ApiKeyScope::ConfigurationValidate => "configuration:validate",
        ApiKeyScope::ConfigurationInstall => "configuration:install",
        ApiKeyScope::RunsDispatch => "runs:dispatch",
        ApiKeyScope::DaemonsEnroll => "daemons:enroll",
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialKind {
    ApiKey,
    CliCredential,
}

impl CredentialKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "apiKey",
            Self::CliCredential => "cliCredential",
        }
    }
}

/// The credential an operation runs for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicAuthorization {
    pub kind: CredentialKind,
    pub credential_id: String,
    pub organization_id: String,
    pub scopes: Vec<ApiKeyScope>,
}

impl PublicAuthorization {
    #[must_use]
    pub fn to_json(&self) -> Json {
        Json::object([
            ("kind", Json::string(self.kind.as_str())),
            ("credentialId", Json::string(&self.credential_id)),
            ("organizationId", Json::string(&self.organization_id)),
            (
                "scopes",
                Json::Array(
                    self.scopes
                        .iter()
                        .map(|scope| Json::string(scope_name(*scope)))
                        .collect(),
                ),
            ),
        ])
    }
}

/// How an operation or an authenticator failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OperationError {
    /// `DatabaseUnavailableError` in the baseline.
    DatabaseUnavailable,
    /// Any other thrown error; its text never reaches a response.
    Failed(String),
}

impl fmt::Display for OperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DatabaseUnavailable => formatter.write_str("database unavailable"),
            Self::Failed(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for OperationError {}

/// `OperationAuthorizationResult`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthorizationOutcome {
    Authorized(PublicAuthorization),
    Unauthorized,
    Forbidden,
}

pub trait OperationAuthenticator {
    /// # Errors
    ///
    /// Returns [`OperationError::DatabaseUnavailable`] when credential storage is unavailable.
    fn authorize(
        &mut self,
        headers: &Headers,
        required_scope: ApiKeyScope,
    ) -> Result<AuthorizationOutcome, OperationError>;
}

/// A domain issue returned by an operation; same shape as a schema issue.
pub type DomainIssue = Issue;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicProject {
    pub id: String,
    pub name: String,
    pub slug: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TriggerFormat {
    SingleRun,
    LegacyMultistep,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicTrigger {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub format: TriggerFormat,
    pub yaml: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GithubResource {
    pub slug: String,
    pub account_login: String,
    pub account_type: String,
    pub repositories: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigurationResources {
    /// Daemon id and slug.
    pub daemons: Vec<(String, String)>,
    pub github: Vec<GithubResource>,
    /// Slug and guild name.
    pub discord: Vec<(String, String)>,
    /// Slug and team name.
    pub slack: Vec<(String, String)>,
    /// Slug and organization name.
    pub linear: Vec<(String, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupResources {
    pub github: Vec<GithubResource>,
    /// Guild id and guild name.
    pub discord: Vec<(String, String)>,
    /// Team id and team name.
    pub slack: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowStatus {
    Running,
    Succeeded,
    Failed,
    TimedOut,
}

impl WorkflowStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListTriggersResult {
    Listed(Vec<PublicTrigger>),
    InfrastructureUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidateTriggerResult {
    Valid { name: String },
    InvalidTrigger(Vec<DomainIssue>),
    InfrastructureUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallTriggerResult {
    Installed {
        trigger_id: String,
        name: String,
        revision_id: String,
        version: i64,
    },
    InvalidTrigger(Vec<DomainIssue>),
    InfrastructureUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListProjectsResult {
    Listed(Vec<PublicProject>),
    InfrastructureUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListConfigurationResourcesResult {
    Listed(ConfigurationResources),
    InfrastructureUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListSetupResourcesResult {
    Listed(SetupResources),
    InfrastructureUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidateConfigurationResult {
    Valid {
        project_slug: String,
        would_create_project: bool,
    },
    ProjectNotFound,
    InvalidBundle(Vec<DomainIssue>),
    InvalidConfiguration(Vec<DomainIssue>),
    InfrastructureUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallConfigurationResult {
    /// `active` is carried through so a result that is not active fails the response schema.
    Installed {
        project_slug: String,
        version_id: String,
        version: i64,
        active: bool,
    },
    ProjectNotFound,
    InvalidBundle(Vec<DomainIssue>),
    InvalidConfiguration {
        version_id: String,
        issues: Vec<DomainIssue>,
    },
    InfrastructureUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DispatchManualRunResult {
    Dispatched {
        delivery_key: String,
        provider_event_receipt_id: String,
        trigger_run_id: String,
        configured_trigger_name: String,
        workflow_status: WorkflowStatus,
    },
    ProjectNotFound,
    ActorForbidden,
    DaemonOffline,
    ExpectedConfigurationNotCurrent,
    ConfigurationNotFound,
    TriggerNotFound,
    InvalidInput {
        trigger_run_id: String,
        issues: Vec<DomainIssue>,
    },
    DispatchConflict,
    InfrastructureUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IssueEnrollmentTokenResult {
    Issued {
        token: String,
        /// Epoch milliseconds, as `Date#getTime`.
        expires_at_ms: i64,
    },
    CredentialRevoked,
    InfrastructureUnavailable,
}

pub use super::contracts::{DispatchManualRunInput, InstallConfigurationInput, TriggerYamlInput};

pub trait PublicOperations {
    /// # Errors
    ///
    /// Every operation reports unexpected failures as [`OperationError`].
    fn list_triggers(
        &mut self,
        authorization: &PublicAuthorization,
    ) -> Result<ListTriggersResult, OperationError>;
    fn validate_trigger(
        &mut self,
        authorization: &PublicAuthorization,
        input: &TriggerYamlInput,
    ) -> Result<ValidateTriggerResult, OperationError>;
    fn install_trigger(
        &mut self,
        authorization: &PublicAuthorization,
        input: &TriggerYamlInput,
    ) -> Result<InstallTriggerResult, OperationError>;
    fn list_projects(
        &mut self,
        authorization: &PublicAuthorization,
    ) -> Result<ListProjectsResult, OperationError>;
    fn list_configuration_resources(
        &mut self,
        authorization: &PublicAuthorization,
    ) -> Result<ListConfigurationResourcesResult, OperationError>;
    fn list_setup_resources(
        &mut self,
        authorization: &PublicAuthorization,
    ) -> Result<ListSetupResourcesResult, OperationError>;
    fn validate_configuration(
        &mut self,
        authorization: &PublicAuthorization,
        input: &InstallConfigurationInput,
    ) -> Result<ValidateConfigurationResult, OperationError>;
    fn install_configuration(
        &mut self,
        authorization: &PublicAuthorization,
        input: &InstallConfigurationInput,
    ) -> Result<InstallConfigurationResult, OperationError>;
    fn dispatch_manual_run(
        &mut self,
        authorization: &PublicAuthorization,
        input: &DispatchManualRunInput,
    ) -> Result<DispatchManualRunResult, OperationError>;
    fn issue_enrollment_token(
        &mut self,
        authorization: &PublicAuthorization,
    ) -> Result<IssueEnrollmentTokenResult, OperationError>;
}
