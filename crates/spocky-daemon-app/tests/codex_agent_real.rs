//! Drives the Codex adapter through the provider seam against the real
//! pinned `codex` (0.159.0) and the G1 scripted Responses stub: catalog,
//! create in full-access mode, one turn, persistence, close.
//!
//! Ignored by default: run with `--include-ignored` and
//! `SPOCKY_REAL_CODEX=1` (PORTING.md) on a host with the pinned binary, which
//! is verified by digest and by a sandboxed `--version` probe, and never
//! skipped.
//!
//! Hermetic: every codex launch goes through the slice harness's recording
//! wrapper (which logs each PID) into `sandbox-exec` with the harness's
//! `EGRESS_PROFILE`, so the kernel denies any outbound connection except
//! loopback; after the test the kernel log must hold no denial for any
//! recorded PID. The model endpoint is the loopback stub via `CODEX_HOME`.

use std::ffi::OsString;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use spocky_contracts::js_value::{self, JsValue};
use spocky_daemon_app::codex_agent::CodexAgentClient;
use spocky_provider_codex::{ProviderCommand, ProviderRuntimeSettings};
use spocky_session::agent_sdk::{AgentClient, AgentPromptInput, FetchCatalogOptions};
use spocky_slice_harness::side::{
    EGRESS_PROFILE, SANDBOX_EXEC, codex_wrapper_script, egress_violations, shell_quote,
};
use spocky_slice_harness::{gates, stub};

const PINNED_CODEX_VERSION: &str = "codex-cli 0.159.0";
const PINNED_CODEX_PATH: &str = "/usr/local/Caskroom/codex/0.159.0/bin/codex";
const PINNED_CODEX_SHA256: &str =
    "1ad71e5ed117114f9d04cdd8d5dd411515b5ab7ebc725b8ca2f484695d71c838";
const WAIT: Duration = Duration::from_secs(90);

/// Runs `command` to completion within [`WAIT`], killing the child it
/// spawned (by its own handle) on overrun; returns its stdout.
fn bounded_stdout(mut command: Command, what: &str) -> String {
    use std::io::Read;
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {what}: {error}"));
    let deadline = Instant::now() + WAIT;
    while child.try_wait().expect("wait").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{what} exceeded {WAIT:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    stdout
}

/// The pinned binary, verified by digest; panics instead of skipping. The
/// env gate `SPOCKY_REAL_CODEX=1` must accompany `--include-ignored`, so an
/// ignored-test sweep without it fails rather than launching codex.
fn pinned_codex() -> &'static str {
    assert_eq!(
        std::env::var("SPOCKY_REAL_CODEX").as_deref(),
        Ok("1"),
        "real-codex test run with --include-ignored but without SPOCKY_REAL_CODEX=1"
    );
    let mut shasum = Command::new("/usr/bin/shasum");
    shasum.args(["-a", "256", PINNED_CODEX_PATH]);
    assert_eq!(
        bounded_stdout(shasum, "shasum").split_whitespace().next(),
        Some(PINNED_CODEX_SHA256),
        "pinned codex digest mismatch at {PINNED_CODEX_PATH}"
    );
    PINNED_CODEX_PATH
}

/// `codex --version` through the hermetic launcher, so the probe is
/// sandboxed and its PID is recorded for the egress check, with a cleared
/// env and the root's own `HOME` and `CODEX_HOME`.
fn assert_pinned_version(root: &Root, launcher: &str) {
    let mut probe = Command::new(launcher);
    probe
        .arg("--version")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.text("home"))
        .env("TMPDIR", root.text("home"))
        .env("CODEX_HOME", root.text("codex-home"));
    assert_eq!(
        bounded_stdout(probe, "codex --version").trim(),
        PINNED_CODEX_VERSION
    );
}

/// Awaits `future` for at most [`WAIT`].
async fn bounded<T>(what: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, future)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish within {WAIT:?}"))
}

