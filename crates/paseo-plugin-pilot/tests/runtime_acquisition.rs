use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use paseo_plugin_pilot::{
    Contribution, HookKind, PluginHost, PluginProcessMessage, PluginProcessRequest,
    PluginSourceIdentity, ProcessHooks, ProviderCatalogOptions, ProviderConnectRequest,
    RuntimeProtocolStep, acquire_git, acquire_npm_tarball,
};
use serde_json::json;

struct TestDir(PathBuf);

static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "paseo-plugin-runtime-{}-{nonce}-{}",
            std::process::id(),
            TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        match fs::remove_dir_all(&self.0) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("remove test directory: {error}"),
        }
    }
}

fn run(command: &mut Command) -> String {
    let output = command.output().expect("run fixture command");
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf8 stdout")
        .trim()
        .to_owned()
}

fn write_runtime_plugin(root: &Path, id: &str, contribution: &str) {
    fs::write(
        root.join("paseo-plugin.json"),
        format!(r#"{{"id":"{id}","requirements":{{"paseo":">=0.8.0"}}}}"#),
    )
    .expect("write manifest");
    fs::write(
        root.join("index.client.ts"),
        format!("export const contribution = {contribution:?};\n"),
    )
    .expect("write client bundle");
    fs::write(
        root.join("index.server.ts"),
        format!(
            r#"const readline = require("node:readline");
const lines = readline.createInterface({{ input: process.stdin }});
lines.on("line", (line) => {{
  const message = JSON.parse(line);
  if (message.type === "initialize") {{
    if (!message.pluginId || !message.bundle || !message.appVersion || !message.pluginDirectory) process.exit(9);
    console.log(JSON.stringify({{ type: "ready", methods: ["{contribution}"], providers: [], usageSources: [], hooks: {{ events: [], before: [] }} }}));
  }}
  if (message.type === "invoke") console.log(JSON.stringify({{ type: "result", requestId: message.requestId, output: {{ echoed: message.input }} }}));
  if (message.type === "shutdown") process.exit(0);
}});
"#
        ),
    )
    .expect("write runtime");
}

#[test]
fn git_acquisition_runs_contribution_process_at_reviewed_revision() {
    let root = TestDir::new();
    let repository = root.path().join("repository");
    let plugin = repository.join("plugins/review");
    fs::create_dir_all(&plugin).expect("create plugin");
    write_runtime_plugin(&plugin, "git-review", "review.start");
    run(Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(&repository));
    run(Command::new("git")
        .args(["config", "user.name", "Paseo Tests"])
        .current_dir(&repository));
    run(Command::new("git")
        .args(["config", "user.email", "tests@paseo.invalid"])
        .current_dir(&repository));
    run(Command::new("git")
        .args(["add", "."])
        .current_dir(&repository));
    run(Command::new("git")
        .args(["commit", "-m", "fixture"])
        .current_dir(&repository));
    let revision = run(Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&repository));

    let acquired = acquire_git(
        repository.to_str().expect("utf8 repository"),
        "plugins/review",
        &revision,
        root.path().join("checkout"),
        Duration::from_secs(10),
    )
    .expect("acquire reviewed Git revision");
    assert_eq!(
        acquired.identity(),
        &PluginSourceIdentity::Git {
            remote: repository.to_string_lossy().into_owned(),
            plugin_path: "plugins/review".into(),
        }
    );
    assert_eq!(acquired.revision(), revision);

    let loaded = acquired
        .load_and_invoke("review.start", json!({"change": 7}), Duration::from_secs(5))
        .expect("run acquired plugin process");
    assert_eq!(loaded.id().as_str(), "git-review");
    assert_eq!(
        loaded.contributions(),
        &[Contribution::Rpc("review.start".into())]
    );
    assert_eq!(
        loaded.invocation_output(),
        Some(&json!({"echoed":{"change":7}}))
    );
    assert_eq!(loaded.traffic().len(), 5);
    assert_eq!(loaded.traffic()[0].direction(), "host_to_plugin");
    assert_eq!(loaded.traffic()[1].direction(), "plugin_to_host");
    println!(
        "PLUGIN_RUNTIME_EVIDENCE {}",
        json!({
            "case": "git_acquisition",
            "revision": revision,
            "contributions": ["rpc:review.start"],
            "traffic": loaded.traffic().iter().map(|frame| json!({
                "direction": frame.direction(),
                "message": frame.message(),
            })).collect::<Vec<_>>(),
        })
    );
}

