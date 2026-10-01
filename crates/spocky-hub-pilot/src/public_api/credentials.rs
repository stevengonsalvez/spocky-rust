//! Public credential authentication (`src/auth/public-credentials.ts`, `cli-credentials.ts`).
//!
//! API keys are authorized by the existing [`crate::HubPilot`] key boundary; CLI credentials are
//! checked here against a [`CliCredentialStore`].

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq as _;

use super::message::Headers;
use super::operations::{
    ALL_SCOPES, AuthorizationOutcome, CredentialKind, OperationAuthenticator, OperationError,
    PublicAuthorization,
};
use crate::{ApiKeyAuthorization, ApiKeyScope, DurableHubStore, HubPilot};

pub const CLI_CREDENTIAL_PREFIX: &str = "paseo_cli_";
const CLI_CREDENTIAL_PREFIX_LENGTH: usize = 12;
const MAX_TOKEN_LENGTH: usize = 200;

/// A stored CLI credential. `verifier` is the base64url SHA-256 of the whole token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliCredentialRecord {
    pub id: String,
    pub organization_id: String,
    pub verifier: String,
    pub revoked: bool,
}

pub trait CliCredentialStore {
    fn find_by_prefix(&self, prefix: &str) -> Option<CliCredentialRecord>;
    /// Records use of a credential; false when it was revoked in the meantime.
    fn touch(&self, id: &str) -> bool;
}

impl<T: CliCredentialStore + ?Sized> CliCredentialStore for std::rc::Rc<T> {
    fn find_by_prefix(&self, prefix: &str) -> Option<CliCredentialRecord> {
        (**self).find_by_prefix(prefix)
    }

    fn touch(&self, id: &str) -> bool {
        (**self).touch(id)
    }
}

/// Authorizes API keys, as `OrganizationApiKeys.authorize` does.
pub trait ApiKeyAuthorizer {
    /// The scopes of a key in creation order, as the baseline returns them; `None` falls back to
    /// the order of the authorization result.
    fn scope_order(&self, _credential_id: &str) -> Option<Vec<ApiKeyScope>> {
        None
    }

    /// # Errors
    ///
    /// Returns [`OperationError`] when key storage fails.
    fn authorize_api_key(
        &mut self,
        authorization: &str,
        required_scope: ApiKeyScope,
    ) -> Result<ApiKeyAuthorization, OperationError>;
}

impl<S: DurableHubStore> ApiKeyAuthorizer for HubPilot<S> {
    fn scope_order(&self, credential_id: &str) -> Option<Vec<ApiKeyScope>> {
        self.api_key_scope_order(credential_id)
    }

    fn authorize_api_key(
        &mut self,
        authorization: &str,
        required_scope: ApiKeyScope,
    ) -> Result<ApiKeyAuthorization, OperationError> {
        HubPilot::authorize_api_key(self, authorization, required_scope)
            .map_err(|error| OperationError::Failed(error.to_string()))
    }
}

/// `PublicCredentialAuthenticator`: CLI credentials by prefix, every other bearer as an API key.
pub struct PublicCredentialAuthenticator<A, C> {
    api_keys: A,
    cli_credentials: C,
}

impl<A: ApiKeyAuthorizer, C: CliCredentialStore> PublicCredentialAuthenticator<A, C> {
    pub fn new(api_keys: A, cli_credentials: C) -> Self {
        Self {
            api_keys,
            cli_credentials,
        }
    }
}

