//! Durable message receipts: first delivery, duplicate suppression, restart,
//! corruption, write failure, and unknown outcomes, following pinned Paseo
//! `5de45e2` `packages/server/src/server/message-receipts/index.ts`.
//!
//! Each `(agentId, messageId)` owns `<directory>/<sha256>.json`, written
//! atomically as `pending` before the provider send and `completed` after it.
//! A retry of a completed receipt is suppressed; a retry of a pending one
//! rejects with `agent_request_outcome_unknown`.
//!
//! Known pinned defect, kept on purpose: when the `completed` write fails
//! after a successful send, the receipt stays `pending`, so every later
//! retry of a delivered message rejects as an unknown outcome.

pub mod node_fs;

use std::collections::HashMap;
use std::fmt::{self, Debug, Display, Formatter};
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use sha2::{Digest, Sha256};
use spocky_contracts::js_value::{
    JsObject, JsValue, JsonSyntaxError, parse, stringify, stringify_pretty,
};
use spocky_contracts::zod::{Schema, UnknownKeys, Verdict, verdict};
use spocky_store::collate::locale_compare;

use crate::node_fs::FsError;

/// The provider side of one message: `prepare` (optional in the baseline,
/// a no-op by default) runs before the `pending` receipt, `send` after it.
pub trait Delivery {
    type Error;

    /// `input.prepare?.()`.
    fn prepare(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        async { Ok(()) }
    }

    /// `input.send()`.
    fn send(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

/// A rejection of [`MessageReceipts::send`], with the baseline's text.
#[derive(Debug)]
pub enum ReceiptError<E> {
    /// `Error("agent_request_key_conflict")`: same ids, different request.
    KeyConflict,
    /// `Error("agent_request_outcome_unknown")`: a `pending` receipt exists.
    OutcomeUnknown,
    /// A node filesystem error from reading or writing the receipt.
    Fs(FsError),
    /// The `JSON.parse` `SyntaxError` of a corrupt receipt.
    Syntax(JsonSyntaxError),
    /// The `ZodError` of a receipt with the wrong shape; holds its message.
    Schema(String),
    /// Whatever `prepare` or `send` rejected with, unchanged.
    Delivery(E),
}

impl<E: Display> Display for ReceiptError<E> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyConflict => formatter.write_str("agent_request_key_conflict"),
            Self::OutcomeUnknown => formatter.write_str("agent_request_outcome_unknown"),
            Self::Fs(error) => Display::fmt(error, formatter),
            Self::Syntax(error) => Display::fmt(error, formatter),
            Self::Schema(message) => formatter.write_str(message),
            Self::Delivery(error) => Display::fmt(error, formatter),
        }
    }
}

impl<E: Debug + Display> std::error::Error for ReceiptError<E> {}

