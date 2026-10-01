use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use spocky_plugin_pilot::{
    CompiledPluginServer, HookKind, PluginError, PluginProcessMessage, PluginProcessRequest,
    ProviderCatalogOptions, ProviderConnectRequest, ProviderEvent, ProviderInput,
    RuntimeProtocolStep,
};

const DIFFERENTIAL_BUNDLE: &str = r#"(function() {
  return { default(server) {
    const eventOrder = [];
    server.handle({ name: "event.order" }, async () => eventOrder);
    server.before("workspace.create", ({ request }) => ({ ...request, title: "first" }));
    server.before("workspace.create", ({ request }) => ({ ...request, title: `${request.title}:second` }));
    server.on("workspace.created", () => { eventOrder.push("failed"); throw new Error("observer failed"); });
    server.on("workspace.created", () => { eventOrder.push("continued"); });
    server.before("agent.session_open", (_input, { signal }) =>
      new Promise((_resolve, reject) => signal.addEventListener("abort", () => reject(new Error("hook canceled")), { once: true })));
    server.registerUsageSource({
      id: "credits",
      label: "Credits",
      input: { async parseAsync(value) {
        if (typeof value?.account !== "string") throw new Error("invalid account payload");
        return value;
      } },
      async identify(input) { return { key: input.account, label: "Work" }; },
      async fetch(input) { return { status: "available", windows: [{ id: "daily", label: input.account, usedPct: 25 }] }; },
      async discover() { return [{ account: "discovered" }]; },
    });
    server.registerProvider({
      id: "direct",
      label: "Direct",
      async getCatalogCacheKey(options) { return `${options.scope}:shared`; },
      async connect(request) {
        if (request.capabilities.includes("fail")) throw new Error("connect failed");
        let listener = () => {};
        return {
          version: 1,
          capabilities: ["session.list"],
          async send(input) {
            if (input.type === "catalog") throw new Error("send rejected");
            listener({ type: "request.completed", requestId: input.requestId });
          },
          onEvent(next) { listener = next; return () => { listener = () => {}; }; },
          async close() {},
        };
      },
    });
    return () => {};
  } };
})"#;

fn receive(message: PluginProcessMessage) -> RuntimeProtocolStep {
    RuntimeProtocolStep::Receive(message)
}

#[test]
fn selected_worker_bounds_hung_hooks_and_surfaces_process_death() {
    let hung = CompiledPluginServer::from_bundle(
        r#"(function() { return { default(server) {
          server.before("workspace.create", async () => new Promise(() => {}));
          return () => {};
        } }; })"#,
    );
    let hung_steps = [RuntimeProtocolStep::Send(PluginProcessRequest::Hook {
        request_id: "hung".into(),
        kind: HookKind::Before,
        name: "workspace.create".into(),
        input: json!({"source":{"kind":"directory","path":"/project"}}),
    })];
    let root = std::env::temp_dir().join(format!("spocky-hook-failure-{}", std::process::id()));
    fs::create_dir_all(&root).expect("create failure settings directory");
    let timeout = hung
        .run_with_protocol_steps(
            "selected",
            "/plugin",
            &root,
            &[
                hung_steps[0].clone(),
                receive(PluginProcessMessage::Result {
                    request_id: "hung".into(),
                    output: json!(null),
                }),
            ],
            Duration::from_millis(100),
        )
        .expect_err("hung hook must time out");
    assert_eq!(timeout, PluginError::RuntimeTimedOut);

    let dying = CompiledPluginServer::from_bundle(
        r#"(function() { return { default(server) {
          server.on("workspace.created", () => process.exit(17));
          return () => {};
        } }; })"#,
    );
    let death = dying
        .run_with_protocol_steps(
            "selected",
            "/plugin",
            &root,
            &[
                RuntimeProtocolStep::Send(PluginProcessRequest::Hook {
                    request_id: "death".into(),
                    kind: HookKind::Event,
                    name: "workspace.created".into(),
                    input: json!({"workspace":{"id":"workspace"}}),
                }),
                receive(PluginProcessMessage::Result {
                    request_id: "death".into(),
                    output: json!(null),
                }),
            ],
            Duration::from_secs(2),
        )
        .expect_err("worker death must reject the exchange");
    assert_eq!(death, PluginError::RuntimeProtocol);
    println!(
        "PLUGIN_HOOK_RUST_FAILURES {}",
        json!({"timeout":"terminated","processDeath":"rejected"})
    );
    fs::remove_dir_all(root).expect("remove failure settings directory");
}

