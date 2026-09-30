use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use spocky_plugin_pilot::{
    CompiledPluginServer, HookKind, PluginProcessMessage, PluginProcessRequest,
    ProviderConnectRequest, ProviderEvent, ProviderInput, RuntimeProtocolStep,
    compile_plugin_server,
};

const SERVER_BUNDLE: &str = r#"(function(require) {
  const { defineRpc } = require("@getpaseo/plugin");
  return { default(server) {
    const echo = defineRpc({ name: "pilot.echo", input: {}, output: {} });
    server.handle(echo, async (input) => ({ ...input, worker: "node" }));
    server.on("agent.created", () => {});
    server.before("agent.create", () => {});
    server.registerProvider({ id: "direct", label: "Direct", connect() {} });
    server.registerUsageSource({ id: "credits", label: "Credits", input: {}, identify() {}, fetch() {} });
    return () => {};
  } };
})"#;

const FULL_SERVER_BUNDLE: &str = r#"(function(require) {
  const { defineRpc, defineSettings } = require("@getpaseo/plugin");
  return { default(server) {
    const preferences = server.registerSettings(defineSettings({
      id: "preferences",
      scope: "host",
      version: 1,
      schema: { async parseAsync(value) { return { enabled: value.enabled ?? true }; } },
    }));
    preferences.subscribe((state) => { globalThis.lastSettings = state; });
    server.handle(defineRpc({ name: "settings.snapshot", input: {}, output: {} }), async () => ({
      current: await preferences.read(),
      observed: globalThis.lastSettings ?? null,
    }));
    server.handle(defineRpc({ name: "daemon.sessions", input: {}, output: {} }), async (_, { paseo }) =>
      paseo.sessions.list({ limit: 2 }));
    server.on("session.created", (input) => ({ observed: input.id }));
    server.before("session.prompt", async (_input, { signal }) =>
      new Promise((_resolve, reject) => signal.addEventListener("abort", () => reject(new Error("cancelled")))));
    server.registerUsageSource({
      id: "credits",
      label: "Credits",
      input: { async parseAsync(value) { return value; } },
      identify: async (input) => ({ account: input.token }),
      fetch: async (input) => ({ remaining: input.account.length }),
      discover: async () => [{ token: "found" }],
    });
    server.registerProvider({
      id: "direct",
      label: "Direct",
      async connect(request) {
        if (request.capabilities.includes("fail")) throw new Error("unavailable");
        let listener = () => {};
        return {
          version: 2,
          capabilities: ["sessions"],
          onEvent(next) { listener = next; return () => { listener = () => {}; }; },
          async send(input) { listener({ type: "sessions", requestId: input.requestId, sessions: [] }); },
          async close() {},
        };
      },
    });
    return () => {};
  } };
})"#;

const MIGRATING_SETTINGS_BUNDLE: &str = r#"(function(require) {
  const { defineRpc, defineSettings } = require("@getpaseo/plugin");
  return { default(server) {
    const preferences = server.registerSettings(defineSettings({
      id: "preferences",
      scope: "host",
      version: 2,
      schema: {
        async parseAsync(value) {
          return { total: typeof value.total === "number" ? value.total : 0 };
        },
      },
      async migrate(values, version) {
        if (version !== 1) throw new Error(`unexpected source version: ${version}`);
        return { total: values.count };
      },
    }));
    server.handle(defineRpc({ name: "settings.snapshot", input: {}, output: {} }), async () =>
      preferences.read());
    return () => {};
  } };
})"#;

const FAILING_SETTINGS_MIGRATION_BUNDLE: &str = r#"(function(require) {
  const { defineRpc, defineSettings } = require("@getpaseo/plugin");
  return { default(server) {
    const preferences = server.registerSettings(defineSettings({
      id: "preferences",
      scope: "host",
      version: 2,
      schema: {
        async parseAsync(value) {
          return { total: typeof value.total === "number" ? value.total : 0 };
        },
      },
      async migrate() { throw new Error("migration failed"); },
    }));
    server.handle(defineRpc({ name: "settings.snapshot", input: {}, output: {} }), async () =>
      preferences.read());
    return () => {};
  } };
})"#;

