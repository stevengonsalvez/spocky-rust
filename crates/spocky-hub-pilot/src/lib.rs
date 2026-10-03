//! Contract pilot for Hub authority, daemon registration, and durable restart behavior.
//!
//! The embedded SQL pilot uses `SQLite` and names its gaps from `PGlite` and `PostgreSQL`.

#![allow(clippy::missing_errors_doc)]

use std::collections::{BTreeMap, BTreeSet, hash_map::DefaultHasher};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use postgres::{Client, NoTls};
use serde::{Deserialize, Serialize};

mod account_emails;
mod api_keys;
pub mod billing;
pub mod daemon_socket;
mod directory_lock;
mod email_delivery;
mod embedded_schema;
mod embedded_sql;
pub mod http;
mod invitations;
pub mod public_api;
mod relational_api_keys;
mod relational_invitations;
mod relational_sessions;
mod retained_pglite;
pub mod triggers;

pub use account_emails::{
    AccountEmailMessage, render_password_reset_email, render_verification_email,
};
pub use api_keys::{ApiKeyAccess, ApiKeyAuthorization, ApiKeyScope, ApiKeySummary, CreatedApiKey};
pub use email_delivery::{ResendConfig, ResendEmailDelivery};
pub use embedded_sql::EmbeddedSqlStore;
pub use invitations::{
    InvitationEmail, InvitationEmailMessage, InvitationRole, InvitationSummary,
    render_invitation_email,
};
pub use relational_api_keys::PostgresApiKeyStore;
pub use relational_invitations::PostgresInvitationStore;
pub use relational_sessions::PostgresSessionStore;
pub use retained_pglite::{
    HostIdentity, IpcValue, MigrationOutcome, QueryResult, RetainedHostError, RetainedPgliteConfig,
    RetainedPgliteHost, SqlStatement,
};