#[test]
#[allow(clippy::too_many_lines)]
fn differential_capture_matches_pinned_hook_usage_and_provider_cases() {
    let root = std::env::temp_dir().join(format!(
        "spocky-hook-runtime-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("create temporary settings directory");
    let steps = vec![
        RuntimeProtocolStep::Send(PluginProcessRequest::Hook {
            request_id: "before-success".into(),
            kind: HookKind::Before,
            name: "workspace.create".into(),
            input: json!({"source":{"kind":"directory","path":"/project"}}),
        }),
        receive(PluginProcessMessage::Result {
            request_id: "before-success".into(),
            output: json!({"title":"first:second","source":{"kind":"directory","path":"/project"}}),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::Hook {
            request_id: "event-failure".into(),
            kind: HookKind::Event,
            name: "workspace.created".into(),
            input: json!({"workspace":{"id":"workspace"}}),
        }),
        receive(PluginProcessMessage::Result {
            request_id: "event-failure".into(),
            output: json!(null),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::Invoke {
            request_id: "event-order".into(),
            method: "event.order".into(),
            input: json!({}),
        }),
        receive(PluginProcessMessage::Result {
            request_id: "event-order".into(),
            output: json!(["failed", "continued"]),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::Hook {
            request_id: "hook-timeout".into(),
            kind: HookKind::Before,
            name: "agent.session_open".into(),
            input: json!({
                "agentId":"agent", "workspaceId":null, "provider":"direct", "cwd":"/project",
                "reason":"resume", "purpose":"interactive", "env":{}
            }),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::HookCancel {
            request_id: "hook-timeout".into(),
        }),
        receive(PluginProcessMessage::Error {
            request_id: "hook-timeout".into(),
            error: "hook canceled".into(),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::UsageIdentify {
            request_id: "usage-identify".into(),
            source_id: "credits".into(),
            input: json!({"account":"work"}),
        }),
        receive(PluginProcessMessage::Result {
            request_id: "usage-identify".into(),
            output: json!({"key":"work","label":"Work"}),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::UsageFetch {
            request_id: "usage-fetch".into(),
            source_id: "credits".into(),
            input: json!({"account":"work"}),
        }),
        receive(PluginProcessMessage::Result {
            request_id: "usage-fetch".into(),
            output: json!({"status":"available","windows":[{"id":"daily","label":"work","usedPct":25}]}),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::UsageDiscover {
            request_id: "usage-discover".into(),
            source_id: "credits".into(),
        }),
        receive(PluginProcessMessage::Result {
            request_id: "usage-discover".into(),
            output: json!([{"account":"discovered"}]),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::UsageFetch {
            request_id: "usage-malformed".into(),
            source_id: "credits".into(),
            input: json!({"account":7}),
        }),
        receive(PluginProcessMessage::Error {
            request_id: "usage-malformed".into(),
            error: "invalid account payload".into(),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::ProviderCatalogKey {
            request_id: "catalog-key".into(),
            provider_id: "direct".into(),
            options: ProviderCatalogOptions::Global { force: None },
        }),
        receive(PluginProcessMessage::Result {
            request_id: "catalog-key".into(),
            output: json!("global:shared"),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::ProviderConnect {
            provider_id: "direct".into(),
            connection_id: "connection".into(),
            request: ProviderConnectRequest {
                versions: vec![1],
                capabilities: vec![],
            },
        }),
        receive(PluginProcessMessage::ProviderConnected {
            connection_id: "connection".into(),
            version: 1,
            capabilities: vec!["session.list".into()],
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::ProviderSend {
            connection_id: "connection".into(),
            acceptance_id: "accepted".into(),
            input: ProviderInput::Sessions {
                request_id: "sessions".into(),
                query: None,
                cwd: None,
                limit: None,
            },
        }),
        receive(PluginProcessMessage::ProviderEvent {
            connection_id: "connection".into(),
            event: ProviderEvent::RequestCompleted {
                request_id: "sessions".into(),
            },
        }),
        receive(PluginProcessMessage::ProviderAccepted {
            connection_id: "connection".into(),
            acceptance_id: "accepted".into(),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::ProviderSend {
            connection_id: "connection".into(),
            acceptance_id: "rejected".into(),
            input: ProviderInput::Catalog {
                request_id: "catalog".into(),
                cwd: None,
            },
        }),
        receive(PluginProcessMessage::ProviderRejected {
            connection_id: "connection".into(),
            acceptance_id: "rejected".into(),
            error: "send rejected".into(),
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::ProviderClose {
            connection_id: "connection".into(),
        }),
        receive(PluginProcessMessage::ProviderClosed {
            connection_id: "connection".into(),
            error: None,
        }),
        RuntimeProtocolStep::Send(PluginProcessRequest::ProviderConnect {
            provider_id: "direct".into(),
            connection_id: "connection-failed".into(),
            request: ProviderConnectRequest {
                versions: vec![1],
                capabilities: vec!["fail".into()],
            },
        }),
        receive(PluginProcessMessage::ProviderConnectFailed {
            connection_id: "connection-failed".into(),
            error: "connect failed".into(),
        }),
    ];
    let run = CompiledPluginServer::from_bundle(DIFFERENTIAL_BUNDLE)
        .run_with_protocol_steps("selected", "/plugin", &root, &steps, Duration::from_secs(5))
        .expect("run selected hook, usage, and provider differential");
    let ready = run
        .traffic()
        .iter()
        .find_map(|entry| {
            let value: serde_json::Value = serde_json::from_str(entry.message()).ok()?;
            (value["type"] == "ready").then_some(value)
        })
        .expect("ready frame");
    let capture = json!({
        "ready": ready,
        "beforeSuccess": {"type":"result","requestId":"before-success","output":{"title":"first:second","source":{"kind":"directory","path":"/project"}}},
        "eventFailure": {"type":"result","requestId":"event-failure","output":null},
        "eventOrder": ["failed","continued"],
        "hookCanceled": {"type":"error","requestId":"hook-timeout","error":"hook canceled"},
        "usageIdentify": {"type":"result","requestId":"usage-identify","output":{"key":"work","label":"Work"}},
        "usageFetch": {"type":"result","requestId":"usage-fetch","output":{"status":"available","windows":[{"id":"daily","label":"work","usedPct":25}]}},
        "usageDiscover": {"type":"result","requestId":"usage-discover","output":[{"account":"discovered"}]},
        "usageMalformed": {"type":"error","requestId":"usage-malformed","error":"invalid account payload"},
        "catalogKey": {"type":"result","requestId":"catalog-key","output":"global:shared"},
        "connected": {"type":"provider.connected","connectionId":"connection","version":1,"capabilities":["session.list"]},
        "providerEvent": {"type":"provider.event","connectionId":"connection","event":{"type":"request.completed","requestId":"sessions"}},
        "accepted": {"type":"provider.accepted","connectionId":"connection","acceptanceId":"accepted"},
        "rejected": {"type":"provider.rejected","connectionId":"connection","acceptanceId":"rejected","error":"send rejected"},
        "closed": {"type":"provider.closed","connectionId":"connection"},
        "connectFailed": {"type":"provider.connect_failed","connectionId":"connection-failed","error":"connect failed"}
    });
    println!("PLUGIN_HOOK_RUST {capture}");
    fs::remove_dir_all(root).expect("remove temporary settings directory");
}