/// A disposable root removed on drop: `home`, `codex-home`, `codex-io`,
/// `project`, `bin`.
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
        for child in ["home", "codex-home", "codex-io", "project", "bin"] {
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

fn write_executable(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// `bin/codex`: the harness recording wrapper, exec'ing `sandbox-exec` with
/// the harness egress profile around the pinned binary. Each `exec` keeps
/// the PID, so the recorded PID is the sandboxed codex process.
fn hermetic_codex(root: &Root, codex: &str) -> String {
    let sandboxed = root.0.join("bin/codex-sandboxed");
    write_executable(
        &sandboxed,
        &format!(
            "#!/bin/sh\nexec {} -p {} {} \"$@\"\n",
            shell_quote(SANDBOX_EXEC),
            shell_quote(EGRESS_PROFILE),
            shell_quote(codex)
        ),
    );
    let wrapper = root.0.join("bin/codex");
    write_executable(
        &wrapper,
        &codex_wrapper_script(&root.text("codex-io"), &sandboxed.to_string_lossy()),
    );
    wrapper.to_string_lossy().into_owned()
}

/// The `codex-home/config.toml` the slice harness writes for both daemons,
/// plus the update check off, so codex has no reason to leave loopback.
fn codex_config(stub_port: u16) -> String {
    format!(
        "model_provider = \"spocky-stub\"\n\
         check_for_update_on_startup = false\n\n\
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
    fn has(seen: &[JsValue], kind: &str) -> bool {
        seen.iter()
            .any(|event| event.get("type").and_then(JsValue::as_str) == Some(kind))
    }

    fn wait_for(&self, kind: &str) {
        let seen = self.seen.lock().unwrap();
        let (_seen, timeout) = self
            .changed
            .wait_timeout_while(seen, WAIT, |seen| !Self::has(seen, kind))
            .unwrap();
        assert!(!timeout.timed_out(), "no {kind} within {WAIT:?}");
    }
}

/// Asserts, when dropped, that the kernel denied no outbound connection for
/// any recorded codex PID since the test started, so the check also runs
/// when an earlier assertion fails.
struct EgressCheck {
    io: PathBuf,
    started: Instant,
}

impl Drop for EgressCheck {
    fn drop(&mut self) {
        let pids: Vec<u32> = fs::read_dir(&self.io)
            .map(|entries| {
                entries
                    .filter_map(|entry| fs::read_to_string(entry.ok()?.path().join("pid")).ok())
                    .filter_map(|pid| pid.trim().parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        let violations = egress_violations(self.started.elapsed() + Duration::from_secs(5), &pids);
        if thread::panicking() {
            if !violations.is_empty() {
                eprintln!("non-loopback egress attempted: {violations:?}");
            }
            return;
        }
        assert!(!pids.is_empty(), "no codex launch was recorded");
        assert!(
            violations.is_empty(),
            "non-loopback egress attempted: {violations:?}"
        );
    }
}

/// Starts the G1 stub, writes `codex-home/config.toml`, and builds the
/// client with the hermetic launcher and a disposable home.
fn hermetic_client(root: &Root, codex: &str) -> CodexAgentClient {
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
    let launcher = hermetic_codex(root, codex);
    assert_pinned_version(root, &launcher);
    CodexAgentClient::new(
        Some(ProviderRuntimeSettings {
            command: Some(ProviderCommand::Replace {
                argv: vec![launcher],
            }),
            env: Some(env),
        }),
        vec![
            (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            (OsString::from("HOME"), OsString::from(root.text("home"))),
            (OsString::from("TMPDIR"), OsString::from(root.text("home"))),
        ],
    )
}

fn assistant_texts(events: &Events) -> Vec<String> {
    events
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
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "drives the pinned codex binary; run with --include-ignored"]
async fn full_access_turn_runs_through_the_seam() {
    let codex = pinned_codex();
    let root = Root::new();
    let _egress = EgressCheck {
        io: root.0.join("codex-io"),
        started: Instant::now(),
    };
    let client = hermetic_client(&root, codex);
    assert!(
        bounded("isAvailable", client.is_available(None, None))
            .await
            .expect("availability")
    );

    let catalog = bounded(
        "fetchCatalog",
        client.fetch_catalog(FetchCatalogOptions::Global { force: false }, None),
    )
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
    let session = bounded(
        "createSession",
        client.create_session(js_value::parse(&config.to_string()).unwrap(), None, None),
    )
    .await
    .expect("create session");
    let events = Arc::new(Events::default());
    let sink = Arc::clone(&events);
    let unsubscribe = session.subscribe(Arc::new(move |event| {
        sink.seen.lock().unwrap().push(event);
        sink.changed.notify_all();
    }));

    let turn = bounded(
        "startTurn",
        session.start_turn(AgentPromptInput::Text(gates::G1_PROMPT.to_owned()), None),
    )
    .await
    .expect("start turn");
    assert_eq!(turn, "codex-turn-0");
    let waiter = Arc::clone(&events);
    bounded(
        "turn_completed",
        tokio::task::spawn_blocking(move || waiter.wait_for("turn_completed")),
    )
    .await
    .expect("event wait");
    assert_eq!(
        assistant_texts(&events).last().map(String::as_str),
        Some(gates::G1_REPLY)
    );

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
    bounded("close", session.close()).await.expect("close");
    assert_eq!(session.describe_persistence(), None);
}