#[test]
fn git_acquisition_normalizes_host_separators_and_cleans_failed_staging() {
    let root = TestDir::new();
    let repository = root.path().join("repository");
    let plugin = repository.join("plugins/review");
    fs::create_dir_all(&plugin).expect("create plugin");
    write_runtime_plugin(&plugin, "path-review", "review.path");
    run(Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(&repository));
    run(Command::new("git")
        .args(["config", "user.name", "Paseo Tests"])
        .current_dir(&repository));
    run(Command::new("git")
        .args(["config", "user.email", "tests@paseo.invalid"])
        .current_dir(&repository));
    run(Command::new("git")
        .args(["add", "."])
        .current_dir(&repository));
    run(Command::new("git")
        .args(["commit", "-m", "fixture"])
        .current_dir(&repository));
    let revision = run(Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&repository));

    let recovered_checkout = root.path().join("windows-separator-checkout");
    let stale_checkout = root.path().join(".windows-separator-checkout.staging");
    fs::create_dir_all(&stale_checkout).expect("create interrupted Git staging");
    fs::write(stale_checkout.join("partial"), "interrupted").expect("write staging marker");
    acquire_git(
        repository.to_str().expect("repository path"),
        "plugins\\review",
        &revision,
        &recovered_checkout,
        Duration::from_secs(10),
    )
    .expect("recover staging and normalize Windows separator");
    assert!(recovered_checkout.is_dir());
    assert!(!stale_checkout.exists());

    let failed_checkout = root.path().join("failed-checkout");
    assert!(
        acquire_git(
            repository.to_str().expect("repository path"),
            "missing",
            &revision,
            &failed_checkout,
            Duration::from_secs(10),
        )
        .is_err()
    );
    assert!(!failed_checkout.exists(), "failed staging must be removed");
}

#[test]
fn npm_tarball_acquisition_runs_contribution_process_at_package_version() {
    let root = TestDir::new();
    let package = root.path().join("package");
    fs::create_dir_all(&package).expect("create package");
    fs::write(
        package.join("package.json"),
        r#"{"name":"@acme/review","version":"1.2.3","files":["paseo-plugin.json","index.client.ts","index.server.ts"]}"#,
    )
    .expect("write package manifest");
    write_runtime_plugin(&package, "npm-review", "review.settings");
    let archive_name = run(Command::new("npm")
        .args(["pack", "--silent"])
        .current_dir(&package));
    let archive = package.join(archive_name.lines().last().expect("archive name"));

    let installation = root.path().join("installation");
    let stale_installation = root.path().join(".installation.staging");
    fs::create_dir_all(&stale_installation).expect("create interrupted npm staging");
    fs::write(stale_installation.join("partial"), "interrupted").expect("write staging marker");
    let acquired = acquire_npm_tarball(
        &archive,
        "@acme/review",
        ".",
        &installation,
        Duration::from_secs(15),
    )
    .expect("recover staging and acquire npm tarball");
    assert!(installation.is_dir());
    assert!(!stale_installation.exists());
    assert_eq!(
        acquired.identity(),
        &PluginSourceIdentity::Npm {
            package_name: "@acme/review".into(),
            plugin_path: ".".into(),
        }
    );
    assert_eq!(acquired.revision(), "1.2.3");

    let loaded = acquired
        .load(Duration::from_secs(5))
        .expect("run npm plugin process");
    assert_eq!(loaded.id().as_str(), "npm-review");
    assert_eq!(
        loaded.contributions(),
        &[Contribution::Rpc("review.settings".into())]
    );
    assert_eq!(loaded.traffic().len(), 3);
    let failed_installation = root.path().join("failed-installation");
    assert!(
        acquire_npm_tarball(
            &archive,
            "@acme/missing",
            ".",
            &failed_installation,
            Duration::from_secs(15),
        )
        .is_err()
    );
    assert!(
        !failed_installation.exists(),
        "failed npm staging must be removed"
    );
    println!(
        "PLUGIN_RUNTIME_EVIDENCE {}",
        json!({
            "case": "npm_acquisition",
            "package": "@acme/review",
            "version": "1.2.3",
            "install": { "offline": true, "ignore_scripts": true },
            "contributions": ["rpc:review.settings"],
            "traffic": loaded.traffic().iter().map(|frame| json!({
                "direction": frame.direction(),
                "message": frame.message(),
            })).collect::<Vec<_>>(),
        })
    );
}

