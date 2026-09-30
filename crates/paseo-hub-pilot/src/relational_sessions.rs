//! `PostgreSQL` implementation of pinned active-organization session selection.

use postgres::{Client, NoTls};
use uuid::Uuid;

use crate::{HubError, StoreError};

pub struct PostgresSessionStore {
    client: Client,
}

impl PostgresSessionStore {
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
        CREATE TABLE IF NOT EXISTS member (
            id TEXT PRIMARY KEY,
            organization_id TEXT NOT NULL REFERENCES organization(id) ON DELETE CASCADE,
            user_id TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
            role TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            UNIQUE (organization_id, user_id),
            CONSTRAINT members_role_check CHECK (role IN ('owner', 'admin', 'member'))
        );
        CREATE TABLE IF NOT EXISTS session (
            id TEXT PRIMARY KEY,
            expires_at TIMESTAMPTZ NOT NULL,
            token TEXT NOT NULL UNIQUE,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            ip_address TEXT,
            user_agent TEXT,
            user_id TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
            active_organization_id TEXT
        );
        CREATE INDEX IF NOT EXISTS sessions_active_organization_id_idx
            ON session (active_organization_id);
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

    pub fn seed(
        &mut self,
        user: &str,
        first_organization: &str,
        second_organization: &str,
        session: &str,
    ) -> Result<(), HubError> {
        let mut transaction = self.client.transaction().map_err(database_error)?;
        for organization in [first_organization, second_organization] {
            transaction
                .execute(
                    "INSERT INTO organization (id, name, slug) VALUES ($1, $1, $1)
                     ON CONFLICT (id) DO NOTHING",
                    &[&organization],
                )
                .map_err(database_error)?;
        }
        let email = format!("{user}@example.test");
        transaction
            .execute(
                "INSERT INTO \"user\" (id, name, email, email_verified)
                 VALUES ($1, $1, $2, TRUE) ON CONFLICT (id) DO NOTHING",
                &[&user, &email],
            )
            .map_err(database_error)?;
        for organization in [first_organization, second_organization] {
            transaction
                .execute(
                    "INSERT INTO member (id, organization_id, user_id, role)
                     VALUES ($1, $2, $3, 'owner')
                     ON CONFLICT (organization_id, user_id) DO NOTHING",
                    &[&Uuid::new_v4().to_string(), &organization, &user],
                )
                .map_err(database_error)?;
        }
        transaction
            .execute(
                "INSERT INTO session
                    (id, expires_at, token, user_id, active_organization_id)
                 VALUES ($1, now() + interval '48 hours', $1, $2, $3)
                 ON CONFLICT (id) DO UPDATE SET
                    active_organization_id = excluded.active_organization_id,
                    updated_at = now()",
                &[&session, &user, &first_organization],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    pub fn select_organization(
        &mut self,
        session: &str,
        user: &str,
        organization: &str,
    ) -> Result<bool, HubError> {
        let mut transaction = self.client.transaction().map_err(database_error)?;
        if transaction
            .query_opt(
                "SELECT 1 FROM member
                 WHERE user_id = $1 AND organization_id = $2
                   AND role IN ('owner', 'admin', 'member')",
                &[&user, &organization],
            )
            .map_err(database_error)?
            .is_none()
        {
            transaction.commit().map_err(database_error)?;
            return Ok(false);
        }
        let changed = transaction
            .execute(
                "UPDATE session SET active_organization_id = $3, updated_at = now()
                 WHERE id = $1 AND user_id = $2 AND expires_at > now()",
                &[&session, &user, &organization],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)?;
        Ok(changed == 1)
    }

    pub fn active_organization(
        &mut self,
        session: &str,
        user: &str,
    ) -> Result<Option<String>, HubError> {
        self.client
            .query_opt(
                "SELECT session.active_organization_id
                 FROM session
                 JOIN member ON member.organization_id = session.active_organization_id
                    AND member.user_id = session.user_id
                    AND member.role IN ('owner', 'admin', 'member')
                 WHERE session.id = $1 AND session.user_id = $2 AND session.expires_at > now()",
                &[&session, &user],
            )
            .map(|row| row.and_then(|row| row.get(0)))
            .map_err(database_error)
    }

    pub fn remove_membership(&mut self, user: &str, organization: &str) -> Result<(), HubError> {
        self.client
            .execute(
                "DELETE FROM member WHERE user_id = $1 AND organization_id = $2",
                &[&user, &organization],
            )
            .map(|_| ())
            .map_err(database_error)
    }
}

fn database_error(error: postgres::Error) -> HubError {
    HubError::Store(StoreError::from(error))
}