impl<A: ApiKeyAuthorizer, C: CliCredentialStore> OperationAuthenticator
    for PublicCredentialAuthenticator<A, C>
{
    fn authorize(
        &mut self,
        headers: &Headers,
        required_scope: ApiKeyScope,
    ) -> Result<AuthorizationOutcome, OperationError> {
        let authorization = headers.get("authorization");
        if authorization
            .as_deref()
            .is_some_and(|value| value.starts_with(&format!("Bearer {CLI_CREDENTIAL_PREFIX}")))
        {
            return Ok(authorize_cli_credential(
                &self.cli_credentials,
                authorization.as_deref(),
            ));
        }
        Ok(
            match self
                .api_keys
                .authorize_api_key(authorization.as_deref().unwrap_or(""), required_scope)?
            {
                ApiKeyAuthorization::Unauthorized => AuthorizationOutcome::Unauthorized,
                ApiKeyAuthorization::Forbidden => AuthorizationOutcome::Forbidden,
                ApiKeyAuthorization::Authorized(access) => {
                    let scopes = self
                        .api_keys
                        .scope_order(&access.credential_id)
                        .unwrap_or_else(|| access.scopes.iter().copied().collect());
                    AuthorizationOutcome::Authorized(PublicAuthorization {
                        kind: CredentialKind::ApiKey,
                        credential_id: access.credential_id,
                        organization_id: access.organization.as_str().to_owned(),
                        scopes,
                    })
                }
            },
        )
    }
}

/// `Bearer ` followed by 1 to 200 characters.
fn bearer_token(value: Option<&str>) -> Option<&str> {
    let token = value?.strip_prefix("Bearer ")?;
    (!token.is_empty() && token.chars().count() <= MAX_TOKEN_LENGTH).then_some(token)
}

/// The public prefix of a CLI credential: `paseo_cli_`, 12 URL-safe characters, then `_` and at
/// least one more character.
fn parse_cli_credential(token: &str) -> Option<&str> {
    let separator = CLI_CREDENTIAL_PREFIX.len() + CLI_CREDENTIAL_PREFIX_LENGTH;
    let bytes = token.as_bytes();
    if bytes.len() <= separator + 1 || bytes[separator] != b'_' {
        return None;
    }
    let prefix = token.get(..separator)?;
    let random = prefix.strip_prefix(CLI_CREDENTIAL_PREFIX)?;
    random
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        .then_some(prefix)
}

#[must_use]
pub fn hash_secret(secret: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()))
}

fn secret_matches(secret: &str, expected_hash: &str) -> bool {
    let actual = Sha256::digest(secret.as_bytes());
    let Ok(expected) = URL_SAFE_NO_PAD.decode(expected_hash) else {
        return false;
    };
    actual.len() == expected.len() && bool::from(actual.as_slice().ct_eq(&expected))
}

fn authorize_cli_credential(
    store: &dyn CliCredentialStore,
    authorization: Option<&str>,
) -> AuthorizationOutcome {
    let Some(token) = bearer_token(authorization) else {
        return AuthorizationOutcome::Unauthorized;
    };
    let Some(prefix) = parse_cli_credential(token) else {
        return AuthorizationOutcome::Unauthorized;
    };
    let Some(record) = store.find_by_prefix(prefix) else {
        return AuthorizationOutcome::Unauthorized;
    };
    if record.revoked || !secret_matches(token, &record.verifier) {
        return AuthorizationOutcome::Unauthorized;
    }
    if !store.touch(&record.id) {
        return AuthorizationOutcome::Unauthorized;
    }
    AuthorizationOutcome::Authorized(PublicAuthorization {
        kind: CredentialKind::CliCredential,
        credential_id: record.id,
        organization_id: record.organization_id,
        scopes: ALL_SCOPES.to_vec(),
    })
}

/// A `(prefix, verifier)` pair for a CLI credential (`cliCredentialParts`).
///
/// # Errors
///
/// Returns the baseline's message when the credential has no secret separator.
pub fn cli_credential_parts(token: &str) -> Result<(String, String), &'static str> {
    let separator = CLI_CREDENTIAL_PREFIX.len() + CLI_CREDENTIAL_PREFIX_LENGTH;
    if token.as_bytes().get(separator) != Some(&b'_') {
        return Err("CLI credential has no secret separator");
    }
    let prefix = token.get(..separator).unwrap_or(token);
    Ok((prefix.to_owned(), hash_secret(token)))
}