fn esbuild() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for ancestor in manifest.ancestors() {
        for root in [ancestor.to_path_buf(), ancestor.join("paseo-rust")] {
            let candidate = root.join(".baselines/paseo-runtime/node_modules/.bin/esbuild");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    panic!("pinned esbuild executable missing");
}

#[test]
fn pinned_server_source_compiles_into_selected_wrapper() {
    let path = std::env::temp_dir().join(format!(
        "spocky-selected-server-{}-{}.ts",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::write(
        &path,
        r#"import { defineRpc } from "@getpaseo/plugin";
export default function contribute(server: any) {
  server.handle(defineRpc({ name: "pilot.typed", input: {}, output: {} }), (input: any) => ({ value: input.value + 1 }));
  return () => {};
}
"#,
    )
    .expect("write pinned-shape server source");
    let compiled = compile_plugin_server(&path, &esbuild(), Duration::from_secs(10))
        .expect("compile server source");
    let loaded = compiled
        .run_and_invoke(
            "selected",
            "/tmp/selected-plugin",
            "pilot.typed",
            json!({"value": 4}),
            Duration::from_secs(5),
        )
        .expect("run compiled source");
    assert_eq!(loaded.invocation_output(), Some(&json!({"value": 5})));
    fs::remove_file(path).expect("remove source fixture");
}

#[test]
fn rust_selected_wrapper_runs_compiled_bundle_in_bounded_node_worker() {
    let compiled = CompiledPluginServer::from_bundle(SERVER_BUNDLE);
    let loaded = compiled
        .run_and_invoke(
            "selected",
            "/tmp/selected-plugin",
            "pilot.echo",
            json!({"value": 7}),
            Duration::from_secs(5),
        )
        .expect("run selected production wrapper");

    assert_eq!(
        loaded.invocation_output(),
        Some(&json!({"value": 7, "worker": "node"}))
    );
    assert_eq!(
        loaded.contribution_labels(),
        [
            "rpc:pilot.echo",
            "provider:direct",
            "usage:credits",
            "hook:event:agent.created",
            "hook:before:agent.create",
        ]
    );
    assert!(loaded.worker_exited());
}

#[test]
fn selected_wrapper_reports_failure_and_recovers_with_fresh_worker() {
    let broken =
        CompiledPluginServer::from_bundle("(function() { throw new Error('broken bundle'); })");
    assert!(
        broken
            .run("selected", "/tmp/selected-plugin", Duration::from_secs(2))
            .is_err()
    );

    let working = CompiledPluginServer::from_bundle(SERVER_BUNDLE);
    assert!(
        working
            .run("selected", "/tmp/selected-plugin", Duration::from_secs(5))
            .is_ok()
    );
}

#[test]
fn selected_wrapper_migrates_pinned_settings_once_and_persists_the_new_version() {
    let root = std::env::temp_dir().join(format!(
        "spocky-selected-migration-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let settings = root.join("settings");
    fs::create_dir_all(&settings).expect("create settings fixture");
    fs::write(
        settings.join("preferences.json"),
        r#"{"version":1,"values":{"count":7}}"#,
    )
    .expect("write version one settings");
    let compiled = CompiledPluginServer::from_bundle(MIGRATING_SETTINGS_BUNDLE);
    let read = RuntimeProtocolStep::Send(PluginProcessRequest::Invoke {
        request_id: "settings-read".into(),
        method: "settings.snapshot".into(),
        input: json!({}),
    });
    let migrated = result(
        "settings-read",
        json!({
            "status":"ready",
            "revision":"d420654e99279ca1ed8b93f0bc7a910d5b4227d2bf938c7dc368a5de5688b23c",
            "values":{"total":7}
        }),
    );

    compiled
        .run_with_protocol_steps(
            "selected",
            "/tmp/selected-plugin",
            &settings,
            &[
                read.clone(),
                RuntimeProtocolStep::Receive(PluginProcessMessage::SettingsChanged {
                    settings_id: "preferences".into(),
                }),
                migrated.clone(),
            ],
            Duration::from_secs(5),
        )
        .expect("migrate version one settings");
    assert_eq!(
        fs::read_to_string(settings.join("preferences.json")).expect("read migrated settings"),
        r#"{"version":2,"values":{"total":7}}"#
    );

    compiled
        .run_with_protocol_steps(
            "selected",
            "/tmp/selected-plugin",
            &settings,
            &[read, migrated],
            Duration::from_secs(5),
        )
        .expect("restart reads migrated settings without migrating again");
    fs::remove_dir_all(root).expect("remove fixture root");
}

#[test]
fn selected_wrapper_preserves_failed_migration_until_explicit_reset() {
    let root = std::env::temp_dir().join(format!(
        "spocky-selected-failed-migration-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let settings = root.join("settings");
    fs::create_dir_all(&settings).expect("create settings fixture");
    let original = r#"{"version":1,"values":{"count":7}}"#;
    fs::write(settings.join("preferences.json"), original).expect("write version one settings");
    let compiled = CompiledPluginServer::from_bundle(FAILING_SETTINGS_MIGRATION_BUNDLE);

    compiled
        .run_with_protocol_steps(
            "selected",
            "/tmp/selected-plugin",
            &settings,
            &[
                RuntimeProtocolStep::Send(PluginProcessRequest::Invoke {
                    request_id: "settings-read".into(),
                    method: "settings.snapshot".into(),
                    input: json!({}),
                }),
                result(
                    "settings-read",
                    json!({
                        "status":"invalid",
                        "revision":"eaa5946e7501f2f983eef5cf9bf0e5f33b5402598eed3c7529a85c59eed96a23",
                        "error":"migration failed"
                    }),
                ),
                RuntimeProtocolStep::Send(PluginProcessRequest::Invoke {
                    request_id: "settings-write".into(),
                    method: "settings.preferences.write".into(),
                    input: json!({
                        "revision":"eaa5946e7501f2f983eef5cf9bf0e5f33b5402598eed3c7529a85c59eed96a23",
                        "values":{"total":9}
                    }),
                }),
                result(
                    "settings-write",
                    json!({
                        "status":"invalid",
                        "error":"Reload or reset settings before saving a different schema version"
                    }),
                ),
            ],
            Duration::from_secs(5),
        )
        .expect("surface failed migration");
    assert_eq!(
        fs::read_to_string(settings.join("preferences.json")).expect("read preserved settings"),
        original
    );

    compiled
        .run_with_protocol_steps(
            "selected",
            "/tmp/selected-plugin",
            &settings,
            &[
                RuntimeProtocolStep::Send(PluginProcessRequest::Invoke {
                    request_id: "settings-reset".into(),
                    method: "settings.preferences.reset".into(),
                    input: json!({
                        "revision":"eaa5946e7501f2f983eef5cf9bf0e5f33b5402598eed3c7529a85c59eed96a23"
                    }),
                }),
                RuntimeProtocolStep::Receive(PluginProcessMessage::SettingsChanged {
                    settings_id: "preferences".into(),
                }),
                result(
                    "settings-reset",
                    json!({
                        "status":"saved",
                        "revision":"b6c43f46dd20c412e1874c356f3d2baa93058ec82f19bef471610c7200da0865",
                        "values":{"total":0}
                    }),
                ),
            ],
            Duration::from_secs(5),
        )
        .expect("reset failed migration");
    assert_eq!(
        fs::read_to_string(settings.join("preferences.json")).expect("read reset settings"),
        r#"{"version":2,"values":{"total":0}}"#
    );
    fs::remove_dir_all(root).expect("remove fixture root");
}

#[test]
fn selected_wrapper_routes_binary_ipc_frames_without_regressing_text_rpc() {
    let root = std::env::temp_dir().join(format!(
        "spocky-selected-binary-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let settings = root.join("settings");
    fs::create_dir_all(&root).expect("create fixture root");
    let compiled = CompiledPluginServer::from_bundle(FULL_SERVER_BUNDLE);
    let response =
        r#"{"type":"response","requestId":"paseo-2","output":{"entries":[{"id":"binary"}]}}"#;
    let steps = [
        RuntimeProtocolStep::Send(PluginProcessRequest::Invoke {
            request_id: "text-call".into(),
            method: "daemon.sessions".into(),
            input: json!({}),
        }),
        RuntimeProtocolStep::Receive(PluginProcessMessage::PaseoFrame {
            data: json!(
                r#"{"type":"request","requestId":"paseo-1","method":"sessions.list","input":{"limit":2}}"#
            ),
            is_binary: false,
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::PaseoFrame {
            data: json!(
                r#"{"type":"response","requestId":"paseo-1","output":{"entries":[{"id":"text"}]}}"#
            ),
            is_binary: false,
        }),
        result("text-call", json!({"entries":[{"id":"text"}]})),
        RuntimeProtocolStep::Send(PluginProcessRequest::Invoke {
            request_id: "binary-call".into(),
            method: "daemon.sessions".into(),
            input: json!({}),
        }),
        RuntimeProtocolStep::Receive(PluginProcessMessage::PaseoFrame {
            data: json!(
                r#"{"type":"request","requestId":"paseo-2","method":"sessions.list","input":{"limit":2}}"#
            ),
            is_binary: false,
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::PaseoFrame {
            data: json!(response.as_bytes()),
            is_binary: true,
        }),
        result("binary-call", json!({"entries":[{"id":"binary"}]})),
    ];

    compiled
        .run_with_protocol_steps(
            "selected",
            "/tmp/selected-plugin",
            &settings,
            &steps,
            Duration::from_secs(5),
        )
        .expect("route text and binary daemon frames");
    fs::remove_dir_all(root).expect("remove fixture root");
}

fn result(request_id: &str, output: serde_json::Value) -> RuntimeProtocolStep {
    RuntimeProtocolStep::Receive(PluginProcessMessage::Result {
        request_id: request_id.into(),
        output,
    })
}

#[test]
#[allow(clippy::too_many_lines)]
fn selected_wrapper_routes_headless_plugin_contracts_and_persists_settings() {
    let root = std::env::temp_dir().join(format!(
        "spocky-selected-full-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let settings = root.join("settings");
    fs::create_dir_all(&root).expect("create fixture root");
    let compiled = CompiledPluginServer::from_bundle(FULL_SERVER_BUNDLE);
    let connect = |capabilities: Vec<String>, connection_id: &str| {
        RuntimeProtocolStep::Send(PluginProcessRequest::ProviderConnect {
            provider_id: "direct".into(),
            connection_id: connection_id.into(),
            request: ProviderConnectRequest {
                versions: vec![1, 2],
                capabilities,
            },
        })
    };
    let steps = vec![
        RuntimeProtocolStep::Send(PluginProcessRequest::Invoke {
            request_id: "settings-read-1".into(),
            method: "settings.preferences.read".into(),
            input: json!({}),
        }),
        result(
            "settings-read-1",
            json!({"status":"ready","revision":"missing","values":{"enabled":true}}),
        ),
        RuntimeProtocolStep::Send(PluginProcessRequest::Invoke {
            request_id: "settings-write".into(),
            method: "settings.preferences.write".into(),
            input: json!({"revision":"missing","values":{"enabled":false}}),
        }),
        RuntimeProtocolStep::Receive(PluginProcessMessage::SettingsChanged {
            settings_id: "preferences".into(),
        }),
        result(
            "settings-write",
            json!({"status":"saved","revision":"c5bc420158555027be715d6b00845243be72562dd1147c0095c9f40f3971e8c6","values":{"enabled":false}}),
        ),
        RuntimeProtocolStep::Send(PluginProcessRequest::UsageIdentify {
            request_id: "usage-identify".into(),
            source_id: "credits".into(),
            input: json!({"token":"acct"}),
        }),
        result("usage-identify", json!({"account":"acct"})),
        RuntimeProtocolStep::Send(PluginProcessRequest::UsageFetch {
            request_id: "usage-fetch".into(),
            source_id: "credits".into(),
            input: json!({"account":"acct"}),
        }),
        result("usage-fetch", json!({"remaining":4})),
        RuntimeProtocolStep::Send(PluginProcessRequest::UsageDiscover {
            request_id: "usage-discover".into(),
            source_id: "credits".into(),
        }),
        result("usage-discover", json!([{"token":"found"}])),
        connect(vec!["sessions".into()], "connection-1"),
        RuntimeProtocolStep::Receive(PluginProcessMessage::ProviderConnected {
            connection_id: "connection-1".into(),
            version: 2,
            capabilities: vec!["sessions".into()],
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::ProviderSend {
            connection_id: "connection-1".into(),
            acceptance_id: "accept-1".into(),
            input: ProviderInput::Sessions {
                request_id: "sessions-1".into(),
                query: None,
                cwd: None,
                limit: None,
            },
        }),
        RuntimeProtocolStep::Receive(PluginProcessMessage::ProviderEvent {
            connection_id: "connection-1".into(),
            event: ProviderEvent::Sessions {
                request_id: "sessions-1".into(),
                sessions: vec![],
            },
        }),
        RuntimeProtocolStep::Receive(PluginProcessMessage::ProviderAccepted {
            connection_id: "connection-1".into(),
            acceptance_id: "accept-1".into(),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::ProviderClose {
            connection_id: "connection-1".into(),
        }),
        RuntimeProtocolStep::Receive(PluginProcessMessage::ProviderClosed {
            connection_id: "connection-1".into(),
            error: None,
        }),
        connect(vec!["fail".into()], "connection-failed"),
        RuntimeProtocolStep::Receive(PluginProcessMessage::ProviderConnectFailed {
            connection_id: "connection-failed".into(),
            error: "unavailable".into(),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::Hook {
            request_id: "event-hook".into(),
            kind: HookKind::Event,
            name: "session.created".into(),
            input: json!({"id":"session-1"}),
        }),
        result("event-hook", json!({"observed":"session-1"})),
        RuntimeProtocolStep::Send(PluginProcessRequest::Hook {
            request_id: "before-hook".into(),
            kind: HookKind::Before,
            name: "session.prompt".into(),
            input: json!({}),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::HookCancel {
            request_id: "before-hook".into(),
        }),
        RuntimeProtocolStep::Receive(PluginProcessMessage::Error {
            request_id: "before-hook".into(),
            error: "cancelled".into(),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::Invoke {
            request_id: "daemon-call".into(),
            method: "daemon.sessions".into(),
            input: json!({}),
        }),
        RuntimeProtocolStep::Receive(PluginProcessMessage::PaseoFrame {
            data: json!(
                r#"{"type":"request","requestId":"paseo-1","method":"sessions.list","input":{"limit":2}}"#
            ),
            is_binary: false,
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::PaseoFrame {
            data: json!(r#"{"type":"response","requestId":"paseo-1","output":{"entries":[]}}"#),
            is_binary: false,
        }),
        result("daemon-call", json!({"entries":[]})),
    ];

    let run = compiled
        .run_with_protocol_steps(
            "selected",
            "/tmp/selected-plugin",
            &settings,
            &steps,
            Duration::from_secs(5),
        )
        .expect("run full selected worker flow");
    assert!(run.worker_exited());
    assert!(settings.join("preferences.json").is_file());
    assert!(
        fs::read_dir(&settings)
            .expect("read settings")
            .all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
    );

    let persisted = compiled
        .run_with_protocol_steps(
            "selected",
            "/tmp/selected-plugin",
            &settings,
            &[
                RuntimeProtocolStep::Send(PluginProcessRequest::Invoke {
                    request_id: "snapshot".into(),
                    method: "settings.snapshot".into(),
                    input: json!({}),
                }),
                result(
                    "snapshot",
                    json!({
                        "current":{"status":"ready","revision":"c5bc420158555027be715d6b00845243be72562dd1147c0095c9f40f3971e8c6","values":{"enabled":false}},
                        "observed":null
                    }),
                ),
            ],
            Duration::from_secs(5),
        )
        .expect("fresh worker reads persisted settings");
    assert!(persisted.worker_exited());
    fs::remove_dir_all(root).expect("remove fixture root");
}
