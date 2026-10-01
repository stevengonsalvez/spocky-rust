//! Offline GitHub webhook intake: signature verification, bounds, normalization, replay.
//!
//! Mirrors the baseline `createWebhookSource` over a caller-supplied [`WebhookBackend`]. The
//! durable receipt store behind that boundary is out of scope here; both sides of the differential
//! use the same stub (first delivery accepted, repeat delivery a duplicate).

use serde_json::{Number, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const MAX_WEBHOOK_BYTES: usize = 1_048_576;
const MAX_HEADER_LENGTH: usize = 128;

#[derive(Clone, Copy)]
pub struct GitHubWebhookRequest<'a> {
    pub delivery_id: Option<&'a str>,
    pub event_type: Option<&'a str>,
    pub signature: Option<&'a str>,
    pub body: &'a [u8],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebhookHttpResponse {
    pub status: u16,
    pub body: String,
}

/// What the endpoint handed to the acceptance boundary for one delivery.
#[derive(Clone, Debug, PartialEq)]
pub struct AcceptCall {
    pub delivery_id: String,
    pub source: String,
    pub drop_reason: Option<&'static str>,
    pub installation_id: Number,
    pub repository_id: Option<Number>,
    pub repo: Option<String>,
    pub signature_hash: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LifecycleCall {
    pub event: String,
    pub source: String,
    pub installation_id: Number,
    pub signature_hash: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcceptFailure {
    DatabaseUnavailable,
    Unexpected,
}

/// What the acceptance boundary decided for one delivery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Acceptance {
    /// A new delivery; `events` routed events go to every registered handler.
    Accepted {
        events: usize,
    },
    Duplicate,
    Dropped,
}

/// The boundary behind the endpoint: durable receipt acceptance, lifecycle application and
/// handler dispatch. The durable store is out of scope, so callers supply the implementation.
pub trait WebhookBackend {
    /// # Errors
    ///
    /// Returns the storage failure the baseline maps to 503 or 500.
    fn accept(&mut self, call: &AcceptCall) -> Result<Acceptance, AcceptFailure>;
    fn apply_lifecycle(&mut self, call: &LifecycleCall);
    /// Hands `events` accepted events to each of `handlers` registered handlers.
    fn dispatch(&mut self, events: usize, handlers: usize);
}

pub struct GitHubWebhook<B> {
    secret: Option<String>,
    handlers: usize,
    backend: B,
}

impl<B: WebhookBackend> GitHubWebhook<B> {
    /// `secret` is `None` when event triggers are not set up; every delivery is then refused.
    #[must_use]
    pub fn new(secret: Option<&str>, backend: B) -> Self {
        Self {
            secret: secret.map(str::to_owned),
            handlers: 0,
            backend,
        }
    }

    /// Registers one more downstream consumer of accepted events.
    pub fn start_handler(&mut self) {
        self.handlers += 1;
    }