/// Owns message delivery receipts; creation is owned by `CreationService`.
pub struct MessageReceipts {
    directory: String,
    pending: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl MessageReceipts {
    /// `new MessageReceipts(directory)`.
    #[must_use]
    pub fn new(directory: impl Into<String>) -> Self {
        Self {
            directory: directory.into(),
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// `send(input)`: sends of one `(agent_id, message_id)` on this instance
    /// run one at a time, whatever the previous outcome.
    ///
    /// The baseline queues a send when `send` is called; this future joins
    /// the queue when it is first polled, so call order equals queue order
    /// only for futures polled in call order (as `tokio::join!` does).
    /// Queues are per instance: two instances on one directory can both
    /// deliver the same message, as in the baseline.
    ///
    /// # Errors
    ///
    /// Rejects as the baseline does; see [`ReceiptError`].
    pub async fn send<D: Delivery>(
        &self,
        agent_id: &str,
        message_id: &str,
        request: &JsValue,
        delivery: D,
    ) -> Result<(), ReceiptError<D::Error>> {
        // Preserve the existing on-disk identity and shape across daemon upgrades.
        let key = digest(&JsValue::Array(vec![
            JsValue::String("send".to_owned()),
            JsValue::String(agent_id.to_owned()),
            JsValue::String(message_id.to_owned()),
        ]));
        let turn = Arc::clone(self.pending_map().entry(key.clone()).or_default());
        let result = {
            let _turn = turn.lock().await;
            self.send_once(&key, agent_id, request, delivery).await
        };
        let mut pending = self.pending_map();
        // Only the map and this call hold the turn: nobody is queued behind it.
        if Arc::strong_count(&turn) == 2 {
            pending.remove(&key);
        }
        result
    }

    fn pending_map(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, Arc<tokio::sync::Mutex<()>>>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn send_once<D: Delivery>(
        &self,
        key: &str,
        agent_id: &str,
        request: &JsValue,
        mut delivery: D,
    ) -> Result<(), ReceiptError<D::Error>> {
        let file = node_fs::join(&self.directory, &format!("{key}.json"));
        let fingerprint = digest(request);
        if let Some(existing) = read_receipt(&file).await? {
            if existing.fingerprint != fingerprint {
                return Err(ReceiptError::KeyConflict);
            }
            if existing.completed {
                return Ok(());
            }
            // A provider may have accepted the message before its receipt was committed.
            return Err(ReceiptError::OutcomeUnknown);
        }
        delivery.prepare().await.map_err(ReceiptError::Delivery)?;
        write_receipt(&file, &fingerprint, agent_id, "pending").await?;
        delivery.send().await.map_err(ReceiptError::Delivery)?;
        write_receipt(&file, &fingerprint, agent_id, "completed").await
    }
}

/// Runs blocking filesystem work off the async executor. A panic in the work
/// resumes here; a task cancelled by runtime shutdown panics with the reason.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => value,
        Err(error) => match error.try_into_panic() {
            Ok(payload) => std::panic::resume_unwind(payload),
            Err(error) => panic!("receipt filesystem task did not finish: {error}"),
        },
    }
}

/// `writeJsonFileAtomic(file, { fingerprint, agentId, state })`.
async fn write_receipt<E>(
    file: &str,
    fingerprint: &str,
    agent_id: &str,
    state: &str,
) -> Result<(), ReceiptError<E>> {
    let mut receipt = JsObject::new();
    receipt.insert("fingerprint", JsValue::String(fingerprint.to_owned()));
    receipt.insert("agentId", JsValue::String(agent_id.to_owned()));
    receipt.insert("state", JsValue::String(state.to_owned()));
    let data = stringify_pretty(&JsValue::Object(receipt));
    let file = file.to_owned();
    blocking(move || node_fs::write_file_atomic(&file, &data))
        .await
        .map_err(ReceiptError::Fs)
}

/// The fields of a receipt the store reads back.
struct Receipt {
    fingerprint: String,
    completed: bool,
}

/// `readReceipt(file)`: `null` when the file is missing, otherwise
/// `ReceiptSchema.parse(JSON.parse(await readFile(file, "utf8")))`.
async fn read_receipt<E>(file: &str) -> Result<Option<Receipt>, ReceiptError<E>> {
    let path = file.to_owned();
    let bytes = match blocking(move || node_fs::read_file(&path)).await {
        Ok(bytes) => bytes,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(ReceiptError::Fs(error)),
    };
    let value = parse(&String::from_utf8_lossy(&bytes)).map_err(ReceiptError::Syntax)?;
    if let Verdict::Invalid(issues) = verdict(&RECEIPT_SCHEMA, &value) {
        return Err(ReceiptError::Schema(stringify_pretty(&JsValue::Array(
            issues,
        ))));
    }
    let text = |key: &str| value.get(key).and_then(JsValue::as_str).unwrap_or_default();
    Ok(Some(Receipt {
        fingerprint: text("fingerprint").to_owned(),
        completed: text("state") == "completed",
    }))
}

/// `ReceiptSchema`, run by the contracts zod engine so a damaged receipt
/// fails with the `ZodError` message the baseline throws.
static RECEIPT_SCHEMA: LazyLock<Schema> = LazyLock::new(|| {
    Schema::Object(
        vec![
            ("fingerprint", Schema::String(Vec::new())),
            ("state", Schema::Enum(&["pending", "completed"])),
            ("agentId", Schema::String(Vec::new())),
        ],
        UnknownKeys::Strip,
    )
});

/// Sorts object keys with `localeCompare` at every depth, as the digest
/// replacer does, then rebuilds each object as `Object.fromEntries` would.
/// Iterative, so deep values never overflow the stack.
fn sorted_keys(value: &JsValue) -> JsValue {
    enum Work<'a> {
        Visit(&'a JsValue),
        Array(usize),
        Object(Vec<&'a str>),
    }
    let mut work = vec![Work::Visit(value)];
    let mut built: Vec<JsValue> = Vec::new();
    while let Some(item) = work.pop() {
        match item {
            Work::Visit(JsValue::Array(items)) => {
                work.push(Work::Array(items.len()));
                work.extend(items.iter().rev().map(Work::Visit));
            }
            Work::Visit(JsValue::Object(object)) => {
                let mut entries: Vec<(&str, &JsValue)> = object.iter().collect();
                // Stable, as `Array.prototype.sort` is.
                entries.sort_by(|(left, _), (right, _)| locale_compare(left, right));
                work.push(Work::Object(entries.iter().map(|(key, _)| *key).collect()));
                work.extend(entries.iter().rev().map(|(_, value)| Work::Visit(value)));
            }
            Work::Visit(leaf) => built.push(leaf.clone()),
            Work::Array(length) => {
                let items = built.split_off(built.len() - length);
                built.push(JsValue::Array(items));
            }
            Work::Object(keys) => {
                let values = built.split_off(built.len() - keys.len());
                let mut object = JsObject::new();
                for (key, value) in keys.into_iter().zip(values) {
                    object.insert(key, value);
                }
                built.push(JsValue::Object(object));
            }
        }
    }
    built.pop().unwrap_or(JsValue::Undefined)
}

/// `digest(value)`: SHA-256 hex of `JSON.stringify` with object keys sorted
/// by `localeCompare`.
///
/// Key order uses [`locale_compare`], an ASCII port of ICU root collation;
/// non-ASCII keys can order differently than node and change the digest.
/// The pinned caller's request is `{ prompt, activeTurnBehavior }`, where
/// `prompt` is a string or blocks from closed zod object schemas, so every
/// key it can carry is ASCII.
#[must_use]
pub fn digest(value: &JsValue) -> String {
    let hash = Sha256::digest(stringify(&sorted_keys(value)).as_bytes());
    hash.iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

#[cfg(test)]
mod tests {
    use super::{RECEIPT_SCHEMA, digest};
    use spocky_contracts::js_value::{JsValue, parse, stringify_pretty};
    use spocky_contracts::zod::{Verdict, verdict};

    /// The issues `ReceiptSchema.parse` throws; empty for a receipt.
    fn receipt_issues(value: &JsValue) -> Vec<JsValue> {
        match verdict(&RECEIPT_SCHEMA, value) {
            Verdict::Invalid(issues) => issues,
            _ => Vec::new(),
        }
    }

    fn value(text: &str) -> JsValue {
        parse(text).expect("valid JSON")
    }

    #[test]
    fn digest_matches_node() {
        // Printed by the pinned dist: digest(["send","agent","m"]) names the
        // receipt file, digest({b:1,a:2}) is its fingerprint.
        assert_eq!(
            digest(&value(r#"["send","agent","m"]"#)),
            "7ca108c96ccc217d42301da57f0fcb7f55b009df941c0d1412f8bd2c7d3fccb6"
        );
        assert_eq!(
            digest(&value(r#"{"b":1,"a":2}"#)),
            "d3626ac30a87e6f7a6428233b3c68299976865fa5508e4267c5415c76af7a772"
        );
        assert_eq!(
            digest(&value(r#"{"b":1,"a":2}"#)),
            digest(&value(r#"{"a":2,"b":1}"#))
        );
    }

    #[test]
    fn schema_issues_match_zod() {
        assert!(
            receipt_issues(&value(
                r#"{"fingerprint":"f","state":"pending","agentId":"a","x":1}"#
            ))
            .is_empty()
        );
        assert_eq!(
            stringify_pretty(&JsValue::Array(receipt_issues(&value("null")))),
            "[\n  {\n    \"expected\": \"object\",\n    \"code\": \"invalid_type\",\n    \"path\": [],\n    \"message\": \"Invalid input: expected object, received null\"\n  }\n]"
        );
        assert_eq!(
            stringify_pretty(&JsValue::Array(receipt_issues(&value(
                r#"{"fingerprint":1,"state":"done"}"#
            )))),
            "[\n  {\n    \"expected\": \"string\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"fingerprint\"\n    ],\n    \"message\": \"Invalid input: expected string, received number\"\n  },\n  {\n    \"code\": \"invalid_value\",\n    \"values\": [\n      \"pending\",\n      \"completed\"\n    ],\n    \"path\": [\n      \"state\"\n    ],\n    \"message\": \"Invalid option: expected one of \\\"pending\\\"|\\\"completed\\\"\"\n  },\n  {\n    \"expected\": \"string\",\n    \"code\": \"invalid_type\",\n    \"path\": [\n      \"agentId\"\n    ],\n    \"message\": \"Invalid input: expected string, received undefined\"\n  }\n]"
        );
    }
}
