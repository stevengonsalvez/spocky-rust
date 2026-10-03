//! The pinned `message-receipts/index.test.ts` cases, ported one to one.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_message_receipts::{Delivery, MessageReceipts, ReceiptError};

struct Disposable(PathBuf);

impl Drop for Disposable {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

static FIXTURES: AtomicUsize = AtomicUsize::new(0);

/// `mkdtemp(path.join(tmpdir(), "agent-requests-"))`: unique per process,
/// test, and call, since tests run in parallel.
fn fixture() -> (Disposable, String) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "agent-requests-{}-{nonce}-{}",
        std::process::id(),
        FIXTURES.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&directory).expect("create disposable directory");
    let text = directory.to_string_lossy().into_owned();
    (Disposable(directory), text)
}

#[derive(Debug, PartialEq, Eq)]
struct Thrown(&'static str);

impl std::fmt::Display for Thrown {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

/// A scripted provider: counts sends, optionally fails `send`, and fails
/// `prepare` while `available` is false.
#[derive(Clone)]
struct Scripted {
    sends: Arc<AtomicUsize>,
    send_error: Option<&'static str>,
    available: Option<Arc<AtomicBool>>,
}

impl Scripted {
    fn new(sends: &Arc<AtomicUsize>) -> Self {
        Self {
            sends: Arc::clone(sends),
            send_error: None,
            available: None,
        }
    }
}

impl Delivery for Scripted {
    type Error = Thrown;

    async fn prepare(&mut self) -> Result<(), Thrown> {
        match &self.available {
            Some(available) if !available.load(Ordering::SeqCst) => Err(Thrown("load failed")),
            _ => Ok(()),
        }
    }

    async fn send(&mut self) -> Result<(), Thrown> {
        self.sends.fetch_add(1, Ordering::SeqCst);
        self.send_error
            .map_or(Ok(()), |message| Err(Thrown(message)))
    }
}

fn request(entries: &[(&str, &str)]) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(*key, JsValue::String((*value).to_owned()));
    }
    JsValue::Object(object)
}

