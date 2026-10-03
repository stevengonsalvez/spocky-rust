//! Organization-scoped API-key boundary pilot.

use std::collections::BTreeSet;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spocky_contracts::text::{js_length, js_trim};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

use crate::{AccountId, DurableHubStore, HubError, HubPilot, OrganizationId};

const PREFIX_START: &str = "paseo_pk_";
const PREFIX_RANDOM_BYTES: usize = 9;
const PREFIX_RANDOM_LENGTH: usize = 12;
const SECRET_BYTES: usize = 32;
const MAX_TOKEN_LENGTH: usize = 200;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum ApiKeyScope {
    #[serde(rename = "projects:read")]
    ProjectsRead,
    #[serde(rename = "configuration:validate")]
    ConfigurationValidate,
    #[serde(rename = "configuration:install")]
    ConfigurationInstall,
    #[serde(rename = "runs:dispatch")]
    RunsDispatch,
    #[serde(rename = "daemons:enroll")]
    DaemonsEnroll,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiKeySummary {
    pub id: String,
    pub name: String,
    pub prefix: String,
    /// In creation order, as the baseline's `[...new Set(scopes)]` keeps them.
    pub scopes: Vec<ApiKeyScope>,
    pub created_at_epoch_seconds: u64,
    pub last_used_at_epoch_seconds: Option<u64>,
    pub revoked_at_epoch_seconds: Option<u64>,
    pub last_used: bool,
    pub revoked: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedApiKey {
    pub summary: ApiKeySummary,
    pub secret: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiKeyAccess {
    pub credential_id: String,
    pub organization: OrganizationId,
    pub scopes: BTreeSet<ApiKeyScope>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApiKeyAuthorization {
    Unauthorized,
    Forbidden,
    Authorized(ApiKeyAccess),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct StoredApiKey {
    organization: OrganizationId,
    name: String,
    prefix: String,
    verifier: [u8; 32],
    scopes: BTreeSet<ApiKeyScope>,
    /// The scopes in the order the key was created with them, duplicates removed, like the
    /// baseline's `[...new Set(scopes)]`. Keys stored before this field existed have none and
    /// report their scopes in declaration order.
    #[serde(default)]
    scope_order: Vec<ApiKeyScope>,
    sequence: u64,
    #[serde(default)]
    created_at_epoch_seconds: u64,
    #[serde(default)]
    last_used_at_epoch_seconds: Option<u64>,
    #[serde(default)]
    revoked_at_epoch_seconds: Option<u64>,
}

impl<S: DurableHubStore> HubPilot<S> {
    pub fn create_api_key(
        &mut self,
        actor: &AccountId,
        organization: &OrganizationId,
        name: &str,
        scopes: impl IntoIterator<Item = ApiKeyScope>,
    ) -> Result<CreatedApiKey, HubError> {
        if !self.authorize(actor, organization)?.can_manage_resources() {
            return Err(crate::AuthorityError::ManageResourcesRequired.into());
        }
        let name = js_trim(name);
        let mut scope_order = Vec::new();
        for scope in scopes {
            if !scope_order.contains(&scope) {
                scope_order.push(scope);
            }
        }
        let scopes = scope_order.iter().copied().collect::<BTreeSet<_>>();
        if name.is_empty() || js_length(name) > 100 || scopes.is_empty() || scopes.len() > 5 {
            return Err(HubError::InvalidApiKeyInput);
        }

        let mut prefix_bytes = [0_u8; PREFIX_RANDOM_BYTES];
        let mut secret_bytes = [0_u8; SECRET_BYTES];
        getrandom::fill(&mut prefix_bytes).map_err(|_| HubError::RandomUnavailable)?;
        getrandom::fill(&mut secret_bytes).map_err(|_| HubError::RandomUnavailable)?;
        let prefix = format!("{PREFIX_START}{}", URL_SAFE_NO_PAD.encode(prefix_bytes));
        let secret = format!("{prefix}_{}", URL_SAFE_NO_PAD.encode(secret_bytes));
        let id = Uuid::new_v4().to_string();
        let now = self.now_epoch_seconds();
        self.state.next_api_key_sequence += 1;
        let stored = StoredApiKey {
            organization: organization.clone(),
            name: name.to_owned(),
            prefix,
            verifier: Sha256::digest(secret.as_bytes()).into(),
            scopes,
            scope_order,
            sequence: self.state.next_api_key_sequence,
            created_at_epoch_seconds: now,
            last_used_at_epoch_seconds: None,
            revoked_at_epoch_seconds: None,
        };
        let summary = summary(&id, &stored);
        self.state.api_keys.insert(id.clone(), stored);
        if let Err(error) = self.persist() {
            self.state.api_keys.remove(&id);
            self.state.next_api_key_sequence -= 1;
            return Err(error);
        }
        Ok(CreatedApiKey { summary, secret })
    }

    pub fn list_api_keys(
        &self,
        actor: &AccountId,
        organization: &OrganizationId,
    ) -> Result<Vec<ApiKeySummary>, HubError> {
        if !self.authorize(actor, organization)?.can_manage_resources() {
            return Err(crate::AuthorityError::ManageResourcesRequired.into());
        }
        let mut keys = self
            .state
            .api_keys
            .iter()
            .filter(|(_, key)| &key.organization == organization)
            .map(|(id, key)| (key.sequence, summary(id, key)))
            .collect::<Vec<_>>();
        keys.sort_by(|left, right| right.0.cmp(&left.0));
        Ok(keys.into_iter().map(|(_, key)| key).collect())
    }

    pub fn authorize_api_key(
        &mut self,
        authorization: &str,
        required_scope: ApiKeyScope,
    ) -> Result<ApiKeyAuthorization, HubError> {
        let now = self.now_epoch_seconds();
        let Some(token) = bearer_token(authorization) else {
            return Ok(ApiKeyAuthorization::Unauthorized);
        };
        let Some(prefix) = parse_prefix(token) else {
            return Ok(ApiKeyAuthorization::Unauthorized);
        };
        let actual: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let Some((id, key)) = self
            .state
            .api_keys
            .iter_mut()
            .find(|(_, key)| key.prefix == prefix)
        else {
            return Ok(ApiKeyAuthorization::Unauthorized);
        };
        if key.revoked_at_epoch_seconds.is_some() || !bool::from(actual.ct_eq(&key.verifier)) {
            return Ok(ApiKeyAuthorization::Unauthorized);
        }
        if !key.scopes.contains(&required_scope) {
            return Ok(ApiKeyAuthorization::Forbidden);
        }
        let access = ApiKeyAccess {
            credential_id: id.clone(),
            organization: key.organization.clone(),
            scopes: key.scopes.clone(),
        };
        let previous_last_used = key.last_used_at_epoch_seconds;
        key.last_used_at_epoch_seconds = Some(previous_last_used.map_or(now, |used| used.max(now)));
        if let Err(error) = self.persist() {
            if let Some(key) = self.state.api_keys.get_mut(&access.credential_id) {
                key.last_used_at_epoch_seconds = previous_last_used;
            }
            return Err(error);
        }
        Ok(ApiKeyAuthorization::Authorized(access))
    }

    /// The scopes of a key in creation order (JavaScript `Set` insertion order), for callers that
    /// expose them the way the baseline does. `None` when no such key exists.
    #[must_use]
    pub fn api_key_scope_order(&self, id: &str) -> Option<Vec<ApiKeyScope>> {
        self.state.api_keys.get(id).map(ordered_scopes)
    }

    pub fn revoke_api_key(
        &mut self,
        actor: &AccountId,
        organization: &OrganizationId,
        id: &str,
    ) -> Result<bool, HubError> {
        let now = self.now_epoch_seconds();
        if !self.authorize(actor, organization)?.can_manage_resources() {
            return Err(crate::AuthorityError::ManageResourcesRequired.into());
        }
        let Some(key) = self.state.api_keys.get_mut(id) else {
            return Ok(false);
        };
        if &key.organization != organization {
            return Ok(false);
        }
        let previous_revoked_at = key.revoked_at_epoch_seconds;
        key.revoked_at_epoch_seconds.get_or_insert(now);
        if let Err(error) = self.persist() {
            if let Some(key) = self.state.api_keys.get_mut(id) {
                key.revoked_at_epoch_seconds = previous_revoked_at;
            }
            return Err(error);
        }
        Ok(true)
    }
}

/// The creation order when the stored order still describes the stored set, declaration order for
/// keys stored before the order existed.
fn ordered_scopes(key: &StoredApiKey) -> Vec<ApiKeyScope> {
    let stored_order_is_current = key.scope_order.len() == key.scopes.len()
        && key
            .scope_order
            .iter()
            .all(|scope| key.scopes.contains(scope));
    if stored_order_is_current {
        key.scope_order.clone()
    } else {
        key.scopes.iter().copied().collect()
    }
}

fn summary(id: &str, key: &StoredApiKey) -> ApiKeySummary {
    ApiKeySummary {
        id: id.to_owned(),
        name: key.name.clone(),
        prefix: key.prefix.clone(),
        scopes: ordered_scopes(key),
        created_at_epoch_seconds: key.created_at_epoch_seconds,
        last_used_at_epoch_seconds: key.last_used_at_epoch_seconds,
        revoked_at_epoch_seconds: key.revoked_at_epoch_seconds,
        last_used: key.last_used_at_epoch_seconds.is_some(),
        revoked: key.revoked_at_epoch_seconds.is_some(),
    }
}

fn bearer_token(value: &str) -> Option<&str> {
    let token = value.strip_prefix("Bearer ")?;
    (!token.is_empty() && token.len() <= MAX_TOKEN_LENGTH).then_some(token)
}

fn parse_prefix(token: &str) -> Option<&str> {
    let separator = PREFIX_START.len() + PREFIX_RANDOM_LENGTH;
    if token.len() <= separator || token.as_bytes().get(separator) != Some(&b'_') {
        return None;
    }
    let prefix = token.get(..separator)?;
    if !prefix.starts_with(PREFIX_START)
        || !prefix[PREFIX_START.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return None;
    }
    Some(prefix)
}