#[test]
fn runtime_rejects_nonzero_exit_after_valid_shutdown_handshake() {
    let root = TestDir::new();
    let package = root.path().join("package");
    fs::create_dir_all(&package).expect("create package");
    fs::write(
        package.join("package.json"),
        r#"{"name":"@acme/nonzero","version":"1.0.0","files":["paseo-plugin.json","index.server.ts"]}"#,
    )
    .expect("write package manifest");
    fs::write(package.join("paseo-plugin.json"), r#"{"id":"nonzero"}"#)
        .expect("write plugin manifest");
    fs::write(
        package.join("index.server.ts"),
        r#"const readline = require("node:readline");
const lines = readline.createInterface({ input: process.stdin });
lines.on("line", (line) => {
  const message = JSON.parse(line);
  if (message.type === "initialize") console.log(JSON.stringify({ type: "ready", methods: [], providers: [] }));
  if (message.type === "shutdown") process.exit(7);
});
"#,
    )
    .expect("write runtime");
    let archive_name = run(Command::new("npm")
        .args(["pack", "--silent"])
        .current_dir(&package));
    let archive = package.join(archive_name.lines().last().expect("archive name"));
    let acquired = acquire_npm_tarball(
        &archive,
        "@acme/nonzero",
        ".",
        root.path().join("installation"),
        Duration::from_secs(15),
    )
    .expect("acquire npm tarball");

    assert!(acquired.load(Duration::from_secs(5)).is_err());
}

#[test]
fn runtime_exchanges_messages_through_real_node_fork_ipc() {
    let root = TestDir::new();
    let package = root.path().join("package");
    fs::create_dir_all(&package).expect("create package");
    fs::write(
        package.join("package.json"),
        r#"{"name":"@acme/ipc","version":"1.0.0","files":["paseo-plugin.json","index.server.ts"]}"#,
    )
    .expect("write package manifest");
    fs::write(package.join("paseo-plugin.json"), r#"{"id":"ipc"}"#).expect("write plugin manifest");
    fs::write(
        package.join("index.server.ts"),
        r#"process.on("message", (message) => {
  if (message.type === "initialize") process.send({ type: "ready", methods: ["ipc.echo"], providers: [], usageSources: [], hooks: { events: [], before: [] } });
  if (message.type === "invoke") process.send({ type: "result", requestId: message.requestId, output: { echoed: message.input, ipc: typeof process.send === "function" } });
  if (message.type === "shutdown") process.exit(0);
});
"#,
    )
    .expect("write IPC runtime");
    let archive_name = run(Command::new("npm")
        .args(["pack", "--silent"])
        .current_dir(&package));
    let archive = package.join(archive_name.lines().last().expect("archive name"));
    let acquired = acquire_npm_tarball(
        &archive,
        "@acme/ipc",
        ".",
        root.path().join("installation"),
        Duration::from_secs(15),
    )
    .expect("acquire IPC plugin");

    let loaded = acquired
        .load_via_node_fork_and_invoke("ipc.echo", json!({"value": 7}), Duration::from_secs(5))
        .expect("load through Node fork IPC");
    assert_eq!(
        loaded.invocation_output(),
        Some(&json!({"echoed":{"value":7},"ipc":true}))
    );
    assert_eq!(
        loaded.contributions(),
        &[Contribution::Rpc("ipc.echo".into())]
    );
    assert_eq!(loaded.traffic().len(), 5);
}

