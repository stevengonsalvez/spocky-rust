//! The public operation manifest (`src/public-api/operation-manifest.ts`).

use crate::ApiKeyScope;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationId {
    ListTriggers,
    ValidateTrigger,
    InstallTrigger,
    ListProjects,
    ListConfigurationResources,
    ListSetupResources,
    ValidateConfiguration,
    InstallConfiguration,
    DispatchManualRun,
    IssueEnrollmentToken,
}

impl OperationId {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ListTriggers => "listTriggers",
            Self::ValidateTrigger => "validateTrigger",
            Self::InstallTrigger => "installTrigger",
            Self::ListProjects => "listProjects",
            Self::ListConfigurationResources => "listConfigurationResources",
            Self::ListSetupResources => "listSetupResources",
            Self::ValidateConfiguration => "validateConfiguration",
            Self::InstallConfiguration => "installConfiguration",
            Self::DispatchManualRun => "dispatchManualRun",
            Self::IssueEnrollmentToken => "issueEnrollmentToken",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        MANIFEST
            .iter()
            .map(|definition| definition.id)
            .find(|id| id.as_str() == name)
    }
}

/// The request schema an operation reads, named after its component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestSchema {
    TriggerYaml,
    InstallConfiguration,
    DispatchManualRun,
}

impl RequestSchema {
    /// The `OpenAPI` component name.
    #[must_use]
    pub fn component(self) -> &'static str {
        match self {
            Self::TriggerYaml => "TriggerYamlRequest",
            Self::InstallConfiguration => "InstallConfigurationRequest",
            Self::DispatchManualRun => "DispatchManualRunRequest",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct OperationDefinition {
    pub id: OperationId,
    /// Lower case, as the manifest writes it.
    pub method: &'static str,
    pub path: &'static str,
    pub scope: ApiKeyScope,
    pub request: Option<RequestSchema>,
    /// The `OpenAPI` component name of the success schema.
    pub success_schema: &'static str,
    pub success_status: u16,
    pub result_mapping: &'static str,
    pub summary: &'static str,
    pub description: &'static str,
    pub tag: &'static str,
    /// Documented statuses in ascending order, with their descriptions.
    pub responses: &'static [(u16, &'static str)],
}

const UNAUTHORIZED: &str = "The bearer credential is missing, malformed, or revoked.";
const UNAVAILABLE: &str = "Hub authentication or storage is unavailable.";
const UNEXPECTED: &str = "The operation failed unexpectedly.";
const BAD_REQUEST_JSON: &str = "The JSON request is malformed.";
const BAD_REQUEST_FIELDS: &str = "The JSON request is malformed or has invalid fields.";
const PROJECT_MISSING: &str = "The project does not exist in the credential's organization.";
const INVALID_BUNDLE: &str =
    "The YAML, supplied prompt partial bundle, or Hub configuration is invalid.";