macro_rules! identifier {
    ($name:ident) => {
        #[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
        pub struct $name(String);

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl $name {
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identifier!(AccountId);
identifier!(OrganizationId);
identifier!(DaemonId);
identifier!(SessionToken);
identifier!(RecoveryToken);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Role {
    Owner,
    Admin,
    Member,
}

impl Role {
    const fn can_manage_resources(self) -> bool {
        matches!(self, Self::Owner | Self::Admin)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum DaemonPermission {
    HubExecute,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bootstrap {
    pub instance_secret: String,
    pub owner: AccountId,
    pub organization: OrganizationId,
    pub temporary_password: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootstrapResult {
    pub created: bool,
    pub password_change_required: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PasswordChange {
    pub account: AccountId,
    pub current_password: String,
    pub new_password: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistrationRequest {
    organization: OrganizationId,
    daemon: DaemonId,
    idempotency_key: String,
    permissions: BTreeSet<DaemonPermission>,
}

impl RegistrationRequest {
    pub fn new(
        organization: OrganizationId,
        daemon: DaemonId,
        idempotency_key: impl Into<String>,
        permissions: impl IntoIterator<Item = DaemonPermission>,
    ) -> Self {
        Self {
            organization,
            daemon,
            idempotency_key: idempotency_key.into(),
            permissions: permissions.into_iter().collect(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DaemonRegistration {
    pub daemon: DaemonId,
    pub organization: OrganizationId,
    pub registration_generation: u64,
    pub permissions: BTreeSet<DaemonPermission>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DaemonSession {
    pub daemon: DaemonId,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreSemantics {
    SingleProcessFileSnapshot,
    EmbeddedSqlTransactionalSnapshot,
    PostgreSqlTransactionalSnapshot,
}

pub trait DurableHubStore {
    fn load(&self) -> Result<Option<Vec<u8>>, StoreError>;
    fn save(&self, bytes: &[u8]) -> Result<(), StoreError>;
}

#[derive(Clone, Debug)]
pub struct EmbeddedFileStore {
    path: PathBuf,
}

impl EmbeddedFileStore {
    pub const SEMANTICS: StoreSemantics = StoreSemantics::SingleProcessFileSnapshot;
    pub const LIMITATIONS: &str =
        "not PGlite; not PostgreSQL; no cross-process transactions; no SQL or advisory-lock parity";

    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(Self { path })
    }
}

impl DurableHubStore for EmbeddedFileStore {
    fn load(&self) -> Result<Option<Vec<u8>>, StoreError> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn save(&self, bytes: &[u8]) -> Result<(), StoreError> {
        atomic_write(&self.path, bytes)
    }
}

pub struct PostgresStore {
    client: Mutex<Client>,
    state_key: String,
}

impl PostgresStore {
    pub const SEMANTICS: StoreSemantics = StoreSemantics::PostgreSqlTransactionalSnapshot;
    pub const LIMITATIONS: &str =
        "not PGlite; whole-state snapshot rather than the baseline relational schema";

    pub fn open(connection: &str, state_key: impl Into<String>) -> Result<Self, StoreError> {
        let mut client = Client::connect(connection, NoTls)?;
        client.batch_execute(
            "CREATE TABLE IF NOT EXISTS spocky_hub_pilot_state (
                state_key TEXT PRIMARY KEY,
                state_bytes BYTEA NOT NULL,
                revision BIGINT NOT NULL DEFAULT 1
            )",
        )?;
        Ok(Self {
            client: Mutex::new(client),
            state_key: state_key.into(),
        })
    }
}

impl DurableHubStore for PostgresStore {
    fn load(&self) -> Result<Option<Vec<u8>>, StoreError> {
        let mut client = self.client.lock().map_err(|_| StoreError::Poisoned)?;
        let row = client.query_opt(
            "SELECT state_bytes FROM spocky_hub_pilot_state WHERE state_key = $1",
            &[&self.state_key],
        )?;
        Ok(row.map(|row| row.get(0)))
    }

    fn save(&self, bytes: &[u8]) -> Result<(), StoreError> {
        let mut client = self.client.lock().map_err(|_| StoreError::Poisoned)?;
        let mut transaction = client.transaction()?;
        transaction.query_one(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            &[&self.state_key],
        )?;
        transaction.execute(
            "INSERT INTO spocky_hub_pilot_state (state_key, state_bytes, revision)
             VALUES ($1, $2, 1)
             ON CONFLICT (state_key) DO UPDATE
             SET state_bytes = EXCLUDED.state_bytes,
                 revision = spocky_hub_pilot_state.revision + 1",
            &[&self.state_key, &bytes],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Account {
    password_fingerprint: u64,
    must_change_password: bool,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default = "default_true")]
    verified: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredRegistration {
    result: DaemonRegistration,
    idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RecoveryGrant {
    account: AccountId,
    expires_at_epoch_seconds: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct HubState {
    instance_secret_fingerprint: Option<u64>,
    instance_operator: Option<AccountId>,
    accounts: BTreeMap<AccountId, Account>,
    memberships: BTreeMap<OrganizationId, BTreeMap<AccountId, Role>>,
    registrations: BTreeMap<DaemonId, StoredRegistration>,
    registration_keys: BTreeMap<String, DaemonId>,
    session_generations: BTreeMap<DaemonId, u64>,
    continuations: BTreeMap<DaemonId, BTreeMap<String, DaemonSession>>,
    browser_sessions: BTreeMap<SessionToken, AccountId>,
    active_browser_organizations: BTreeMap<SessionToken, OrganizationId>,
    next_browser_session: u64,
    app_setup_complete: bool,
    api_keys: BTreeMap<String, api_keys::StoredApiKey>,
    next_api_key_sequence: u64,
    invitations: BTreeMap<String, invitations::StoredInvitation>,
    invitation_entitlements: BTreeMap<OrganizationId, invitations::InvitationEntitlements>,
    next_invitation_sequence: u64,
    verification_tokens: BTreeMap<RecoveryToken, RecoveryGrant>,
    password_reset_tokens: BTreeMap<RecoveryToken, RecoveryGrant>,
    next_recovery_sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BrowserAccountStatus {
    InstanceSetupRequired,
    SignedOut,
    PasswordChangeRequired,
    AppSetupRequired,
    OrganizationRequired,
    Active,
}

#[derive(Serialize)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum BrowserAccountState {
    InstanceSetupRequired,
    SignedOut {
        registration: &'static str,
    },
    PasswordChangeRequired {
        account: BrowserAccountSummary,
    },
    AppSetupRequired {
        account: BrowserAccountSummary,
        organization: BrowserOrganizationSummary,
        memberships: Vec<BrowserMembershipSummary>,
        capabilities: BrowserOrganizationCapabilities,
    },
    OrganizationRequired {
        account: BrowserAccountSummary,
        memberships: Vec<BrowserMembershipSummary>,
        can_create_organization: bool,
    },
    Active {
        account: BrowserAccountSummary,
        memberships: Vec<BrowserMembershipSummary>,
        organization: BrowserOrganizationSummary,
        membership: BrowserMembershipAccess,
        capabilities: BrowserOrganizationCapabilities,
        is_instance_operator: bool,
        can_create_organization: bool,
        team: BrowserTeamSummary,
    },
}

#[derive(Serialize)]
pub(crate) struct BrowserAccountSummary {
    id: String,
    name: String,
    email: String,
}

#[derive(Serialize)]
pub(crate) struct BrowserOrganizationSummary {
    id: String,
    name: String,
    slug: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserMembershipSummary {
    id: String,
    name: String,
    slug: String,
    membership_id: String,
    role: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct BrowserOrganizationCapabilities {
    view: bool,
    manage_members: bool,
    manage_owners: bool,
    manage_resources: bool,
}

#[derive(Serialize)]
pub(crate) struct BrowserMembershipAccess {
    id: String,
    role: &'static str,
}

#[derive(Serialize)]
pub(crate) struct BrowserTeamSummary {
    members: Vec<BrowserTeamMemberSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    invitations: Option<Vec<BrowserManagerInvitationSummary>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserTeamMemberSummary {
    id: String,
    user_id: String,
    name: String,
    email: String,
    role: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BrowserManagerInvitationSummary {
    id: String,
    email: String,
    role: &'static str,
    expires_at: String,
    link: String,
}

pub struct HubPilot<S: DurableHubStore> {
    store: S,
    state: HubState,
    now_epoch_seconds_override: Option<u64>,
}

impl<S: DurableHubStore> HubPilot<S> {
    pub fn open(store: S) -> Result<Self, HubError> {
        Self::open_inner(store, None)
    }

    pub fn open_at(store: S, invitation_now_epoch_seconds: u64) -> Result<Self, HubError> {
        Self::open_inner(store, Some(invitation_now_epoch_seconds))
    }

    fn open_inner(store: S, now_epoch_seconds_override: Option<u64>) -> Result<Self, HubError> {
        let state = match store.load()? {
            Some(bytes) => serde_json::from_slice(&bytes)?,
            None => HubState::default(),
        };
        Ok(Self {
            store,
            state,
            now_epoch_seconds_override,
        })
    }

    pub(crate) fn now_epoch_seconds(&self) -> u64 {
        self.now_epoch_seconds_override.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs())
        })
    }

    pub fn bootstrap(&mut self, input: Bootstrap) -> Result<BootstrapResult, HubError> {
        if let Some(account) = self.state.accounts.get(&input.owner) {
            return Ok(BootstrapResult {
                created: false,
                password_change_required: account.must_change_password,
            });
        }
        if !self.state.accounts.is_empty() || input.instance_secret.len() < 32 {
            return Err(HubError::BootstrapUnavailable);
        }
        self.state.instance_secret_fingerprint = Some(fingerprint(&input.instance_secret));
        self.state.instance_operator = Some(input.owner.clone());
        self.state.accounts.insert(
            input.owner.clone(),
            Account {
                password_fingerprint: fingerprint(&input.temporary_password),
                must_change_password: true,
                display_name: None,
                verified: true,
            },
        );
        self.state
            .memberships
            .entry(input.organization)
            .or_default()
            .insert(input.owner, Role::Owner);
        self.persist()?;
        Ok(BootstrapResult {
            created: true,
            password_change_required: true,
        })
    }

    pub fn replace_password(&mut self, change: &PasswordChange) -> Result<(), HubError> {
        let account = self
            .state
            .accounts
            .get_mut(&change.account)
            .ok_or(AuthorityError::AccountUnavailable)?;
        if account.password_fingerprint != fingerprint(&change.current_password) {
            return Err(HubError::InvalidCurrentPassword);
        }
        account.password_fingerprint = fingerprint(&change.new_password);
        account.must_change_password = false;
        self.persist()
    }

    pub fn sign_in(
        &mut self,
        account: &AccountId,
        password: &str,
    ) -> Result<SessionToken, HubError> {
        let stored = self
            .state
            .accounts
            .get(account)
            .ok_or(HubError::InvalidCredentials)?;
        if stored.password_fingerprint != fingerprint(password) {
            return Err(HubError::InvalidCredentials);
        }
        if !stored.verified {
            return Err(HubError::EmailNotVerified);
        }
        self.create_browser_session(account)
    }

    pub fn sign_in_after_verification(
        &mut self,
        account: &AccountId,
    ) -> Result<SessionToken, HubError> {
        if !self
            .state
            .accounts
            .get(account)
            .is_some_and(|stored| stored.verified)
        {
            return Err(HubError::InvalidRecoveryToken);
        }
        self.create_browser_session(account)
    }

    fn create_browser_session(&mut self, account: &AccountId) -> Result<SessionToken, HubError> {
        self.state.next_browser_session += 1;
        let token = SessionToken(format!("hub-session-{}", self.state.next_browser_session));
        self.state
            .browser_sessions
            .insert(token.clone(), account.clone());
        let memberships = self
            .state
            .memberships
            .iter()
            .filter_map(|(organization, members)| {
                members
                    .contains_key(account)
                    .then_some(organization.clone())
            })
            .collect::<Vec<_>>();
        if let [organization] = memberships.as_slice() {
            self.state
                .active_browser_organizations
                .insert(token.clone(), organization.clone());
        }
        self.persist()?;
        Ok(token)
    }

    pub fn register_unverified_account(
        &mut self,
        account: &AccountId,
        display_name: &str,
        password: &str,
    ) -> Result<RecoveryToken, HubError> {
        // The sign-up `name` is `z.string()` in better-auth: no trim and no minimum length.
        if !account.as_str().contains('@') || password.is_empty() {
            return Err(HubError::InvalidRecoveryInput);
        }
        if self.state.accounts.contains_key(account) {
            return Err(HubError::IdempotencyConflict);
        }
        self.state.accounts.insert(
            account.clone(),
            Account {
                password_fingerprint: fingerprint(password),
                must_change_password: false,
                display_name: Some(display_name.to_owned()),
                verified: false,
            },
        );
        let token = self.next_recovery_token("verification");
        self.state
            .verification_tokens
            .insert(token.clone(), self.recovery_grant(account));
        self.persist()?;
        Ok(token)
    }

    pub fn verify_account(&mut self, token: &RecoveryToken) -> Result<AccountId, HubError> {
        let grant = self
            .state
            .verification_tokens
            .remove(token)
            .ok_or(HubError::InvalidRecoveryToken)?;
        if grant.expires_at_epoch_seconds <= self.now_epoch_seconds() {
            self.persist()?;
            return Err(HubError::InvalidRecoveryToken);
        }
        self.state
            .accounts
            .get_mut(&grant.account)
            .ok_or(HubError::InvalidRecoveryToken)?
            .verified = true;
        self.persist()?;
        Ok(grant.account)
    }

    pub fn request_password_reset(
        &mut self,
        account: &AccountId,
    ) -> Result<Option<RecoveryToken>, HubError> {
        if !self.state.accounts.contains_key(account) {
            return Ok(None);
        }
        let token = self.next_recovery_token("password-reset");
        self.state
            .password_reset_tokens
            .insert(token.clone(), self.recovery_grant(account));
        self.persist()?;
        Ok(Some(token))
    }

    pub fn reset_password(
        &mut self,
        token: &RecoveryToken,
        new_password: &str,
    ) -> Result<(), HubError> {
        if new_password.is_empty() {
            return Err(HubError::InvalidRecoveryInput);
        }
        let grant = self
            .state
            .password_reset_tokens
            .remove(token)
            .ok_or(HubError::InvalidRecoveryToken)?;
        if grant.expires_at_epoch_seconds <= self.now_epoch_seconds() {
            self.persist()?;
            return Err(HubError::InvalidRecoveryToken);
        }
        let account = grant.account;
        self.state
            .accounts
            .get_mut(&account)
            .ok_or(HubError::InvalidRecoveryToken)?
            .password_fingerprint = fingerprint(new_password);
        let revoked = self
            .state
            .browser_sessions
            .iter()
            .filter_map(|(session, owner)| (owner == &account).then_some(session.clone()))
            .collect::<Vec<_>>();
        self.state
            .browser_sessions
            .retain(|_, owner| owner != &account);
        for session in revoked {
            self.state.active_browser_organizations.remove(&session);
        }
        self.persist()
    }

    fn next_recovery_token(&mut self, prefix: &str) -> RecoveryToken {
        self.state.next_recovery_sequence += 1;
        RecoveryToken(format!("{prefix}-{}", self.state.next_recovery_sequence))
    }

    fn recovery_grant(&self, account: &AccountId) -> RecoveryGrant {
        RecoveryGrant {
            account: account.clone(),
            expires_at_epoch_seconds: self.now_epoch_seconds() + 3_600,
        }
    }

    #[must_use]
    pub fn browser_account_status(&self, token: Option<&SessionToken>) -> BrowserAccountStatus {
        if self.state.accounts.is_empty() {
            return BrowserAccountStatus::InstanceSetupRequired;
        }
        let Some((token, account_id)) = token.and_then(|token| {
            self.state
                .browser_sessions
                .get(token)
                .map(|account| (token, account))
        }) else {
            return BrowserAccountStatus::SignedOut;
        };
        let Some(account) = self.state.accounts.get(account_id) else {
            return BrowserAccountStatus::SignedOut;
        };
        if account.must_change_password {
            BrowserAccountStatus::PasswordChangeRequired
        } else if !self.state.app_setup_complete {
            BrowserAccountStatus::AppSetupRequired
        } else if self.active_organization_for_session(token).is_some() {
            BrowserAccountStatus::Active
        } else {
            BrowserAccountStatus::OrganizationRequired
        }
    }

    pub(crate) fn browser_account_state(
        &self,
        token: Option<&SessionToken>,
    ) -> BrowserAccountState {
        let status = self.browser_account_status(token);
        let account = token.and_then(|token| self.state.browser_sessions.get(token));
        if status == BrowserAccountStatus::PasswordChangeRequired
            && let Some(account) = account
        {
            return BrowserAccountState::PasswordChangeRequired {
                account: browser_account_summary(&self.state, account),
            };
        }
        if status == BrowserAccountStatus::AppSetupRequired
            && let Some((token, account)) = token.zip(account)
            && let Some(organization) = self.active_organization_for_session(token)
            && let Some(role) = self
                .state
                .memberships
                .get(&organization)
                .and_then(|members| members.get(account))
        {
            return BrowserAccountState::AppSetupRequired {
                account: browser_account_summary(&self.state, account),
                organization: browser_organization_summary(&organization),
                memberships: browser_memberships(&self.state, account),
                capabilities: browser_capabilities(*role),
            };
        }
        if status == BrowserAccountStatus::Active
            && let Some((token, account)) = token.zip(account)
            && let Some(organization) = self.active_organization_for_session(token)
            && let Some(role) = self
                .state
                .memberships
                .get(&organization)
                .and_then(|members| members.get(account))
        {
            let membership_id = browser_membership_id(&organization, account);
            return BrowserAccountState::Active {
                account: browser_account_summary(&self.state, account),
                memberships: browser_memberships(&self.state, account),
                organization: browser_organization_summary(&organization),
                membership: BrowserMembershipAccess {
                    id: membership_id,
                    role: browser_role(*role),
                },
                capabilities: browser_capabilities(*role),
                is_instance_operator: browser_is_instance_operator(&self.state, account),
                can_create_organization: false,
                team: browser_team(&self.state, &organization, *role, self.now_epoch_seconds()),
            };
        }
        if status == BrowserAccountStatus::OrganizationRequired
            && let Some(account) = account
        {
            return BrowserAccountState::OrganizationRequired {
                account: browser_account_summary(&self.state, account),
                memberships: browser_memberships(&self.state, account),
                can_create_organization: false,
            };
        }
        match status {
            BrowserAccountStatus::InstanceSetupRequired => {
                BrowserAccountState::InstanceSetupRequired
            }
            BrowserAccountStatus::SignedOut
            | BrowserAccountStatus::PasswordChangeRequired
            | BrowserAccountStatus::AppSetupRequired
            | BrowserAccountStatus::OrganizationRequired
            | BrowserAccountStatus::Active => BrowserAccountState::SignedOut {
                registration: "invite_only",
            },
        }
    }

    pub fn complete_app_setup(&mut self, token: &SessionToken) -> Result<(), HubError> {
        match self.browser_account_status(Some(token)) {
            BrowserAccountStatus::AppSetupRequired
            | BrowserAccountStatus::OrganizationRequired
            | BrowserAccountStatus::Active => {
                self.state.app_setup_complete = true;
                self.persist()
            }
            BrowserAccountStatus::PasswordChangeRequired => {
                Err(AuthorityError::PasswordChangeRequired.into())
            }
            BrowserAccountStatus::InstanceSetupRequired | BrowserAccountStatus::SignedOut => {
                Err(HubError::InvalidSession)
            }
        }
    }

    #[must_use]
    pub fn account_for_session(&self, token: &SessionToken) -> Option<AccountId> {
        self.state.browser_sessions.get(token).cloned()
    }

    #[must_use]
    pub fn active_organization_for_session(&self, token: &SessionToken) -> Option<OrganizationId> {
        let account = self.state.browser_sessions.get(token)?;
        if let Some(organization) = self.state.active_browser_organizations.get(token)
            && self
                .state
                .memberships
                .get(organization)
                .is_some_and(|members| members.contains_key(account))
        {
            return Some(organization.clone());
        }
        let memberships = self
            .state
            .memberships
            .iter()
            .filter_map(|(organization, members)| {
                members
                    .contains_key(account)
                    .then_some(organization.clone())
            })
            .collect::<Vec<_>>();
        match memberships.as_slice() {
            [organization] => Some(organization.clone()),
            _ => None,
        }
    }

    pub fn create_organization_for_session(
        &mut self,
        token: &SessionToken,
        organization: OrganizationId,
    ) -> Result<(), HubError> {
        let account = self
            .state
            .browser_sessions
            .get(token)
            .cloned()
            .ok_or(HubError::InvalidSession)?;
        let state = self
            .state
            .accounts
            .get(&account)
            .ok_or(HubError::InvalidSession)?;
        if state.must_change_password {
            return Err(AuthorityError::PasswordChangeRequired.into());
        }
        if self.state.memberships.contains_key(&organization) {
            return Err(HubError::IdempotencyConflict);
        }
        self.state
            .memberships
            .entry(organization.clone())
            .or_default()
            .insert(account, Role::Owner);
        self.state
            .active_browser_organizations
            .insert(token.clone(), organization);
        self.persist()
    }

    pub fn select_organization(
        &mut self,
        token: &SessionToken,
        organization: &OrganizationId,
    ) -> Result<(), HubError> {
        let account = self
            .state
            .browser_sessions
            .get(token)
            .ok_or(HubError::InvalidSession)?;
        if !self
            .state
            .memberships
            .get(organization)
            .is_some_and(|members| members.contains_key(account))
        {
            return Err(AuthorityError::OrganizationUnavailable.into());
        }
        self.state
            .active_browser_organizations
            .insert(token.clone(), organization.clone());
        self.persist()
    }

    pub fn authorize(
        &self,
        account: &AccountId,
        organization: &OrganizationId,
    ) -> Result<Role, AuthorityError> {
        let state = self
            .state
            .accounts
            .get(account)
            .ok_or(AuthorityError::AccountUnavailable)?;
        if state.must_change_password {
            return Err(AuthorityError::PasswordChangeRequired);
        }
        self.state
            .memberships
            .get(organization)
            .and_then(|members| members.get(account))
            .copied()
            .ok_or(AuthorityError::OrganizationUnavailable)
    }

    pub fn add_member(
        &mut self,
        actor: &AccountId,
        account: AccountId,
        organization: OrganizationId,
        role: Role,
    ) -> Result<(), HubError> {
        if !self.authorize(actor, &organization)?.can_manage_resources() {
            return Err(AuthorityError::ManageResourcesRequired.into());
        }
        self.state
            .accounts
            .entry(account.clone())
            .or_insert(Account {
                password_fingerprint: 0,
                must_change_password: false,
                display_name: None,
                verified: true,
            });
        self.state
            .memberships
            .entry(organization)
            .or_default()
            .insert(account, role);
        self.persist()
    }

    pub fn register_daemon(
        &mut self,
        actor: &AccountId,
        request: RegistrationRequest,
    ) -> Result<DaemonRegistration, HubError> {
        if !self
            .authorize(actor, &request.organization)?
            .can_manage_resources()
        {
            return Err(AuthorityError::ManageResourcesRequired.into());
        }
        if let Some(existing_id) = self.state.registration_keys.get(&request.idempotency_key) {
            let existing = &self.state.registrations[existing_id];
            if existing.result.daemon == request.daemon
                && existing.result.organization == request.organization
                && existing.result.permissions == request.permissions
            {
                return Ok(existing.result.clone());
            }
            return Err(HubError::IdempotencyConflict);
        }
        let generation = self
            .state
            .registrations
            .get(&request.daemon)
            .map_or(1, |existing| existing.result.registration_generation + 1);
        let result = DaemonRegistration {
            daemon: request.daemon.clone(),
            organization: request.organization,
            registration_generation: generation,
            permissions: request.permissions,
        };
        self.state
            .registration_keys
            .insert(request.idempotency_key.clone(), request.daemon.clone());
        self.state.registrations.insert(
            request.daemon,
            StoredRegistration {
                result: result.clone(),
                idempotency_key: request.idempotency_key,
            },
        );
        self.persist()?;
        Ok(result)
    }

    pub fn connect_daemon(
        &mut self,
        daemon: &DaemonId,
        advertised: impl IntoIterator<Item = DaemonPermission>,
    ) -> Result<DaemonSession, SessionError> {
        let registration = self
            .state
            .registrations
            .get(daemon)
            .ok_or(SessionError::DaemonUnavailable)?;
        if advertised.into_iter().collect::<BTreeSet<_>>() != registration.result.permissions {
            return Err(SessionError::PermissionAgreementMismatch);
        }
        let generation = self
            .state
            .session_generations
            .entry(daemon.clone())
            .or_default();
        *generation += 1;
        let session = DaemonSession {
            daemon: daemon.clone(),
            generation: *generation,
        };
        self.persist()
            .map_err(|_| SessionError::PersistenceFailed)?;
        Ok(session)
    }

    pub fn continue_session(
        &mut self,
        daemon: &DaemonId,
        previous_generation: u64,
        idempotency_key: impl Into<String>,
    ) -> Result<DaemonSession, SessionError> {
        let idempotency_key = idempotency_key.into();
        if let Some(existing) = self
            .state
            .continuations
            .get(daemon)
            .and_then(|continuations| continuations.get(&idempotency_key))
        {
            return Ok(existing.clone());
        }
        let current = self
            .state
            .session_generations
            .get_mut(daemon)
            .ok_or(SessionError::DaemonUnavailable)?;
        if *current != previous_generation {
            return Err(SessionError::SupersededGeneration);
        }
        *current += 1;
        let session = DaemonSession {
            daemon: daemon.clone(),
            generation: *current,
        };
        self.state
            .continuations
            .entry(daemon.clone())
            .or_default()
            .insert(idempotency_key, session.clone());
        self.persist()
            .map_err(|_| SessionError::PersistenceFailed)?;
        Ok(session)
    }

    #[must_use]
    pub fn state_contains_secret(&self, secret: &str) -> bool {
        serde_json::to_string(&self.state).is_ok_and(|state| state.contains(secret))
    }

    fn persist(&self) -> Result<(), HubError> {
        self.store.save(&serde_json::to_vec_pretty(&self.state)?)?;
        Ok(())
    }
}

fn browser_account_summary(state: &HubState, account: &AccountId) -> BrowserAccountSummary {
    let email = account.as_str().to_owned();
    let name = state
        .accounts
        .get(account)
        .and_then(|stored| stored.display_name.clone())
        .unwrap_or_else(|| email.split('@').next().unwrap_or(&email).to_owned());
    BrowserAccountSummary {
        id: email.clone(),
        name,
        email,
    }
}

fn browser_organization_summary(organization: &OrganizationId) -> BrowserOrganizationSummary {
    let id = organization.as_str().to_owned();
    BrowserOrganizationSummary {
        id: id.clone(),
        name: id.clone(),
        slug: id,
    }
}

fn browser_memberships(state: &HubState, account: &AccountId) -> Vec<BrowserMembershipSummary> {
    state
        .memberships
        .iter()
        .filter_map(|(organization, members)| {
            let role = members.get(account)?;
            let id = organization.as_str().to_owned();
            Some(BrowserMembershipSummary {
                id: id.clone(),
                name: id.clone(),
                slug: id.clone(),
                membership_id: browser_membership_id(organization, account),
                role: browser_role(*role),
            })
        })
        .collect()
}

fn browser_membership_id(organization: &OrganizationId, account: &AccountId) -> String {
    format!("membership:{}:{}", organization.as_str(), account.as_str())
}

fn browser_is_instance_operator(state: &HubState, account: &AccountId) -> bool {
    state.instance_operator.as_ref().map_or_else(
        || state.accounts.len() == 1 && state.accounts.contains_key(account),
        |operator| operator == account,
    )
}

fn browser_team(
    state: &HubState,
    organization: &OrganizationId,
    viewer_role: Role,
    now_epoch_seconds: u64,
) -> BrowserTeamSummary {
    let members = state
        .memberships
        .get(organization)
        .into_iter()
        .flat_map(BTreeMap::iter)
        .map(|(account, role)| {
            let summary = browser_account_summary(state, account);
            BrowserTeamMemberSummary {
                id: browser_membership_id(organization, account),
                user_id: summary.id,
                name: summary.name,
                email: summary.email,
                role: browser_role(*role),
            }
        })
        .collect();
    let invitations = viewer_role.can_manage_resources().then(|| {
        invitations::pending_summaries(state, organization, now_epoch_seconds)
            .into_iter()
            .map(|invitation| BrowserManagerInvitationSummary {
                id: invitation.id,
                email: invitation.email,
                role: match invitation.role {
                    InvitationRole::Admin => "admin",
                    InvitationRole::Member => "member",
                },
                expires_at: iso_timestamp(invitation.expires_at_epoch_seconds),
                link: invitation.link,
            })
            .collect()
    });
    BrowserTeamSummary {
        members,
        invitations,
    }
}

fn iso_timestamp(epoch_seconds: u64) -> String {
    let datetime = time::OffsetDateTime::from_unix_timestamp(
        i64::try_from(epoch_seconds).expect("invitation timestamp fits i64"),
    )
    .expect("invitation timestamp is representable");
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000Z",
        datetime.year(),
        u8::from(datetime.month()),
        datetime.day(),
        datetime.hour(),
        datetime.minute(),
        datetime.second()
    )
}

const fn browser_role(role: Role) -> &'static str {
    match role {
        Role::Owner => "owner",
        Role::Admin => "admin",
        Role::Member => "member",
    }
}

const fn browser_capabilities(role: Role) -> BrowserOrganizationCapabilities {
    BrowserOrganizationCapabilities {
        view: true,
        manage_members: matches!(role, Role::Owner | Role::Admin),
        manage_owners: matches!(role, Role::Owner),
        manage_resources: matches!(role, Role::Owner | Role::Admin),
    }
}

fn fingerprint(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorityError {
    AccountUnavailable,
    OrganizationUnavailable,
    PasswordChangeRequired,
    ManageResourcesRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionError {
    DaemonUnavailable,
    PermissionAgreementMismatch,
    SupersededGeneration,
    PersistenceFailed,
}

#[derive(Debug)]
pub enum HubError {
    Authority(AuthorityError),
    Session(SessionError),
    BootstrapUnavailable,
    InvalidCurrentPassword,
    InvalidCredentials,
    EmailNotVerified,
    InvalidRecoveryInput,
    InvalidRecoveryToken,
    InvalidSession,
    InvalidApiKeyInput,
    InvalidInvitationInput,
    InvitationManagementRequired,
    InvitationsDisabled,
    SeatLimitReached,
    AlreadyMember,
    InvitationUnavailable,
    EmailDeliveryConfig,
    EmailDeliveryTransport,
    EmailDeliveryRejected(u16),
    RandomUnavailable,
    IdempotencyConflict,
    Store(StoreError),
    Json(serde_json::Error),
}

impl PartialEq for HubError {
    fn eq(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
            && match (self, other) {
                (Self::Authority(left), Self::Authority(right)) => left == right,
                (Self::Session(left), Self::Session(right)) => left == right,
                (Self::EmailDeliveryRejected(left), Self::EmailDeliveryRejected(right)) => {
                    left == right
                }
                _ => true,
            }
    }
}

impl Eq for HubError {}

impl From<AuthorityError> for HubError {
    fn from(error: AuthorityError) -> Self {
        Self::Authority(error)
    }
}

impl From<SessionError> for HubError {
    fn from(error: SessionError) -> Self {
        Self::Session(error)
    }
}

impl From<StoreError> for HubError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<serde_json::Error> for HubError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl fmt::Display for HubError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for HubError {}

const fn default_true() -> bool {
    true
}

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Postgres(postgres::Error),
    Sqlite(rusqlite::Error),
    EmbeddedDirectoryInUse(PathBuf),
    MigrationJournalMismatch(String),
    TransactionAborted,
    Poisoned,
}

impl From<std::io::Error> for StoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<postgres::Error> for StoreError {
    fn from(error: postgres::Error) -> Self {
        Self::Postgres(error)
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Postgres(error) => error.fmt(formatter),
            Self::Sqlite(error) => error.fmt(formatter),
            Self::EmbeddedDirectoryInUse(path) => write!(
                formatter,
                "embedded database directory is already in use: {}",
                path.display()
            ),
            Self::MigrationJournalMismatch(detail) => {
                write!(formatter, "embedded migration journal mismatch: {detail}")
            }
            Self::TransactionAborted => formatter.write_str("embedded transaction aborted"),
            Self::Poisoned => formatter.write_str("database client lock poisoned"),
        }
    }
}

impl std::error::Error for StoreError {}