    #[must_use]
    pub const fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub fn handle(&mut self, request: GitHubWebhookRequest<'_>) -> WebhookHttpResponse {
        let Some(secret) = self.secret.clone() else {
            return text(503, "Service Unavailable");
        };
        let Some(signature) = request.signature else {
            return text(401, "Unauthorized");
        };
        if request.body.len() > MAX_WEBHOOK_BYTES {
            return text(413, "Payload Too Large");
        }
        if !verify_github_signature(&secret, request.body, signature) {
            return text(401, "Unauthorized");
        }
        let signature_hash = hash_signature(signature);
        let (Some(delivery_id), Some(event_type)) = (
            bounded_header(request.delivery_id),
            bounded_header(request.event_type),
        ) else {
            return text(400, "Bad Request");
        };
        let payload = match parse_payload(request.body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let Some(installation_id) = payload
            .get("installation")
            .and_then(|installation| installation.get("id"))
            .and_then(number)
        else {
            return text(400, "Bad Request");
        };

        if event_type == "installation" || event_type == "installation_repositories" {
            self.backend.apply_lifecycle(&LifecycleCall {
                event: event_type.to_owned(),
                source: format!("github.{event_type}"),
                installation_id,
                signature_hash,
            });
            return text(200, "OK");
        }

        let repo = payload
            .get("repository")
            .and_then(|repository| repository.get("full_name"))
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty());
        let repository_id = payload
            .get("repository")
            .and_then(|repository| repository.get("id"))
            .and_then(number);
        let normalized = repo.zip(repository_id);
        let drop_reason = match (&normalized, self.handlers) {
            (None, _) => Some("no_trigger_for_source"),
            (Some(_), 0) => Some("configuration_unavailable"),
            (Some(_), _) => None,
        };
        let (repo, repository_id) =
            normalized.map_or((None, None), |(repo, id)| (Some(repo.to_owned()), Some(id)));
        let call = AcceptCall {
            delivery_id: delivery_id.to_owned(),
            source: format!("github.{event_type}"),
            drop_reason,
            installation_id,
            repository_id,
            repo,
            signature_hash,
        };
        match self.backend.accept(&call) {
            Err(AcceptFailure::DatabaseUnavailable) => {
                text(503, "{\"error\":\"database_unavailable\"}")
            }
            Err(AcceptFailure::Unexpected) => {
                text(500, "{\"error\":\"webhook_processing_failed\"}")
            }
            Ok(Acceptance::Accepted { events }) => {
                self.backend.dispatch(events, self.handlers);
                text(200, "OK")
            }
            Ok(Acceptance::Duplicate | Acceptance::Dropped) => text(200, "OK"),
        }
    }
}

fn text(status: u16, body: &str) -> WebhookHttpResponse {
    WebhookHttpResponse {
        status,
        body: body.to_owned(),
    }
}

fn number(value: &Value) -> Option<Number> {
    match value {
        Value::Number(number) => Some(number.clone()),
        _ => None,
    }
}

/// Header values are byte strings, so length is the byte length (ASCII headers only).
fn bounded_header(value: Option<&str>) -> Option<&str> {
    value.filter(|header| !header.is_empty() && header.len() <= MAX_HEADER_LENGTH)
}

fn parse_payload(body: &[u8]) -> Result<Value, WebhookHttpResponse> {
    let invalid_json = || text(400, "{\"error\":\"request body must be valid JSON\"}");
    let decoded = std::str::from_utf8(body).map_err(|_| invalid_json())?;
    let decoded = decoded.strip_prefix('\u{feff}').unwrap_or(decoded);
    let value: Value = serde_json::from_str(decoded).map_err(|_| invalid_json())?;
    if value.is_object() {
        return Ok(value);
    }
    Err(text(
        400,
        &format!(
            "{{\"error\":\"invalid webhook payload\",\"issues\":{{\"_errors\":[\"Invalid input: expected object, received {}\"]}}}}",
            json_type_name(&value)
        ),
    ))
}

/// Type names as the baseline schema library words them in its first issue.
#[must_use]
pub fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[must_use]
pub fn github_signature(secret: &str, body: &[u8]) -> String {
    const BLOCK_BYTES: usize = 64;
    let mut key = [0_u8; BLOCK_BYTES];
    if secret.len() > BLOCK_BYTES {
        key[..32].copy_from_slice(&Sha256::digest(secret.as_bytes()));
    } else {
        key[..secret.len()].copy_from_slice(secret.as_bytes());
    }
    let mut inner_pad = [0x36_u8; BLOCK_BYTES];
    let mut outer_pad = [0x5c_u8; BLOCK_BYTES];
    for index in 0..BLOCK_BYTES {
        inner_pad[index] ^= key[index];
        outer_pad[index] ^= key[index];
    }
    let inner = Sha256::new()
        .chain_update(inner_pad)
        .chain_update(body)
        .finalize();
    let digest = Sha256::new()
        .chain_update(outer_pad)
        .chain_update(inner)
        .finalize();
    format!("sha256={digest:x}")
}

#[must_use]
pub fn verify_github_signature(secret: &str, body: &[u8], signature: &str) -> bool {
    let expected = github_signature(secret, body);
    expected.len() == signature.len() && bool::from(expected.as_bytes().ct_eq(signature.as_bytes()))
}

/// The stored dedupe evidence: SHA-256 of the signature header text.
#[must_use]
pub fn hash_signature(signature: &str) -> String {
    format!("{:x}", Sha256::digest(signature.as_bytes()))
}
