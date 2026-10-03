//! CLI device authorization (`src/cli-authorizations/index.ts`) and its in-memory state machine
//! (`MemoryDatabase.*CliAuthorization` in `src/db/memory.ts`).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::rc::Rc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Url;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization as _;

use super::contracts::{
    is_empty_object, iso_string, parse_decision, parse_device_code, parse_user_code,
};
use super::credentials::{
    CLI_CREDENTIAL_PREFIX, CliCredentialRecord, CliCredentialStore, cli_credential_parts,
    hash_secret,
};
use super::message::{ApiRequest, ApiResponse};
use super::validation::ParseFailure;
use super::value::{JsValueExt as _, Json, decode_request_json};

const LIFETIME_SECONDS: u64 = 10 * 60;
const INITIAL_POLL_INTERVAL_SECONDS: u64 = 5;
const PER_FINGERPRINT_LIMIT: usize = 5;
const GLOBAL_LIMIT: usize = 1_000;
/// Header the baseline's node server sets from the peer address. Rust passes the address to
/// [`CliAuthorizations::start`] instead of reading this header.
pub const CLIENT_ADDRESS_HEADER: &str = "x-paseo-client-address";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationStatus {
    Pending,
    Approved,
    Denied,
    Expired,
    Disclosed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliAuthorizationRecord {
    pub id: String,
    pub status: AuthorizationStatus,
    pub poll_interval_seconds: u64,
    pub approved_organization_id: Option<String>,
    pub approved_by_user_id: Option<String>,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartInput {
    pub id: String,
    pub device_verifier: String,
    pub user_code_verifier: String,
    pub fingerprint_verifier: String,
    pub lifetime_seconds: u64,
    pub poll_interval_seconds: u64,
    pub per_fingerprint_limit: usize,
    pub global_limit: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CredentialInput {
    pub id: String,
    pub prefix: String,
    pub verifier: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PollOutcome {
    Pending {
        interval_seconds: u64,
    },
    SlowDown {
        interval_seconds: u64,
    },
    Authorized {
        interval_seconds: u64,
        organization_id: String,
    },
    Denied {
        interval_seconds: u64,
    },
    Expired {
        interval_seconds: u64,
    },
    Disclosed {
        interval_seconds: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecisionAccess {
    pub session_id: String,
    pub user_id: String,
    pub membership_id: String,
    pub organization_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecisionOutcome {
    Approved,
    Denied,
    Unavailable,
    Forbidden,
}

/// The state the authorization endpoints keep. Time comes from the store.
pub trait CliAuthorizationStore {
    fn start(&self, input: &StartInput) -> Option<CliAuthorizationRecord>;
    fn inspect(&self, user_code_verifier: &str) -> Option<CliAuthorizationRecord>;
    fn decide(
        &self,
        user_code_verifier: &str,
        approve: bool,
        access: &DecisionAccess,
    ) -> DecisionOutcome;
    fn poll(&self, device_verifier: &str, credential: &CredentialInput) -> PollOutcome;
}

struct StoredAuthorization {
    record: CliAuthorizationRecord,
    user_code_verifier: String,
    fingerprint_verifier: String,
    next_poll_at_ms: i64,
}

/// Epoch milliseconds source.
pub type Clock = Rc<dyn Fn() -> i64>;

/// In-memory authorizations and credentials, mirroring the baseline's memory database.
pub struct MemoryCliAuthorizations {
    now: Clock,
    state: RefCell<MemoryState>,
}

#[derive(Default)]
struct MemoryState {
    /// Keyed by device verifier and kept in insertion order, like the baseline `Map`: setting an
    /// existing key replaces its value in place, and a lookup by user code finds the oldest match.
    /// Records are never removed; the baseline has no code path that prunes them.
    // ponytail: unbounded and scanned linearly, as the baseline Map is unbounded; only active
    // records count against the limits and a lookup is O(records). Add a device and user code
    // index (keeping first-insertion order) if a long-running process needs it.
    authorizations: Vec<(String, StoredAuthorization)>,
    /// Credentials created when an approved authorization was first polled, keyed by prefix. Only
    /// looked up by prefix or searched for a boolean, never listed, so key order is not observable.
    credentials: BTreeMap<String, CliCredentialRecord>,
}

impl MemoryCliAuthorizations {
    #[must_use]
    pub fn new(now: Clock) -> Self {
        Self {
            now,
            state: RefCell::new(MemoryState::default()),
        }
    }

    /// Revokes the credential with the given prefix; false when there is none.
    pub fn revoke_credential(&self, prefix: &str) -> bool {
        match self.state.borrow_mut().credentials.get_mut(prefix) {
            Some(record) => {
                record.revoked = true;
                true
            }
            None => false,
        }
    }

    fn now(&self) -> i64 {
        (self.now)()
    }
}

#[allow(clippy::cast_possible_wrap)]
fn millis(seconds: u64) -> i64 {
    seconds as i64 * 1000
}

impl CliAuthorizationStore for MemoryCliAuthorizations {
    fn start(&self, input: &StartInput) -> Option<CliAuthorizationRecord> {
        let now = self.now();
        let mut state = self.state.borrow_mut();
        let active: Vec<&StoredAuthorization> = state
            .authorizations
            .iter()
            .map(|(_, stored)| stored)
            .filter(|stored| {
                matches!(
                    stored.record.status,
                    AuthorizationStatus::Pending | AuthorizationStatus::Approved
                ) && stored.record.expires_at_ms > now
            })
            .collect();
        let fingerprint_count = active
            .iter()
            .filter(|stored| stored.fingerprint_verifier == input.fingerprint_verifier)
            .count();
        if fingerprint_count >= input.per_fingerprint_limit || active.len() >= input.global_limit {
            return None;
        }
        let record = CliAuthorizationRecord {
            id: input.id.clone(),
            status: AuthorizationStatus::Pending,
            poll_interval_seconds: input.poll_interval_seconds,
            approved_organization_id: None,
            approved_by_user_id: None,
            created_at_ms: now,
            expires_at_ms: now + millis(input.lifetime_seconds),
        };
        let stored = StoredAuthorization {
            record: record.clone(),
            user_code_verifier: input.user_code_verifier.clone(),
            fingerprint_verifier: input.fingerprint_verifier.clone(),
            next_poll_at_ms: now,
        };
        match state
            .authorizations
            .iter_mut()
            .find(|(device, _)| *device == input.device_verifier)
        {
            Some(slot) => slot.1 = stored,
            None => state
                .authorizations
                .push((input.device_verifier.clone(), stored)),
        }
        Some(record)
    }

    fn inspect(&self, user_code_verifier: &str) -> Option<CliAuthorizationRecord> {
        let now = self.now();
        let mut state = self.state.borrow_mut();
        let stored = state
            .authorizations
            .iter_mut()
            .map(|(_, stored)| stored)
            .find(|stored| stored.user_code_verifier == user_code_verifier)?;
        if stored.record.expires_at_ms <= now {
            stored.record.status = AuthorizationStatus::Expired;
        }
        (stored.record.status == AuthorizationStatus::Pending).then(|| stored.record.clone())
    }

    fn decide(
        &self,
        user_code_verifier: &str,
        approve: bool,
        access: &DecisionAccess,
    ) -> DecisionOutcome {
        let now = self.now();
        let mut state = self.state.borrow_mut();
        let Some(stored) = state
            .authorizations
            .iter_mut()
            .map(|(_, stored)| stored)
            .find(|stored| stored.user_code_verifier == user_code_verifier)
        else {
            return DecisionOutcome::Unavailable;
        };
        if stored.record.expires_at_ms <= now {
            stored.record.status = AuthorizationStatus::Expired;
        }
        if stored.record.status != AuthorizationStatus::Pending {
            return DecisionOutcome::Unavailable;
        }
        if approve {
            stored.record.status = AuthorizationStatus::Approved;
            stored.record.approved_organization_id = Some(access.organization_id.clone());
            stored.record.approved_by_user_id = Some(access.user_id.clone());
            DecisionOutcome::Approved
        } else {
            stored.record.status = AuthorizationStatus::Denied;
            DecisionOutcome::Denied
        }
    }

    fn poll(&self, device_verifier: &str, credential: &CredentialInput) -> PollOutcome {
        let now = self.now();
        let mut state = self.state.borrow_mut();
        let MemoryState {
            authorizations,
            credentials,
        } = &mut *state;
        let Some(stored) = authorizations
            .iter_mut()
            .find(|(device, _)| device == device_verifier)
            .map(|(_, stored)| stored)
        else {
            return PollOutcome::Expired {
                interval_seconds: INITIAL_POLL_INTERVAL_SECONDS,
            };
        };
        if stored.record.expires_at_ms <= now {
            stored.record.status = AuthorizationStatus::Expired;
        }
        let interval_seconds = stored.record.poll_interval_seconds;
        match stored.record.status {
            AuthorizationStatus::Expired => return PollOutcome::Expired { interval_seconds },
            AuthorizationStatus::Denied => return PollOutcome::Denied { interval_seconds },
            AuthorizationStatus::Disclosed => return PollOutcome::Disclosed { interval_seconds },
            AuthorizationStatus::Pending | AuthorizationStatus::Approved => {}
        }
        if stored.next_poll_at_ms > now {
            stored.record.poll_interval_seconds += 5;
            stored.next_poll_at_ms = now + millis(stored.record.poll_interval_seconds);
            return PollOutcome::SlowDown {
                interval_seconds: stored.record.poll_interval_seconds,
            };
        }
        stored.next_poll_at_ms = now + millis(interval_seconds);
        if stored.record.status == AuthorizationStatus::Approved {
            let organization_id = stored
                .record
                .approved_organization_id
                .clone()
                .unwrap_or_default();
            credentials.insert(
                credential.prefix.clone(),
                CliCredentialRecord {
                    id: credential.id.clone(),
                    organization_id: organization_id.clone(),
                    verifier: credential.verifier.clone(),
                    revoked: false,
                },
            );
            stored.record.status = AuthorizationStatus::Disclosed;
            return PollOutcome::Authorized {
                interval_seconds,
                organization_id,
            };
        }
        PollOutcome::Pending { interval_seconds }
    }
}

impl CliCredentialStore for MemoryCliAuthorizations {
    fn find_by_prefix(&self, prefix: &str) -> Option<CliCredentialRecord> {
        self.state.borrow().credentials.get(prefix).cloned()
    }

    fn touch(&self, id: &str) -> bool {
        self.state
            .borrow()
            .credentials
            .values()
            .any(|record| record.id == id && !record.revoked)
    }
}

/// An organization member's browser session, as `resolveOrganizationAccess` returns it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrganizationAccess {
    pub session_id: String,
    pub account_id: String,
    pub organization_id: String,
    pub organization_name: String,
    pub organization_slug: String,
    pub membership_id: String,
    pub manage_resources: bool,
}

/// Why browser access could not be resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AccessFailure {
    /// `ProductRequestError`: answered with `{"error": code}` at `status`.
    Product { status: u16, code: String },
    /// Any other thrown error; it propagates out of the handler.
    Failed(String),
}

pub trait BrowserAccess {
    /// A response when the cookie mutation check rejects the request.
    fn reject_cookie_mutation(&self, request: &ApiRequest) -> Option<ApiResponse>;
    /// # Errors
    ///
    /// Returns [`AccessFailure`] when no organization access can be resolved.
    fn resolve_organization_access(
        &self,
        request: &ApiRequest,
    ) -> Result<OrganizationAccess, AccessFailure>;
}

/// An error thrown out of a handler instead of a response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandlerError(pub String);

impl fmt::Display for HandlerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for HandlerError {}

/// Source of random bytes (`randomBytes`).
pub type RandomBytes = Box<dyn FnMut(usize) -> Vec<u8>>;
/// The production random byte source: the operating system generator.
///
/// # Panics
///
/// The returned closure panics if the system generator fails, as Node's `randomBytes` throws.
#[must_use]
pub fn os_random_bytes() -> RandomBytes {
    Box::new(|size| {
        let mut bytes = vec![0_u8; size];
        getrandom::fill(&mut bytes).expect("system random bytes are available");
        bytes
    })
}

/// Source of generated identifiers (`randomUUID`).
pub type UuidSource = Box<dyn FnMut() -> String>;

pub struct CliAuthorizations {
    store: Rc<dyn CliAuthorizationStore>,
    access: Option<Box<dyn BrowserAccess>>,
    public_base_url: Option<String>,
    random_bytes: RandomBytes,
    ids: UuidSource,
}

impl CliAuthorizations {
    #[must_use]
    pub fn new(
        store: Rc<dyn CliAuthorizationStore>,
        access: Option<Box<dyn BrowserAccess>>,
        public_base_url: Option<String>,
        random_bytes: RandomBytes,
        ids: UuidSource,
    ) -> Self {
        Self {
            store,
            access,
            public_base_url,
            random_bytes,
            ids,
        }
    }

    /// `POST /api/v1/cli-authorizations`.
    ///
    /// `client_address` is the caller's peer address, taken from the connection by the server
    /// wiring (the baseline's node server overwrites `x-paseo-client-address` the same way). It
    /// is the per-client capacity key; `None` is the baseline's `unknown`. A
    /// `x-paseo-client-address` header on the request is ignored, so a client cannot choose its
    /// own key.
    ///
    /// # Errors
    ///
    /// Returns [`HandlerError`] when the verification URI cannot be built.
    pub fn start(
        &mut self,
        request: &ApiRequest,
        client_address: Option<&str>,
    ) -> Result<ApiResponse, HandlerError> {
        if !parsed_json(request).is_some_and(|body| is_empty_object(&body)) {
            return Ok(invalid_request());
        }
        let device_code = URL_SAFE_NO_PAD.encode((self.random_bytes)(32));
        let user_code = format_user_code(&base32(&(self.random_bytes)(8)));
        let fingerprint = client_address.unwrap_or("unknown");
        let input = StartInput {
            id: (self.ids)(),
            device_verifier: hash_secret(&device_code),
            user_code_verifier: hash_secret(&normalize_user_code(&user_code)),
            fingerprint_verifier: hash_secret(fingerprint),
            lifetime_seconds: LIFETIME_SECONDS,
            poll_interval_seconds: INITIAL_POLL_INTERVAL_SECONDS,
            per_fingerprint_limit: PER_FINGERPRINT_LIMIT,
            global_limit: GLOBAL_LIMIT,
        };
        let Some(authorization) = self.store.start(&input) else {
            return Ok(ApiResponse::json(
                429,
                &Json::object([
                    ("status", Json::string("retry_later")),
                    ("interval", Json::integer(5)),
                ])
                .stringify(),
                &[("retry-after", "5")],
            ));
        };
        let base = self
            .public_base_url
            .as_deref()
            .unwrap_or_else(|| request.url.as_str());
        let invalid = || HandlerError("Invalid URL".to_owned());
        let verification_uri = Url::parse(base)
            .and_then(|base| base.join("/cli-login"))
            .map_err(|_| invalid())?;
        let mut complete = verification_uri.clone();
        complete.query_pairs_mut().append_pair("code", &user_code);
        let expires_at = iso_string(authorization.expires_at_ms).ok_or_else(invalid)?;
        let body = Json::object([
            ("deviceCode", Json::string(&device_code)),
            ("userCode", Json::string(&user_code)),
            ("verificationUri", Json::string(verification_uri.as_str())),
            ("verificationUriComplete", Json::string(complete.as_str())),
            ("expiresAt", Json::String(expires_at)),
            (
                "interval",
                Json::integer(i64::try_from(authorization.poll_interval_seconds).unwrap_or(0)),
            ),
        ]);
        Ok(ApiResponse::json(201, &body.stringify(), &[]))
    }

    /// `POST /api/v1/cli-authorizations/poll`.
    ///
    /// # Errors
    ///
    /// Returns [`HandlerError`] when body validation throws, which the baseline does not catch.
    pub fn poll(&mut self, request: &ApiRequest) -> Result<ApiResponse, HandlerError> {
        let Some(device_code) = safe_parse(request, parse_device_code)? else {
            return Ok(invalid_request());
        };
        let credential = derive_credential(&device_code);
        let (prefix, verifier) = cli_credential_parts(&credential).unwrap_or_default();
        let outcome = self.store.poll(
            &hash_secret(&device_code),
            &CredentialInput {
                id: (self.ids)(),
                prefix,
                verifier,
            },
        );
        let (status, interval, authorized) = match outcome {
            PollOutcome::Pending { interval_seconds } => ("pending", interval_seconds, None),
            PollOutcome::SlowDown { interval_seconds } => ("slow_down", interval_seconds, None),
            PollOutcome::Denied { interval_seconds } => ("denied", interval_seconds, None),
            PollOutcome::Expired { interval_seconds } => ("expired", interval_seconds, None),
            PollOutcome::Disclosed { interval_seconds } => ("disclosed", interval_seconds, None),
            PollOutcome::Authorized {
                interval_seconds,
                organization_id,
            } => ("authorized", interval_seconds, Some(organization_id)),
        };
        let mut fields = vec![
            ("status".to_owned(), Json::string(status)),
            (
                "interval".to_owned(),
                Json::integer(i64::try_from(interval).unwrap_or(0)),
            ),
        ];
        if let Some(organization_id) = authorized {
            fields.push(("credential".to_owned(), Json::String(credential)));
            fields.push(("organizationId".to_owned(), Json::String(organization_id)));
        }
        Ok(ApiResponse::json(
            200,
            &Json::from_pairs(fields).stringify(),
            &[],
        ))
    }

    /// `POST /cli-authorizations/inspect`.
    ///
    /// # Errors
    ///
    /// Returns [`HandlerError`] when browser access fails with an unexpected error.
    pub fn inspect(&mut self, request: &ApiRequest) -> Result<ApiResponse, HandlerError> {
        let access = match self.browser_access(request)? {
            Ok(access) => access,
            Err(response) => return Ok(response),
        };
        let Some(user_code) = safe_parse(request, parse_user_code)? else {
            return Ok(invalid_request());
        };
        let Some(authorization) = self
            .store
            .inspect(&hash_secret(&normalize_user_code(&user_code)))
        else {
            return Ok(unavailable());
        };
        let expires_at = iso_string(authorization.expires_at_ms)
            .ok_or_else(|| HandlerError("Invalid time value".to_owned()))?;
        let body = Json::object([
            ("expiresAt", Json::String(expires_at)),
            (
                "organization",
                Json::object([
                    ("id", Json::string(&access.organization_id)),
                    ("name", Json::string(&access.organization_name)),
                    ("slug", Json::string(&access.organization_slug)),
                ]),
            ),
            ("canManage", Json::Bool(access.manage_resources)),
        ]);
        Ok(ApiResponse::json(200, &body.stringify(), &[]))
    }

    /// `POST /cli-authorizations/decision`.
    ///
    /// # Errors
    ///
    /// Returns [`HandlerError`] when browser access fails with an unexpected error.
    pub fn decide(&mut self, request: &ApiRequest) -> Result<ApiResponse, HandlerError> {
        let access = match self.browser_access(request)? {
            Ok(access) => access,
            Err(response) => return Ok(response),
        };
        if !access.manage_resources {
            return Ok(error_response(403, "forbidden"));
        }
        let Some(decision) = safe_parse(request, parse_decision)? else {
            return Ok(invalid_request());
        };
        if decision.organization_id != access.organization_id {
            return Ok(error_response(403, "organization_required"));
        }
        let outcome = self.store.decide(
            &hash_secret(&normalize_user_code(&decision.user_code)),
            decision.approve,
            &DecisionAccess {
                session_id: access.session_id,
                user_id: access.account_id,
                membership_id: access.membership_id,
                organization_id: access.organization_id,
            },
        );
        Ok(match outcome {
            DecisionOutcome::Unavailable => unavailable(),
            DecisionOutcome::Forbidden => error_response(403, "organization_required"),
            DecisionOutcome::Approved => status_response("approved"),
            DecisionOutcome::Denied => status_response("denied"),
        })
    }

    /// `Ok(Ok(access))` to continue, `Ok(Err(response))` to answer, `Err` to throw.
    fn browser_access(
        &self,
        request: &ApiRequest,
    ) -> Result<Result<OrganizationAccess, ApiResponse>, HandlerError> {
        let Some(access) = &self.access else {
            return Ok(Err(error_response(503, "auth_unavailable")));
        };
        if let Some(rejected) = access.reject_cookie_mutation(request) {
            return Ok(Err(rejected));
        }
        match access.resolve_organization_access(request) {
            Ok(resolved) => Ok(Ok(resolved)),
            Err(AccessFailure::Product { status, code }) => Ok(Err(error_response(status, &code))),
            Err(AccessFailure::Failed(message)) => Err(HandlerError(message)),
        }
    }
}

fn parsed_json(request: &ApiRequest) -> Option<Json> {
    decode_request_json(&request.body)
}

/// What V8 throws when zod coerces a `length` array nested beyond the call stack.
const STACK_OVERFLOW: &str = "Maximum call stack size exceeded";

/// `schema.safeParse(await this.parsedJson(request, ...))`: `Ok(None)` for a body that is not JSON
/// or fails the schema, and the handler failing when `safeParse` itself throws.
fn safe_parse<T>(
    request: &ApiRequest,
    parse: impl FnOnce(&Json) -> Result<T, ParseFailure>,
) -> Result<Option<T>, HandlerError> {
    let Some(body) = parsed_json(request) else {
        return Ok(None);
    };
    match parse(&body) {
        Ok(parsed) => Ok(Some(parsed)),
        Err(ParseFailure::Invalid(_)) => Ok(None),
        Err(ParseFailure::Thrown) => Err(HandlerError(STACK_OVERFLOW.to_owned())),
    }
}

fn error_response(status: u16, code: &str) -> ApiResponse {
    ApiResponse::json(
        status,
        &Json::object([("error", Json::string(code))]).stringify(),
        &[],
    )
}

fn invalid_request() -> ApiResponse {
    error_response(400, "invalid_request")
}

fn unavailable() -> ApiResponse {
    error_response(404, "authorization_unavailable")
}

fn status_response(status: &str) -> ApiResponse {
    ApiResponse::json(
        200,
        &Json::object([("status", Json::string(status))]).stringify(),
        &[],
    )
}

/// NFKC, upper case, then drop everything outside `A-Z2-7`.
#[must_use]
pub fn normalize_user_code(value: &str) -> String {
    value
        .nfkc()
        .collect::<String>()
        .to_uppercase()
        .chars()
        .filter(|ch| matches!(ch, 'A'..='Z' | '2'..='7'))
        .collect()
}

fn derive_credential(device_code: &str) -> String {
    let prefix_hash = Sha256::new()
        .chain_update(b"paseo-cli-prefix\0")
        .chain_update(device_code.as_bytes())
        .finalize();
    let secret = Sha256::new()
        .chain_update(b"paseo-cli-credential\0")
        .chain_update(device_code.as_bytes())
        .finalize();
    let prefix = &URL_SAFE_NO_PAD.encode(prefix_hash)[..12];
    format!(
        "{CLI_CREDENTIAL_PREFIX}{prefix}_{}",
        URL_SAFE_NO_PAD.encode(secret)
    )
}

fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut bits = 0_u32;
    let mut buffer = 0_u32;
    let mut result = String::new();
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            result.push(char::from(ALPHABET[((buffer >> bits) & 31) as usize]));
        }
        buffer &= (1 << bits) - 1;
    }
    if bits > 0 {
        result.push(char::from(ALPHABET[((buffer << (5 - bits)) & 31) as usize]));
    }
    result
}

fn format_user_code(value: &str) -> String {
    format!("{}-{}-{}", &value[..4], &value[4..8], &value[8..])
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::{
        CliAuthorizationStore, DecisionAccess, DecisionOutcome, MemoryCliAuthorizations,
        PollOutcome, StartInput, base32, format_user_code, normalize_user_code, os_random_bytes,
    };
    use super::{CredentialInput, hash_secret};

    fn start_input(id: &str, device: &str, user: &str) -> StartInput {
        StartInput {
            id: id.to_owned(),
            device_verifier: hash_secret(device),
            user_code_verifier: hash_secret(user),
            fingerprint_verifier: hash_secret(device),
            lifetime_seconds: 600,
            poll_interval_seconds: 5,
            per_fingerprint_limit: 5,
            global_limit: 1000,
        }
    }

    fn access() -> DecisionAccess {
        DecisionAccess {
            session_id: "session".to_owned(),
            user_id: "user".to_owned(),
            membership_id: "member".to_owned(),
            organization_id: "org".to_owned(),
        }
    }

    fn credential() -> CredentialInput {
        CredentialInput {
            id: "credential".to_owned(),
            prefix: "paseo_cli_aaaaaaaaaaaa".to_owned(),
            verifier: hash_secret("secret"),
        }
    }

    #[test]
    fn a_user_code_collision_resolves_to_the_oldest_record_like_a_js_map() {
        let store = MemoryCliAuthorizations::new(Rc::new(|| 0));
        // Device verifiers are hashes, so their sorted order is unrelated to insertion order. Try
        // several pairs so at least one has the later record sorting first.
        for pair in 0..8 {
            let first = format!("device-{pair}-first");
            let second = format!("device-{pair}-second");
            let user = format!("user-{pair}");
            store
                .start(&start_input("a", &first, &user))
                .expect("first");
            store
                .start(&start_input("b", &second, &user))
                .expect("second");
            assert_eq!(
                store.decide(&hash_secret(&user), true, &access()),
                DecisionOutcome::Approved
            );
            assert!(matches!(
                store.poll(&hash_secret(&first), &credential()),
                PollOutcome::Authorized { .. }
            ));
            assert!(matches!(
                store.poll(&hash_secret(&second), &credential()),
                PollOutcome::Pending { .. }
            ));
        }
    }

    #[test]
    fn expired_records_are_kept_like_the_baseline_keeps_them() {
        let clock = Rc::new(Cell::new(0_i64));
        let store = MemoryCliAuthorizations::new({
            let clock = Rc::clone(&clock);
            Rc::new(move || clock.get())
        });
        store
            .start(&start_input("a", "device-a", "user-a"))
            .expect("start");
        store.poll(&hash_secret("device-a"), &credential());
        // A second poll before the interval passes raises the interval to 10 seconds.
        assert_eq!(
            store.poll(&hash_secret("device-a"), &credential()),
            PollOutcome::SlowDown {
                interval_seconds: 10
            }
        );
        clock.set(601_000);
        for index in 0..20 {
            store
                .start(&start_input(
                    "x",
                    &format!("other-{index}"),
                    &format!("other-user-{index}"),
                ))
                .expect("start");
        }
        // A pruned record would answer as an unknown code with the default interval of 5.
        assert_eq!(
            store.poll(&hash_secret("device-a"), &credential()),
            PollOutcome::Expired {
                interval_seconds: 10
            }
        );
    }

    #[test]
    fn system_random_bytes_have_the_requested_length_and_differ() {
        let mut draw = os_random_bytes();
        let first = draw(32);
        assert_eq!(first.len(), 32);
        assert_ne!(first, draw(32));
        assert_eq!(draw(8).len(), 8);
    }

    #[test]
    fn user_codes_are_base32_and_normalize_compatibility_characters() {
        let code = format_user_code(&base32(&[0xde, 0xad, 0xbe, 0xef, 0x00, 0x11, 0x22, 0x33]));
        assert_eq!(code, "32W3-53YA-CERDG");
        assert_eq!(
            normalize_user_code("\u{ff33}\u{212a}-ab 0 1 \u{b2}"),
            "SKAB2"
        );
    }
}
