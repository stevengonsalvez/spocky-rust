//! `PostgreSQL` implementation of pinned invitation serialization boundaries.

use postgres::{Client, NoTls};
use uuid::Uuid;

use crate::{HubError, InvitationRole, StoreError};

pub struct PostgresInvitationStore {
    client: Client,
}

impl PostgresInvitationStore {
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
        CREATE INDEX IF NOT EXISTS members_user_id_idx ON member (user_id);
        CREATE INDEX IF NOT EXISTS members_organization_id_idx ON member (organization_id);
        CREATE TABLE IF NOT EXISTS invitation (
            id TEXT PRIMARY KEY,
            organization_id TEXT NOT NULL REFERENCES organization(id) ON DELETE CASCADE,
            email TEXT NOT NULL,
            role TEXT NOT NULL,
            status TEXT NOT NULL,
            expires_at TIMESTAMPTZ NOT NULL,
            inviter_id TEXT NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            CONSTRAINT invitations_role_check CHECK (role IN ('admin', 'member')),
            CONSTRAINT invitations_status_check
                CHECK (status IN ('pending', 'accepted', 'rejected', 'canceled'))
        );
        CREATE INDEX IF NOT EXISTS invitations_organization_status_idx
            ON invitation (organization_id, status);
        CREATE UNIQUE INDEX IF NOT EXISTS invitations_pending_organization_email_unique
            ON invitation (organization_id, lower(email)) WHERE status = 'pending';
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