#[tokio::test]
async fn message_retries_survive_reconstruction_without_submitting_twice() {
    let (_guard, directory) = fixture();
    let requests = MessageReceipts::new(directory.clone());
    let deliveries = Arc::new(AtomicUsize::new(0));
    let input = Scripted::new(&deliveries);
    let body = request(&[("text", "hello")]);
    let (first, second) = tokio::join!(
        requests.send("agent", "arrival", &body, input.clone()),
        requests.send("agent", "arrival", &body, input.clone()),
    );
    first.expect("first send");
    second.expect("duplicate send");
    MessageReceipts::new(directory)
        .send("agent", "arrival", &body, input.clone())
        .await
        .expect("send after reconstruction");
    assert_eq!(deliveries.load(Ordering::SeqCst), 1);
    requests
        .send("another", "arrival", &body, input)
        .await
        .expect("another agent");
    assert_eq!(deliveries.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn ambiguous_provider_delivery_is_never_blindly_replayed_after_restart() {
    let (_guard, directory) = fixture();
    let requests = MessageReceipts::new(directory.clone());
    let deliveries = Arc::new(AtomicUsize::new(0));
    let input = Scripted {
        send_error: Some("connection lost"),
        ..Scripted::new(&deliveries)
    };
    let body = request(&[]);
    let first = requests
        .send("agent", "arrival", &body, input.clone())
        .await;
    assert!(matches!(
        first,
        Err(ReceiptError::Delivery(Thrown("connection lost")))
    ));
    let retry = MessageReceipts::new(directory)
        .send("agent", "arrival", &body, input)
        .await
        .expect_err("pending receipt rejects");
    assert_eq!(retry.to_string(), "agent_request_outcome_unknown");
    assert_eq!(deliveries.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_local_message_preparation_does_not_leave_an_ambiguous_receipt() {
    let (_guard, directory) = fixture();
    let requests = MessageReceipts::new(directory.clone());
    let sends = Arc::new(AtomicUsize::new(0));
    let available = Arc::new(AtomicBool::new(false));
    let input = Scripted {
        available: Some(Arc::clone(&available)),
        ..Scripted::new(&sends)
    };
    let body = request(&[]);
    let first = requests
        .send("agent", "message", &body, input.clone())
        .await;
    assert_eq!(first.expect_err("prepare fails").to_string(), "load failed");
    available.store(true, Ordering::SeqCst);
    MessageReceipts::new(directory)
        .send("agent", "message", &body, input.clone())
        .await
        .expect("prepared send");
    available.store(false, Ordering::SeqCst);
    requests
        .send("agent", "message", &body, input)
        .await
        .expect("completed receipt suppresses the retry");
    assert_eq!(sends.load(Ordering::SeqCst), 1);
}

/// The baseline queues a send when `send` is called, not when it is awaited:
/// polling the later call first must not let it overtake the earlier one.
#[tokio::test]
async fn sends_run_in_call_order_when_polled_out_of_order() {
    let (_guard, directory) = fixture();
    let requests = MessageReceipts::new(directory);
    let sends = Arc::new(AtomicUsize::new(0));
    let body = request(&[]);
    let failing = Scripted {
        send_error: Some("connection lost"),
        ..Scripted::new(&sends)
    };
    let first = requests.send("agent", "message", &body, failing);
    let second = requests.send("agent", "message", &body, Scripted::new(&sends));
    // `tokio::join!` polls its arguments in order: the later call first.
    let (second, first) = tokio::join!(second, first);
    assert!(matches!(
        first,
        Err(ReceiptError::Delivery(Thrown("connection lost")))
    ));
    assert_eq!(
        second
            .expect_err("queued behind the failed send")
            .to_string(),
        "agent_request_outcome_unknown"
    );
    assert_eq!(sends.load(Ordering::SeqCst), 1);
}

/// A baseline promise runs when it is created, so awaiting the later of two
/// sends first must not wait on the earlier one's poll. A lazy future
/// deadlocks here; the timeout turns that into a failure.
#[tokio::test]
async fn awaiting_a_later_send_first_does_not_deadlock() {
    let (_guard, directory) = fixture();
    let requests = MessageReceipts::new(directory);
    let sends = Arc::new(AtomicUsize::new(0));
    let body = request(&[]);
    let first = requests.send("agent", "message", &body, Scripted::new(&sends));
    let second = requests.send("agent", "message", &body, Scripted::new(&sends));
    let bound = std::time::Duration::from_secs(30);
    tokio::time::timeout(bound, second)
        .await
        .expect("the later send settles without the earlier one being polled")
        .expect("the later send finds the completed receipt");
    tokio::time::timeout(bound, first)
        .await
        .expect("the earlier send settles")
        .expect("the earlier send delivers");
    assert_eq!(sends.load(Ordering::SeqCst), 1);
}

/// A promise cannot be cancelled by forgetting it: dropping the future of a
/// send does not stop the send.
#[tokio::test]
async fn dropping_a_send_does_not_cancel_it() {
    let (_guard, directory) = fixture();
    let requests = MessageReceipts::new(directory.clone());
    let sends = Arc::new(AtomicUsize::new(0));
    let body = request(&[]);
    drop(requests.send("agent", "message", &body, Scripted::new(&sends)));
    for _ in 0..600 {
        if sends.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(sends.load(Ordering::SeqCst), 1, "the dropped send ran");
    // The completed write follows the send; a retry sees the receipt.
    let retry = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        requests.send("agent", "message", &body, Scripted::new(&sends)),
    )
    .await
    .expect("the retry settles");
    retry.expect("the dropped send left a completed receipt");
    assert_eq!(sends.load(Ordering::SeqCst), 1);
}

/// Known pinned defect, reproduced on purpose: a failed `completed` write
/// after a successful send leaves the receipt `pending`, so the delivered
/// message can never be confirmed and every retry is an unknown outcome.
#[cfg(unix)]
#[tokio::test]
async fn completed_write_failure_leaves_a_delivered_message_unknown() {
    use std::os::unix::fs::PermissionsExt;

    struct LockAfterSend {
        directory: String,
        sends: Arc<AtomicUsize>,
    }

    impl Delivery for LockAfterSend {
        type Error = Thrown;

        async fn send(&mut self) -> Result<(), Thrown> {
            self.sends.fetch_add(1, Ordering::SeqCst);
            fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o555))
                .expect("lock receipts directory");
            Ok(())
        }
    }

    let (_guard, directory) = fixture();
    let sends = Arc::new(AtomicUsize::new(0));
    let body = request(&[]);
    let failed = MessageReceipts::new(directory.clone())
        .send(
            "agent",
            "message",
            &body,
            LockAfterSend {
                directory: directory.clone(),
                sends: Arc::clone(&sends),
            },
        )
        .await
        .expect_err("completed write fails");
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755))
        .expect("unlock receipts directory");
    assert!(
        failed
            .to_string()
            .starts_with("EACCES: permission denied, open '"),
        "{failed}"
    );
    let receipt = fs::read_dir(&directory)
        .expect("list receipts")
        .map(|entry| entry.expect("entry").path())
        .collect::<Vec<_>>();
    assert_eq!(receipt.len(), 1, "temp file removed, receipt kept");
    let text = fs::read_to_string(&receipt[0]).expect("read receipt");
    assert!(text.ends_with("\"state\": \"pending\"\n}"), "{text}");
    let retry = MessageReceipts::new(directory)
        .send("agent", "message", &body, Scripted::new(&sends))
        .await
        .expect_err("delivered message stays unknown");
    assert_eq!(retry.to_string(), "agent_request_outcome_unknown");
    assert_eq!(sends.load(Ordering::SeqCst), 1);
}