#[cfg(unix)]
#[test]
fn timed_out_node_fork_ipc_reaps_the_plugin_child() {
    let root = TestDir::new();
    let package = root.path().join("package");
    fs::create_dir_all(&package).expect("create package");
    fs::write(
        package.join("package.json"),
        r#"{"name":"@acme/ipc-hang","version":"1.0.0","files":["paseo-plugin.json","index.server.ts"]}"#,
    )
    .expect("write package manifest");
    fs::write(package.join("paseo-plugin.json"), r#"{"id":"ipc-hang"}"#)
        .expect("write plugin manifest");
    fs::write(
        package.join("index.server.ts"),
        r#"const fs = require("node:fs");
const path = require("node:path");
process.on("message", (message) => {
  if (message.type === "initialize") {
    fs.writeFileSync(path.join(message.pluginDirectory, "ipc-child.pid"), String(process.pid));
    process.send({ type: "ready", methods: ["ipc.hang"], providers: [], usageSources: [], hooks: { events: [], before: [] } });
  }
});
"#,
    )
    .expect("write hanging IPC runtime");
    let archive_name = run(Command::new("npm")
        .args(["pack", "--silent"])
        .current_dir(&package));
    let archive = package.join(archive_name.lines().last().expect("archive name"));
    let installation = root.path().join("installation");
    let acquired = acquire_npm_tarball(
        &archive,
        "@acme/ipc-hang",
        ".",
        &installation,
        Duration::from_secs(15),
    )
    .expect("acquire hanging IPC plugin");

    assert!(matches!(
        acquired.load_via_node_fork_and_invoke("ipc.hang", json!({}), Duration::from_secs(3),),
        Err(paseo_plugin_pilot::PluginError::RuntimeTimedOut)
    ));
    let pid = fs::read_to_string(
        installation
            .join("node_modules")
            .join("@acme")
            .join("ipc-hang")
            .join("ipc-child.pid"),
    )
    .expect("child pid")
    .trim()
    .to_owned();
    assert!(
        !Command::new("kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()
            .expect("probe child")
            .success(),
        "timed-out IPC child must be reaped"
    );
}

#[test]
fn runtime_exposes_all_ready_contributions_and_recovers_after_fatal_message() {
    let root = TestDir::new();
    let package = root.path().join("package");
    fs::create_dir_all(&package).expect("create package");
    fs::write(
        package.join("package.json"),
        r#"{"name":"@acme/full","version":"1.0.0","files":["paseo-plugin.json","index.server.ts"]}"#,
    )
    .expect("write package manifest");
    fs::write(package.join("paseo-plugin.json"), r#"{"id":"full"}"#)
        .expect("write plugin manifest");
    fs::write(
        package.join("index.server.ts"),
        r#"const readline = require("node:readline");
const lines = readline.createInterface({ input: process.stdin });
lines.on("line", (line) => {
  const message = JSON.parse(line);
  if (message.type === "initialize") console.log(JSON.stringify({
    type: "ready",
    methods: ["review.start"],
    providers: [{ id: "codex", label: "Codex" }],
    usageSources: [{ id: "credits", label: "Credits", discover: true }],
    hooks: { events: ["session.created"], before: ["session.prompt"] }
  }));
  if (message.type === "invoke") console.log(JSON.stringify({ type: "fatal", error: "boom" }));
  if (message.type === "shutdown") process.exit(0);
});
"#,
    )
    .expect("write runtime");
    let archive_name = run(Command::new("npm")
        .args(["pack", "--silent"])
        .current_dir(&package));
    let archive = package.join(archive_name.lines().last().expect("archive name"));

    let load = |directory: &str| {
        acquire_npm_tarball(
            &archive,
            "@acme/full",
            ".",
            root.path().join(directory),
            Duration::from_secs(15),
        )
        .expect("acquire full plugin")
    };
    let loaded = load("first")
        .load(Duration::from_secs(5))
        .expect("load complete metadata");
    assert_eq!(
        loaded.contributions(),
        &[
            Contribution::Rpc("review.start".into()),
            Contribution::Provider("codex".into()),
            Contribution::UsageSource("credits".into()),
            Contribution::HookEvent("session.created".into()),
            Contribution::HookBefore("session.prompt".into()),
        ]
    );

    assert!(matches!(
        load("fatal").load_and_invoke(
            "review.start",
            json!({"change": 7}),
            Duration::from_secs(5),
        ),
        Err(paseo_plugin_pilot::PluginError::RuntimeFatal(
            error
        )) if error == "boom"
    ));
    assert_eq!(
        load("restart")
            .load(Duration::from_secs(5))
            .expect("restart after fatal")
            .contributions(),
        loaded.contributions()
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn runtime_executes_hooks_usage_provider_reconnect_and_session_frames() {
    let root = TestDir::new();
    let package = root.path().join("package");
    fs::create_dir_all(&package).expect("create package");
    fs::write(
        package.join("package.json"),
        r#"{"name":"@acme/protocol","version":"1.0.0","files":["paseo-plugin.json","index.server.ts"]}"#,
    )
    .expect("write package manifest");
    fs::write(package.join("paseo-plugin.json"), r#"{"id":"protocol"}"#)
        .expect("write plugin manifest");
    fs::write(
        package.join("index.server.ts"),
        r#"const readline = require("node:readline");
const lines = readline.createInterface({ input: process.stdin });
lines.on("line", (line) => {
  const message = JSON.parse(line);
  const send = (value) => console.log(JSON.stringify(value));
  if (message.type === "initialize") {
    send({ type: "ready", methods: [], providers: [{ id: "codex", label: "Codex" }], usageSources: [{ id: "credits", label: "Credits", discover: true }], hooks: { events: ["session.created"], before: ["session.prompt"] } });
    send({ type: "settings.changed", settingsId: "display" });
    send({ type: "hooks.changed", hooks: { events: ["session.updated"], before: [] } });
  }
  if (message.type === "provider.catalog_key") send({ type: "result", requestId: message.requestId, output: "catalog-v1" });
  if (message.type === "hook.cancel") send({ type: "error", requestId: message.requestId, error: "cancelled" });
  if (message.type.startsWith("usage.")) send({ type: "result", requestId: message.requestId, output: message.type });
  if (message.type === "provider.connect") send({ type: "provider.connected", connectionId: message.connectionId, version: 2, capabilities: ["sessions"] });
  if (message.type === "provider.send") {
    send({ type: "provider.accepted", connectionId: message.connectionId, acceptanceId: message.acceptanceId });
    send({ type: "provider.event", connectionId: message.connectionId, event: { type: "sessions", sessions: [] } });
  }
  if (message.type === "provider.close") send({ type: "provider.closed", connectionId: message.connectionId });
  if (message.type === "paseo_frame") send(message);
  if (message.type === "paseo_close") send({ type: "paseo_close" });
  if (message.type === "shutdown") process.exit(0);
});
"#,
    )
    .expect("write runtime");
    let archive_name = run(Command::new("npm")
        .args(["pack", "--silent"])
        .current_dir(&package));
    let archive = package.join(archive_name.lines().last().expect("archive name"));
    let acquired = acquire_npm_tarball(
        &archive,
        "@acme/protocol",
        ".",
        root.path().join("installation"),
        Duration::from_secs(15),
    )
    .expect("acquire protocol plugin");

    let result = |request_id: &str, output: serde_json::Value| {
        RuntimeProtocolStep::Receive(PluginProcessMessage::Result {
            request_id: request_id.into(),
            output,
        })
    };
    let connection = || PluginProcessRequest::ProviderConnect {
        provider_id: "codex".into(),
        connection_id: "connection-1".into(),
        request: ProviderConnectRequest {
            versions: vec![1, 2],
            capabilities: vec!["sessions".into()],
        },
    };
    let steps = vec![
        RuntimeProtocolStep::Receive(PluginProcessMessage::SettingsChanged {
            settings_id: "display".into(),
        }),
        RuntimeProtocolStep::Receive(PluginProcessMessage::HooksChanged {
            hooks: ProcessHooks {
                events: vec!["session.updated".into()],
                before: vec![],
            },
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::ProviderCatalogKey {
            request_id: "catalog-1".into(),
            provider_id: "codex".into(),
            options: ProviderCatalogOptions::Global { force: Some(true) },
        }),
        result("catalog-1", json!("catalog-v1")),
        RuntimeProtocolStep::Send(PluginProcessRequest::Hook {
            request_id: "hook-1".into(),
            kind: HookKind::Event,
            name: "session.created".into(),
            input: json!({"id": 1}),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::HookCancel {
            request_id: "hook-1".into(),
        }),
        RuntimeProtocolStep::Receive(PluginProcessMessage::Error {
            request_id: "hook-1".into(),
            error: "cancelled".into(),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::UsageIdentify {
            request_id: "usage-1".into(),
            source_id: "credits".into(),
            input: json!({"token": "a"}),
        }),
        result("usage-1", json!("usage.identify")),
        RuntimeProtocolStep::Send(PluginProcessRequest::UsageFetch {
            request_id: "usage-2".into(),
            source_id: "credits".into(),
            input: json!({"account": "a"}),
        }),
        result("usage-2", json!("usage.fetch")),
        RuntimeProtocolStep::Send(PluginProcessRequest::UsageDiscover {
            request_id: "usage-3".into(),
            source_id: "credits".into(),
        }),
        result("usage-3", json!("usage.discover")),
        RuntimeProtocolStep::Send(connection()),
        RuntimeProtocolStep::Receive(PluginProcessMessage::ProviderConnected {
            connection_id: "connection-1".into(),
            version: 2,
            capabilities: vec!["sessions".into()],
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::ProviderSend {
            connection_id: "connection-1".into(),
            acceptance_id: "accept-1".into(),
            input: json!({"type": "sessions"}),
        }),
        RuntimeProtocolStep::Receive(PluginProcessMessage::ProviderAccepted {
            connection_id: "connection-1".into(),
            acceptance_id: "accept-1".into(),
        }),
        RuntimeProtocolStep::Receive(PluginProcessMessage::ProviderEvent {
            connection_id: "connection-1".into(),
            event: json!({"type": "sessions", "sessions": []}),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::ProviderClose {
            connection_id: "connection-1".into(),
        }),
        RuntimeProtocolStep::Receive(PluginProcessMessage::ProviderClosed {
            connection_id: "connection-1".into(),
            error: None,
        }),
        RuntimeProtocolStep::Send(connection()),
        RuntimeProtocolStep::Receive(PluginProcessMessage::ProviderConnected {
            connection_id: "connection-1".into(),
            version: 2,
            capabilities: vec!["sessions".into()],
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::PaseoFrame {
            data: json!("frame"),
            is_binary: false,
        }),
        RuntimeProtocolStep::Receive(PluginProcessMessage::PaseoFrame {
            data: json!("frame"),
            is_binary: false,
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::PaseoClose {}),
        RuntimeProtocolStep::Receive(PluginProcessMessage::PaseoClose {}),
    ];
    let loaded = acquired
        .load_with_protocol_steps(&steps, Duration::from_secs(5))
        .expect("execute process envelope");
    assert_eq!(loaded.traffic().len(), 2 + steps.len() + 1);
}

#[test]
fn failed_reviewed_runtime_update_preserves_active_state_after_restart() {
    let root = TestDir::new();
    let repository = root.path().join("repository");
    fs::create_dir_all(&repository).expect("create repository");
    write_runtime_plugin(&repository, "review", "review.v1");
    run(Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(&repository));
    run(Command::new("git")
        .args(["config", "user.name", "Paseo Tests"])
        .current_dir(&repository));
    run(Command::new("git")
        .args(["config", "user.email", "tests@paseo.invalid"])
        .current_dir(&repository));
    run(Command::new("git")
        .args(["add", "."])
        .current_dir(&repository));
    run(Command::new("git")
        .args(["commit", "-m", "working"])
        .current_dir(&repository));
    let working_revision = run(Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&repository));

    let loaded = acquire_git(
        repository.to_str().expect("utf8 repository"),
        ".",
        &working_revision,
        root.path().join("working-checkout"),
        Duration::from_secs(10),
    )
    .expect("acquire working revision")
    .load(Duration::from_secs(5))
    .expect("load working revision");
    let id = loaded.id().clone();
    let state = root.path().join("plugins.json");
    let mut host = PluginHost::open(&state).expect("open host");
    host.install(id.clone(), loaded.into_candidate())
        .expect("activate working revision");
    host.write_settings(&id, BTreeMap::from([("tone".into(), "terse".into())]))
        .expect("persist settings");

    fs::write(repository.join("index.server.ts"), "process.exit(7);\n").expect("break runtime");
    run(Command::new("git")
        .args(["add", "."])
        .current_dir(&repository));
    run(Command::new("git")
        .args(["commit", "-m", "broken update"])
        .current_dir(&repository));
    let broken_revision = run(Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&repository));
    let review = host
        .review_update(&id, broken_revision.clone())
        .expect("review exact broken revision");
    let failure = acquire_git(
        repository.to_str().expect("utf8 repository"),
        ".",
        &broken_revision,
        root.path().join("broken-checkout"),
        Duration::from_secs(10),
    )
    .expect("acquire broken reviewed revision")
    .load(Duration::from_secs(2));
    assert!(failure.is_err());
    drop(review);
    drop(host);

    let restarted = PluginHost::open(&state).expect("restart host after failed update");
    assert_eq!(
        restarted
            .installation(&id)
            .expect("active installation")
            .revision(),
        Some(working_revision.as_str())
    );
    assert_eq!(
        restarted.contributions_for(&id),
        vec![Contribution::Rpc("review.v1".into())]
    );
    assert_eq!(
        restarted.settings(&id),
        Some(&BTreeMap::from([("tone".into(), "terse".into())]))
    );
    println!(
        "PLUGIN_RUNTIME_EVIDENCE {}",
        json!({
            "case": "failed_update_restart",
            "active_revision": working_revision,
            "failed_reviewed_revision": broken_revision,
            "failure_observed": true,
            "restart": {
                "contributions": ["rpc:review.v1"],
                "settings": { "tone": "terse" },
            },
            "filesystem_state": serde_json::from_slice::<serde_json::Value>(
                &fs::read(&state).expect("read persisted state")
            ).expect("parse persisted state"),
        })
    );
}
