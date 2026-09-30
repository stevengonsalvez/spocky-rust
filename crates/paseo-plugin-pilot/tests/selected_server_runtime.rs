use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use paseo_plugin_pilot::{CompiledPluginServer, compile_plugin_server};
use serde_json::json;

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
        "paseo-selected-server-{}-{}.ts",
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
