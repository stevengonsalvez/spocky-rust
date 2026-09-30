//! Contract pilot for Hub authority, daemon registration, and durable restart behavior.
//!
//! This crate intentionally does not implement `PGlite` or `PostgreSQL` semantics.

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

mod api_keys;
pub mod billing;
pub mod daemon_socket;
mod email_delivery;
pub mod http;
mod invitations;
mod relational_api_keys;
mod relational_invitations;
mod relational_sessions;

pub use api_keys::{ApiKeyAccess, ApiKeyAuthorization, ApiKeyScope, ApiKeySummary, CreatedApiKey};
pub use email_delivery::{ResendConfig, ResendEmailDelivery};
pub use invitations::{
    InvitationEmail, InvitationEmailMessage, InvitationRole, InvitationSummary,
    render_invitation_email,
};
pub use relational_api_keys::PostgresApiKeyStore;
pub use relational_invitations::PostgresInvitationStore;
pub use relational_sessions::PostgresSessionStore;

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
            "CREATE TABLE IF NOT EXISTS paseo_hub_pilot_state (
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
            "SELECT state_bytes FROM paseo_hub_pilot_state WHERE state_key = $1",
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
            "INSERT INTO paseo_hub_pilot_state (state_key, state_bytes, revision)
             VALUES ($1, $2, 1)
             ON CONFLICT (state_key) DO UPDATE
             SET state_bytes = EXCLUDED.state_bytes,
                 revision = paseo_hub_pilot_state.revision + 1",
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
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredRegistration {
    result: DaemonRegistration,
    idempotency_key: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct HubState {
    instance_secret_fingerprint: Option<u64>,
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
        self.state.accounts.insert(
            input.owner.clone(),
            Account {
                password_fingerprint: fingerprint(&input.temporary_password),
                must_change_password: true,
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

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Postgres(postgres::Error),
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

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Postgres(error) => error.fmt(formatter),
            Self::Poisoned => formatter.write_str("PostgreSQL client lock poisoned"),
        }
    }
}

impl std::error::Error for StoreError {}