pub const MANIFEST: [OperationDefinition; 10] = [
    OperationDefinition {
        id: OperationId::ListTriggers,
        method: "get",
        path: "/api/v1/triggers",
        scope: ApiKeyScope::ConfigurationValidate,
        request: None,
        success_schema: "TriggerList",
        success_status: 200,
        result_mapping: "triggers",
        summary: "List triggers",
        description: "Lists active organization triggers with their deployable YAML documents.",
        tag: "Triggers",
        responses: &[
            (200, "The organization's active trigger documents."),
            (401, UNAUTHORIZED),
            (403, "The bearer credential lacks configuration:validate."),
            (500, UNEXPECTED),
            (503, UNAVAILABLE),
        ],
    },
    OperationDefinition {
        id: OperationId::ValidateTrigger,
        method: "post",
        path: "/api/v1/triggers/validate",
        scope: ApiKeyScope::ConfigurationValidate,
        request: Some(RequestSchema::TriggerYaml),
        success_schema: "ValidatedTrigger",
        success_status: 200,
        result_mapping: "trigger-validation",
        summary: "Validate one trigger",
        description: "Validates one self-contained trigger against organization resources.",
        tag: "Triggers",
        responses: &[
            (200, "The trigger is valid."),
            (400, BAD_REQUEST_JSON),
            (401, UNAUTHORIZED),
            (403, "The bearer credential lacks configuration:validate."),
            (
                422,
                "The trigger YAML or referenced organization resource is invalid.",
            ),
            (503, UNAVAILABLE),
        ],
    },
    OperationDefinition {
        id: OperationId::InstallTrigger,
        method: "post",
        path: "/api/v1/triggers/install",
        scope: ApiKeyScope::ConfigurationInstall,
        request: Some(RequestSchema::TriggerYaml),
        success_schema: "InstalledTrigger",
        success_status: 201,
        result_mapping: "trigger-installation",
        summary: "Install one trigger",
        description: "Creates or replaces an organization trigger by its YAML name.",
        tag: "Triggers",
        responses: &[
            (201, "The trigger revision is active."),
            (400, BAD_REQUEST_JSON),
            (401, UNAUTHORIZED),
            (403, "The bearer credential lacks configuration:install."),
            (
                422,
                "The trigger YAML or referenced organization resource is invalid.",
            ),
            (503, UNAVAILABLE),
        ],
    },
    OperationDefinition {
        id: OperationId::ListProjects,
        method: "get",
        path: "/api/v1/projects",
        scope: ApiKeyScope::ProjectsRead,
        request: None,
        success_schema: "ProjectList",
        success_status: 200,
        result_mapping: "projects",
        summary: "List projects",
        description: "Lists active projects in the authenticated organization.",
        tag: "Projects",
        responses: &[
            (200, "The organization's active projects."),
            (401, UNAUTHORIZED),
            (403, "The bearer credential lacks projects:read."),
            (500, UNEXPECTED),
            (503, UNAVAILABLE),
        ],
    },
    OperationDefinition {
        id: OperationId::ListConfigurationResources,
        method: "get",
        path: "/api/v1/configuration-resources",
        scope: ApiKeyScope::ConfigurationValidate,
        request: None,
        success_schema: "ConfigurationResources",
        success_status: 200,
        result_mapping: "configuration-resources",
        summary: "List configuration resources",
        description: "Lists organization daemon and provider slugs that configuration validation resolves.",
        tag: "Configurations",
        responses: &[
            (200, "The organization's configuration resources."),
            (401, UNAUTHORIZED),
            (403, "The bearer credential lacks configuration:validate."),
            (500, UNEXPECTED),
            (503, UNAVAILABLE),
        ],
    },
    OperationDefinition {
        id: OperationId::ListSetupResources,
        method: "get",
        path: "/api/v1/setup-resources",
        scope: ApiKeyScope::ConfigurationValidate,
        request: None,
        success_schema: "SetupResources",
        success_status: 200,
        result_mapping: "setup-resources",
        summary: "List setup resources",
        description: "Lists provider-native identifiers and labels needed to author starter workflow filters.",
        tag: "Configurations",
        responses: &[
            (200, "The organization's setup resources."),
            (401, UNAUTHORIZED),
            (403, "The bearer credential lacks configuration:validate."),
            (500, UNEXPECTED),
            (503, UNAVAILABLE),
        ],
    },
    OperationDefinition {
        id: OperationId::ValidateConfiguration,
        method: "post",
        path: "/api/v1/configurations/validate",
        scope: ApiKeyScope::ConfigurationValidate,
        request: Some(RequestSchema::InstallConfiguration),
        success_schema: "ValidatedConfiguration",
        success_status: 200,
        result_mapping: "validation",
        summary: "Validate configuration",
        description: "Resolves the deployment project and validates the same YAML, prompt-partial bundle, daemon, and provider resources as installation without creating a project, recording a revision, or changing active configuration.",
        tag: "Configurations",
        responses: &[
            (200, "The configuration is valid for the project."),
            (400, BAD_REQUEST_FIELDS),
            (401, UNAUTHORIZED),
            (403, "The bearer credential lacks configuration:validate."),
            (404, PROJECT_MISSING),
            (422, INVALID_BUNDLE),
            (500, UNEXPECTED),
            (503, UNAVAILABLE),
        ],
    },
    OperationDefinition {
        id: OperationId::InstallConfiguration,
        method: "post",
        path: "/api/v1/configurations/install",
        scope: ApiKeyScope::ConfigurationInstall,
        request: Some(RequestSchema::InstallConfiguration),
        success_schema: "InstalledConfiguration",
        success_status: 201,
        result_mapping: "configuration",
        summary: "Install and activate configuration",
        description: "Resolves or creates the deployment project, validates the complete canonical Hub bundle, records a configuration revision, and atomically activates it.",
        tag: "Configurations",
        responses: &[
            (201, "The new configuration revision is active."),
            (400, BAD_REQUEST_FIELDS),
            (401, UNAUTHORIZED),
            (403, "The bearer credential lacks configuration:install."),
            (404, PROJECT_MISSING),
            (422, INVALID_BUNDLE),
            (500, UNEXPECTED),
            (503, UNAVAILABLE),
        ],
    },
    OperationDefinition {
        id: OperationId::DispatchManualRun,
        method: "post",
        path: "/api/v1/manual-runs",
        scope: ApiKeyScope::RunsDispatch,
        request: Some(RequestSchema::DispatchManualRun),
        success_schema: "DispatchedManualRun",
        success_status: 200,
        result_mapping: "manual-run",
        summary: "Dispatch a manual run",
        description: "Uses deliveryKey as caller-supplied request identity in the existing durable manual-event path.",
        tag: "Runs",
        responses: &[
            (200, "The durable manual event resolved to a run."),
            (400, "The JSON request or trigger input is invalid."),
            (401, UNAUTHORIZED),
            (
                403,
                "The bearer credential lacks runs:dispatch or the actor is forbidden.",
            ),
            (
                404,
                "The project, configuration, or manual trigger does not exist.",
            ),
            (
                409,
                "The existing manual event path could not resolve a run.",
            ),
            (500, UNEXPECTED),
            (503, UNAVAILABLE),
        ],
    },
    OperationDefinition {
        id: OperationId::IssueEnrollmentToken,
        method: "post",
        path: "/api/v1/daemons/enrollment-tokens",
        scope: ApiKeyScope::DaemonsEnroll,
        request: None,
        success_schema: "EnrollmentToken",
        success_status: 201,
        result_mapping: "enrollment-token",
        summary: "Issue a daemon enrollment token",
        description: "Returns a short-lived, single-use token for enrolling one daemon.",
        tag: "Daemons",
        responses: &[
            (201, "A short-lived enrollment token was issued."),
            (401, UNAUTHORIZED),
            (403, "The bearer credential lacks daemons:enroll."),
            (500, UNEXPECTED),
            (503, UNAVAILABLE),
        ],
    },
];

/// The manifest entry of an operation id.
///
/// # Panics
///
/// Never: every [`OperationId`] has an entry.
#[must_use]
pub fn definition(id: OperationId) -> &'static OperationDefinition {
    MANIFEST
        .iter()
        .find(|definition| definition.id == id)
        .expect("every operation id is in the manifest")
}
