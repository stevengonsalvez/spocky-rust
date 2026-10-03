//! `PostgreSQL` implementation of the pinned organization API-key boundary.

use std::collections::BTreeSet;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use postgres::{Client, NoTls, Row};
use sha2::{Digest, Sha256};
use spocky_contracts::text::{js_length, js_trim};
use subtle::ConstantTimeEq as _;
use uuid::Uuid;

use crate::{
    ApiKeyAccess, ApiKeyAuthorization, ApiKeyScope, ApiKeySummary, CreatedApiKey, HubError,
    OrganizationId, StoreError,
};

const PREFIX_START: &str = "paseo_pk_";
const PREFIX_RANDOM_BYTES: usize = 9;
const PREFIX_RANDOM_LENGTH: usize = 12;
const SECRET_BYTES: usize = 32;

pub struct PostgresApiKeyStore {
    client: Client,
}

impl PostgresApiKeyStore {
    pub const DISPOSABLE_SCHEMA_SQL: &str = r#"
        CREATE TABLE IF NOT EXISTS organization (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            slug TEXT NOT NULL UNIQUE
        );
        CREATE TABLE IF NOT EXISTS "user" (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            email TEXT NOT NULL UNIQUE,
            email_verified BOOLEAN NOT NULL DEFAULT TRUE
        );
        CREATE TABLE IF NOT EXISTS organization_api_keys (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            organization_id TEXT NOT NULL REFERENCES organization(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            prefix TEXT NOT NULL UNIQUE,
            verifier TEXT NOT NULL,
            scopes TEXT[] NOT NULL,
            created_by_user_id TEXT REFERENCES "user"(id) ON DELETE SET NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            last_used_at TIMESTAMPTZ,
            revoked_at TIMESTAMPTZ,
            CONSTRAINT organization_api_keys_scopes_check CHECK (
                scopes <@ ARRAY[
                    'projects:read',
                    'configuration:validate',
                    'configuration:install',
                    'runs:dispatch',
                    'daemons:enroll'
                ]::TEXT[] AND cardinality(scopes) > 0
            )
        );
        CREATE INDEX IF NOT EXISTS organization_api_keys_organization_created_idx
            ON organization_api_keys (organization_id, created_at DESC);
        CREATE TABLE IF NOT EXISTS daemon_enrollment_tokens (
            id UUID PRIMARY KEY,
            verifier TEXT NOT NULL,
            organization_id TEXT NOT NULL REFERENCES organization(id) ON DELETE CASCADE,
            issued_by_api_key_id UUID REFERENCES organization_api_keys(id) ON DELETE SET NULL,
            expires_at TIMESTAMPTZ NOT NULL,
            consumed_at TIMESTAMPTZ
        );
    "#;

    pub fn open(connection: &str) -> Result<Self, HubError> {
        Ok(Self {
            client: Client::connect(connection, NoTls).map_err(database_error)?,
        })
    }

    pub fn bootstrap_disposable_schema(connection: &str) -> Result<(), HubError> {
        let mut client = Client::connect(connection, NoTls).map_err(database_error)?;
        client
            .batch_execute(Self::DISPOSABLE_SCHEMA_SQL)
            .map_err(database_error)
    }

    pub fn seed_identity(&mut self, organization: &str, user: &str) -> Result<(), HubError> {
        self.client
            .execute(
                "INSERT INTO organization (id, name, slug) VALUES ($1, $1, $1)
                 ON CONFLICT (id) DO NOTHING",
                &[&organization],
            )
            .map_err(database_error)?;
        let email = format!("{user}@example.test");
        self.client
            .execute(
                "INSERT INTO \"user\" (id, name, email, email_verified)
                 VALUES ($1, $1, $2, TRUE) ON CONFLICT (id) DO NOTHING",
                &[&user, &email],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn create(
        &mut self,
        organization: &str,
        created_by_user: &str,
        name: &str,
        scopes: &[ApiKeyScope],
    ) -> Result<CreatedApiKey, HubError> {
        let name = js_trim(name);
        // `[...new Set(scopes)]`: first-seen order, stored as given.
        let mut ordered = Vec::new();
        for scope in scopes {
            if !ordered.contains(scope) {
                ordered.push(*scope);
            }
        }
        if name.is_empty() || js_length(name) > 100 || ordered.is_empty() || ordered.len() > 5 {
            return Err(HubError::InvalidApiKeyInput);
        }
        let mut prefix_bytes = [0_u8; PREFIX_RANDOM_BYTES];
        let mut secret_bytes = [0_u8; SECRET_BYTES];
        getrandom::fill(&mut prefix_bytes).map_err(|_| HubError::RandomUnavailable)?;
        getrandom::fill(&mut secret_bytes).map_err(|_| HubError::RandomUnavailable)?;
        let prefix = format!("{PREFIX_START}{}", URL_SAFE_NO_PAD.encode(prefix_bytes));
        let secret = format!("{prefix}_{}", URL_SAFE_NO_PAD.encode(secret_bytes));
        let id = Uuid::new_v4().to_string();
        let verifier = URL_SAFE_NO_PAD.encode(Sha256::digest(secret.as_bytes()));
        let scope_names = ordered
            .iter()
            .map(|scope| scope_name(*scope))
            .collect::<Vec<_>>();
        let row = self
            .client
            .query_one(
                "INSERT INTO organization_api_keys
                    (id, organization_id, name, prefix, verifier, scopes, created_by_user_id)
                 VALUES ($1::text::uuid, $2, $3, $4, $5, $6, $7)
                 RETURNING id::text, name, prefix, scopes,
                    extract(epoch from created_at)::bigint AS created_at,
                    extract(epoch from last_used_at)::bigint AS last_used_at,
                    extract(epoch from revoked_at)::bigint AS revoked_at",
                &[
                    &id,
                    &organization,
                    &name,
                    &prefix,
                    &verifier,
                    &scope_names,
                    &created_by_user,
                ],
            )
            .map_err(database_error)?;
        Ok(CreatedApiKey {
            summary: summary(&row)?,
            secret,
        })
    }

    pub fn list(&mut self, organization: &str) -> Result<Vec<ApiKeySummary>, HubError> {
        self.client
            .query(
                "SELECT id::text, name, prefix, scopes,
                    extract(epoch from created_at)::bigint AS created_at,
                    extract(epoch from last_used_at)::bigint AS last_used_at,
                    extract(epoch from revoked_at)::bigint AS revoked_at
                 FROM organization_api_keys WHERE organization_id = $1
                 ORDER BY created_at DESC, id DESC",
                &[&organization],
            )
            .map_err(database_error)?
            .iter()
            .map(summary)
            .collect()
    }

    pub fn authorize(
        &mut self,
        credential: &str,
        required_scope: ApiKeyScope,
    ) -> Result<ApiKeyAuthorization, HubError> {
        let token = credential.strip_prefix("Bearer ").unwrap_or(credential);
        let Some(prefix) = parse_prefix(token) else {
            return Ok(ApiKeyAuthorization::Unauthorized);
        };
        let Some(row) = self
            .client
            .query_opt(
                "SELECT id::text, organization_id, verifier, scopes
                 FROM organization_api_keys WHERE prefix = $1 AND revoked_at IS NULL",
                &[&prefix],
            )
            .map_err(database_error)?
        else {
            return Ok(ApiKeyAuthorization::Unauthorized);
        };
        let expected: String = row.get("verifier");
        let actual = Sha256::digest(token.as_bytes());
        let Ok(expected) = URL_SAFE_NO_PAD.decode(expected) else {
            return Ok(ApiKeyAuthorization::Unauthorized);
        };
        if actual.len() != expected.len() || !bool::from(actual.as_slice().ct_eq(&expected)) {
            return Ok(ApiKeyAuthorization::Unauthorized);
        }
        let scopes = parse_scopes(row.get("scopes"))?;
        if !scopes.contains(&required_scope) {
            return Ok(ApiKeyAuthorization::Forbidden);
        }
        let id: String = row.get("id");
        if self
            .client
            .execute(
                "UPDATE organization_api_keys
                 SET last_used_at = greatest(coalesce(last_used_at, to_timestamp(0)), now())
                 WHERE id = $1::text::uuid AND revoked_at IS NULL",
                &[&id],
            )
            .map_err(database_error)?
            != 1
        {
            return Ok(ApiKeyAuthorization::Unauthorized);
        }
        Ok(ApiKeyAuthorization::Authorized(ApiKeyAccess {
            credential_id: id,
            organization: OrganizationId::from(row.get::<_, &str>("organization_id")),
            scopes: scopes.into_iter().collect::<BTreeSet<_>>(),
        }))
    }

    pub fn revoke(&mut self, organization: &str, id: &str) -> Result<bool, HubError> {
        let mut transaction = self.client.transaction().map_err(database_error)?;
        transaction
            .query_one(
                "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
                &[&id],
            )
            .map_err(database_error)?;
        let updated = transaction
            .execute(
                "UPDATE organization_api_keys
                 SET revoked_at = coalesce(revoked_at, now())
                 WHERE id = $1::text::uuid AND organization_id = $2",
                &[&id, &organization],
            )
            .map_err(database_error)?;
        if updated == 1 {
            transaction
                .execute(
                    "UPDATE daemon_enrollment_tokens
                     SET expires_at = least(expires_at, now())
                     WHERE issued_by_api_key_id = $1::text::uuid
                       AND organization_id = $2 AND consumed_at IS NULL",
                    &[&id, &organization],
                )
                .map_err(database_error)?;
        }
        transaction.commit().map_err(database_error)?;
        Ok(updated == 1)
    }

    pub fn issue_enrollment_token(
        &mut self,
        id: &str,
        verifier: &str,
        organization: &str,
        issued_by_api_key: &str,
        expires_at_epoch_seconds: i64,
    ) -> Result<bool, HubError> {
        let mut transaction = self.client.transaction().map_err(database_error)?;
        transaction
            .query_one(
                "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
                &[&issued_by_api_key],
            )
            .map_err(database_error)?;
        if transaction
            .query_opt(
                "SELECT id FROM organization_api_keys
                 WHERE id = $1::text::uuid AND organization_id = $2 AND revoked_at IS NULL
                 FOR UPDATE",
                &[&issued_by_api_key, &organization],
            )
            .map_err(database_error)?
            .is_none()
        {
            transaction.commit().map_err(database_error)?;
            return Ok(false);
        }
        transaction
            .execute(
                "INSERT INTO daemon_enrollment_tokens
                    (id, verifier, organization_id, issued_by_api_key_id, expires_at)
                 VALUES ($1::text::uuid, $2, $3, $4::text::uuid, to_timestamp($5::bigint))",
                &[
                    &id,
                    &verifier,
                    &organization,
                    &issued_by_api_key,
                    &expires_at_epoch_seconds,
                ],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)?;
        Ok(true)
    }

    pub fn enrollment_token_is_expired(&mut self, id: &str) -> Result<bool, HubError> {
        self.client
            .query_one(
                "SELECT expires_at <= now() FROM daemon_enrollment_tokens WHERE id = $1::text::uuid",
                &[&id],
            )
            .map(|row| row.get(0))
            .map_err(database_error)
    }
}

fn summary(row: &Row) -> Result<ApiKeySummary, HubError> {
    let last_used_at_epoch_seconds = row
        .get::<_, Option<i64>>("last_used_at")
        .map(i64::cast_unsigned);
    let revoked_at_epoch_seconds = row
        .get::<_, Option<i64>>("revoked_at")
        .map(i64::cast_unsigned);
    Ok(ApiKeySummary {
        id: row.get("id"),
        name: row.get("name"),
        prefix: row.get("prefix"),
        scopes: parse_scopes(row.get("scopes"))?,
        created_at_epoch_seconds: row.get::<_, i64>("created_at").cast_unsigned(),
        last_used_at_epoch_seconds,
        revoked_at_epoch_seconds,
        last_used: last_used_at_epoch_seconds.is_some(),
        revoked: revoked_at_epoch_seconds.is_some(),
    })
}

fn parse_prefix(token: &str) -> Option<&str> {
    if token.len() > 200 {
        return None;
    }
    let separator = PREFIX_START.len() + PREFIX_RANDOM_LENGTH;
    if token.len() <= separator || token.as_bytes().get(separator) != Some(&b'_') {
        return None;
    }
    let prefix = token.get(..separator)?;
    prefix[PREFIX_START.len()..]
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        .then_some(prefix)
}

fn scope_name(scope: ApiKeyScope) -> &'static str {
    match scope {
        ApiKeyScope::ProjectsRead => "projects:read",
        ApiKeyScope::ConfigurationValidate => "configuration:validate",
        ApiKeyScope::ConfigurationInstall => "configuration:install",
        ApiKeyScope::RunsDispatch => "runs:dispatch",
        ApiKeyScope::DaemonsEnroll => "daemons:enroll",
    }
}

/// The stored array in its stored (creation) order.
fn parse_scopes(scopes: Vec<String>) -> Result<Vec<ApiKeyScope>, HubError> {
    scopes
        .into_iter()
        .map(|scope| match scope.as_str() {
            "projects:read" => Ok(ApiKeyScope::ProjectsRead),
            "configuration:validate" => Ok(ApiKeyScope::ConfigurationValidate),
            "configuration:install" => Ok(ApiKeyScope::ConfigurationInstall),
            "runs:dispatch" => Ok(ApiKeyScope::RunsDispatch),
            "daemons:enroll" => Ok(ApiKeyScope::DaemonsEnroll),
            _ => Err(HubError::InvalidApiKeyInput),
        })
        .collect()
}

fn database_error(error: postgres::Error) -> HubError {
    HubError::Store(StoreError::from(error))
}
