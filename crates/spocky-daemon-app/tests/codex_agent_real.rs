//! Drives the Codex adapter through the provider seam against the real
//! pinned `codex` (0.159.0) and the G1 scripted Responses stub: catalog,
//! create in full-access mode, one turn, persistence, close.
//!
//! Runs only with `SPOCKY_REAL_CODEX=1`, as the provider lane's real-Codex
//! tests do; without it the test prints SKIPPED and is never gate evidence.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use spocky_contracts::js_value::{self, JsValue};
use spocky_daemon_app::codex_agent::CodexAgentClient;
use spocky_provider_codex::ProviderRuntimeSettings;
use spocky_session::agent_sdk::{AgentClient, AgentPromptInput, FetchCatalogOptions};
use spocky_slice_harness::{gates, stub};

const PINNED_CODEX_PATH: &str = "/usr/local/Caskroom/codex/0.159.0/bin/codex";
const PINNED_CODEX_SHA256: &str =
    "1ad71e5ed117114f9d04cdd8d5dd411515b5ab7ebc725b8ca2f484695d71c838";
const WAIT: Duration = Duration::from_secs(90);

fn real_codex() -> Option<&'static str> {
    if std::env::var("SPOCKY_REAL_CODEX").as_deref() != Ok("1") {
        eprintln!("SKIPPED real-Codex test: set SPOCKY_REAL_CODEX=1 to run it");
        return None;
    }
    let digest = Command::new("shasum")
        .args(["-a", "256", PINNED_CODEX_PATH])
        .output()
        .expect("shasum of the pinned codex");
    assert_eq!(
        String::from_utf8_lossy(&digest.stdout)
            .split_whitespace()
            .next(),
        Some(PINNED_CODEX_SHA256),
        "pinned codex digest mismatch"
    );
    Some(PINNED_CODEX_PATH)
}

/// A disposable root removed on drop: `home`, `codex-home`, `project`.
struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "spocky-daemon-app-codex-{}-{nanos}",
            std::process::id()
        ));
        for child in ["home", "codex-home", "project"] {
            fs::create_dir_all(path.join(child)).unwrap();
        }
        Self(fs::canonicalize(path).unwrap())
    }

    fn text(&self, child: &str) -> String {
        self.0.join(child).to_string_lossy().into_owned()
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The `codex-home/config.toml` the slice harness writes for both daemons.
fn codex_config(stub_port: u16) -> String {
    format!(
        "model_provider = \"spocky-stub\"\n\n\
         [model_providers.spocky-stub]\n\
         name = \"spocky-stub\"\n\
         base_url = \"http://127.0.0.1:{stub_port}/v1\"\n\
         env_key = \"OPENAI_API_KEY\"\n\
         wire_api = \"responses\"\n\
         supports_websockets = false\n\
         request_max_retries = 0\n\
         stream_max_retries = 0\n\n\
         [analytics]\n\
         enabled = false\n\n\
         [features]\n\
         plugins = false\n"
    )
}

#[derive(Default)]
struct Events {
    seen: Mutex<Vec<JsValue>>,
    changed: Condvar,
}

impl Events {
    fn wait_for(&self, kind: &str) -> JsValue {
        let seen = self.seen.lock().unwrap();
        let (seen, timeout) = self
            .changed
            .wait_timeout_while(seen, WAIT, |seen| {
                !seen
                    .iter()
                    .any(|event| event.get("type").and_then(JsValue::as_str) == Some(kind))
            })
            .unwrap();
        assert!(!timeout.timed_out(), "no {kind} within {WAIT:?}");
        seen.iter()
            .find(|event| event.get("type").and_then(JsValue::as_str) == Some(kind))
            .cloned()
            .unwrap()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_access_turn_runs_through_the_seam() {
    let Some(codex) = real_codex() else {
        return;
    };
    let root = Root::new();
    let listener = stub::bind_loopback().expect("stub listener");
    let stub_port = listener.local_addr().unwrap().port();
    assert!(stub_port != 6767 && stub_port != 6768);
    fs::write(
        root.0.join("codex-home/config.toml"),
        codex_config(stub_port),
    )
    .unwrap();
    let record = fs::File::create(root.0.join("stub.log")).unwrap();
    thread::spawn(move || stub::serve(&listener, gates::g1().script, record));

    let env = [
        ("CODEX_HOME".to_owned(), root.text("codex-home")),
        ("OPENAI_API_KEY".to_owned(), "test-key".to_owned()),
    ]
    .into_iter()
    .collect();
    let client = CodexAgentClient::new(
        Some(ProviderRuntimeSettings {
            command: Some(spocky_provider_codex::ProviderCommand::Replace {
                argv: vec![codex.to_owned()],
            }),
            env: Some(env),
        }),
        vec![
            (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            (OsString::from("HOME"), OsString::from(root.text("home"))),
        ],
    );
    assert!(client.is_available(None, None).await.expect("availability"));

    let catalog = client
        .fetch_catalog(FetchCatalogOptions::Global { force: false }, None)
        .await
        .expect("catalog");
    let catalog = serde_json::from_str(&js_value::stringify(&catalog)).unwrap();
    let model = spocky_provider_codex::catalog::default_model_id(&catalog).expect("default model");

    let config = serde_json::json!({
        "provider": "codex",
        "cwd": root.text("project"),
        "modeId": "full-access",
        "model": model,
    });
    let session = client
        .create_session(js_value::parse(&config.to_string()).unwrap(), None, None)
        .await
        .expect("create session");
    let events = Arc::new(Events::default());
    let sink = Arc::clone(&events);
    let unsubscribe = session.subscribe(Arc::new(move |event| {
        sink.seen.lock().unwrap().push(event);
        sink.changed.notify_all();
    }));

    let turn = session
        .start_turn(AgentPromptInput::Text(gates::G1_PROMPT.to_owned()), None)
        .await
        .expect("start turn");
    assert_eq!(turn, "codex-turn-0");
    events.wait_for("turn_completed");
    let assistant: Vec<String> = events
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.get("type").and_then(JsValue::as_str) == Some("timeline"))
        .filter_map(|event| event.get("item"))
        .filter(|item| item.get("type").and_then(JsValue::as_str) == Some("assistant_message"))
        .filter_map(|item| {
            item.get("text")
                .and_then(JsValue::as_str)
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(assistant.last().map(String::as_str), Some(gates::G1_REPLY));

    let thread_id = session.id().expect("thread id");
    let persistence = session.describe_persistence().expect("persistence handle");
    assert_eq!(
        persistence.get("sessionId").and_then(JsValue::as_str),
        Some(thread_id.as_str())
    );
    assert_eq!(
        persistence
            .get("metadata")
            .and_then(|metadata| metadata.get("modeId"))
            .and_then(JsValue::as_str),
        Some("full-access")
    );

    unsubscribe();
    session.close().await.expect("close");
    assert_eq!(session.describe_persistence(), None);
}