    pub fn seed_identity(
        &mut self,
        organization: &str,
        manager: &str,
        manager_email: &str,
        invitee: &str,
        invitee_email: &str,
    ) -> Result<(), HubError> {
        let mut transaction = self.client.transaction().map_err(database_error)?;
        transaction
            .execute(
                "INSERT INTO organization (id, name, slug) VALUES ($1, $1, $1)
                 ON CONFLICT (id) DO NOTHING",
                &[&organization],
            )
            .map_err(database_error)?;
        for (id, email) in [(manager, manager_email), (invitee, invitee_email)] {
            transaction
                .execute(
                    "INSERT INTO \"user\" (id, name, email, email_verified)
                     VALUES ($1, $1, $2, TRUE) ON CONFLICT (id) DO NOTHING",
                    &[&id, &email],
                )
                .map_err(database_error)?;
        }
        transaction
            .execute(
                "INSERT INTO member (id, organization_id, user_id, role)
                 VALUES ($1, $2, $3, 'owner') ON CONFLICT (organization_id, user_id) DO NOTHING",
                &[&Uuid::new_v4().to_string(), &organization, &manager],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    pub fn create(
        &mut self,
        organization: &str,
        inviter: &str,
        email: &str,
        role: InvitationRole,
    ) -> Result<String, HubError> {
        let email = normalize_email(email).ok_or(HubError::InvalidInvitationInput)?;
        let role = invitation_role_name(role);
        let mut transaction = self.client.transaction().map_err(database_error)?;
        lock_organization(&mut transaction, organization)?;
        let manager = transaction
            .query_opt(
                "SELECT role FROM member
                 WHERE organization_id = $1 AND user_id = $2 FOR UPDATE",
                &[&organization, &inviter],
            )
            .map_err(database_error)?;
        if !matches!(manager.map(|row| row.get::<_, String>(0)), Some(role) if role == "owner" || role == "admin")
        {
            return Err(HubError::InvitationManagementRequired);
        }
        transaction
            .execute(
                "UPDATE invitation SET status = 'canceled'
                 WHERE organization_id = $1 AND status = 'pending' AND expires_at <= now()",
                &[&organization],
            )
            .map_err(database_error)?;
        if transaction
            .query_opt(
                "SELECT 1 FROM member JOIN \"user\" ON \"user\".id = member.user_id
                 WHERE member.organization_id = $1 AND lower(\"user\".email) = $2 LIMIT 1",
                &[&organization, &email],
            )
            .map_err(database_error)?
            .is_some()
        {
            return Err(HubError::AlreadyMember);
        }
        if let Some(row) = transaction
            .query_opt(
                "SELECT id FROM invitation
                 WHERE organization_id = $1 AND lower(email) = $2 AND status = 'pending'
                   AND expires_at > now()",
                &[&organization, &email],
            )
            .map_err(database_error)?
        {
            let id = row.get(0);
            transaction.commit().map_err(database_error)?;
            return Ok(id);
        }
        let id = Uuid::new_v4().to_string();
        transaction
            .execute(
                "INSERT INTO invitation
                    (id, organization_id, email, role, status, expires_at, inviter_id)
                 VALUES ($1, $2, $3, $4, 'pending', now() + interval '48 hours', $5)",
                &[&id, &organization, &email, &role, &inviter],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)?;
        Ok(id)
    }

    pub fn accept(
        &mut self,
        invitation: &str,
        user: &str,
        email: &str,
    ) -> Result<String, HubError> {
        let email = normalize_email(email).ok_or(HubError::InvitationUnavailable)?;
        let mut transaction = self.client.transaction().map_err(database_error)?;
        let target = transaction
            .query_opt(
                "SELECT organization_id FROM invitation WHERE id = $1",
                &[&invitation],
            )
            .map_err(database_error)?
            .ok_or(HubError::InvitationUnavailable)?;
        let organization: String = target.get(0);
        transaction
            .query_one(
                "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
                &[&format!("paseo:invitation:{invitation}")],
            )
            .map_err(database_error)?;
        lock_organization(&mut transaction, &organization)?;
        let row = transaction
            .query_opt(
                "SELECT email, role FROM invitation
                 WHERE id = $1 AND status = 'pending' AND expires_at > now()
                 FOR UPDATE",
                &[&invitation],
            )
            .map_err(database_error)?
            .ok_or(HubError::InvitationUnavailable)?;
        let stored_email: String = row.get("email");
        if normalize_email(&stored_email).as_deref() != Some(email.as_str()) {
            return Err(HubError::InvitationUnavailable);
        }
        let role: String = row.get("role");
        if !matches!(role.as_str(), "admin" | "member") {
            return Err(HubError::InvitationUnavailable);
        }
        transaction
            .execute(
                "INSERT INTO member (id, organization_id, user_id, role)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (organization_id, user_id) DO NOTHING",
                &[&Uuid::new_v4().to_string(), &organization, &user, &role],
            )
            .map_err(database_error)?;
        transaction
            .execute(
                "UPDATE invitation SET status = 'accepted' WHERE id = $1",
                &[&invitation],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)?;
        Ok(organization)
    }

    pub fn pending_count(&mut self, organization: &str, email: &str) -> Result<i64, HubError> {
        let email = normalize_email(email).ok_or(HubError::InvalidInvitationInput)?;
        self.client
            .query_one(
                "SELECT count(*) FROM invitation
                 WHERE organization_id = $1 AND lower(email) = $2 AND status = 'pending'",
                &[&organization, &email],
            )
            .map(|row| row.get(0))
            .map_err(database_error)
    }

    pub fn membership_count(&mut self, organization: &str, user: &str) -> Result<i64, HubError> {
        self.client
            .query_one(
                "SELECT count(*) FROM member WHERE organization_id = $1 AND user_id = $2",
                &[&organization, &user],
            )
            .map(|row| row.get(0))
            .map_err(database_error)
    }
}

fn lock_organization(
    transaction: &mut postgres::Transaction<'_>,
    organization: &str,
) -> Result<(), HubError> {
    transaction
        .query_one(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            &[&format!("paseo:organization-membership:{organization}")],
        )
        .map_err(database_error)?;
    Ok(())
}

fn normalize_email(email: &str) -> Option<String> {
    let email = email.trim().to_ascii_lowercase();
    (!email.is_empty() && email.contains('@')).then_some(email)
}

const fn invitation_role_name(role: InvitationRole) -> &'static str {
    match role {
        InvitationRole::Admin => "admin",
        InvitationRole::Member => "member",
    }
}

fn database_error(error: postgres::Error) -> HubError {
    HubError::Store(StoreError::from(error))
}
