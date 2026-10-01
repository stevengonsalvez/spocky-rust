//! Scoped organization public API, CLI device authorization and the `OpenAPI` document.
//!
//! Each behavior mirrors the pinned Hub baseline; see `evidence/phase2/hub-api-enumeration.md` for
//! the covered surface and the remaining gaps.

mod api;
mod cli_authorizations;
mod contracts;
mod credentials;
mod manifest;
mod message;
mod openapi;
mod operations;
mod validation;
mod value;

pub use api::{Composition, IdSource, OperationsRequired, PublicApi, random_ids};
pub use cli_authorizations::{
    AccessFailure, AuthorizationStatus, BrowserAccess, CLIENT_ADDRESS_HEADER,
    CliAuthorizationRecord, CliAuthorizationStore, CliAuthorizations, Clock, CredentialInput,
    DecisionAccess, DecisionOutcome, HandlerError, MemoryCliAuthorizations, OrganizationAccess,
    PollOutcome, RandomBytes, StartInput, UuidSource, normalize_user_code, os_random_bytes,
};
pub use contracts::{
    BundleFile, DecisionBody, DispatchManualRunInput, InstallConfigurationInput, TriggerYamlInput,
    iso_string, parse_decision, parse_device_code, parse_dispatch_manual_run,
    parse_install_configuration, parse_trigger_yaml, parse_user_code,
};
pub use credentials::{
    ApiKeyAuthorizer, CLI_CREDENTIAL_PREFIX, CliCredentialRecord, CliCredentialStore,
    PublicCredentialAuthenticator, cli_credential_parts, hash_secret,
};
pub use manifest::{MANIFEST, OperationDefinition, OperationId, RequestSchema, definition};
pub use message::{ApiRequest, ApiResponse, Headers, InvalidUrl};
pub use openapi::{document, document_text};
pub use operations::{
    ALL_SCOPES, AuthorizationOutcome, ConfigurationResources, CredentialKind,
    DispatchManualRunResult, DomainIssue, GithubResource, InstallConfigurationResult,
    InstallTriggerResult, IssueEnrollmentTokenResult, ListConfigurationResourcesResult,
    ListProjectsResult, ListSetupResourcesResult, ListTriggersResult, OperationAuthenticator,
    OperationError, PublicAuthorization, PublicOperations, PublicProject, PublicTrigger,
    SetupResources, TriggerFormat, ValidateConfigurationResult, ValidateTriggerResult,
    WorkflowStatus, scope_name,
};
pub use spocky_contracts::js_value::{JsObject, JsValue, parse as parse_json, stringify_pretty};
pub use validation::{Issue, PathPart};
pub use value::{JsValueExt, Json, decode_request_json};
