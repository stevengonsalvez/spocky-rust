use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use spocky_plugin_pilot::{ClientContribution, PluginError, compile_plugin_client};

struct TestDir(PathBuf);

static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "spocky-plugin-client-{}-{nonce}-{}",
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
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn esbuild() -> PathBuf {
    if let Some(path) = std::env::var_os("PASEO_ESBUILD_BIN") {
        return path.into();
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    for ancestor in manifest.ancestors() {
        for root in [ancestor.to_path_buf(), ancestor.join("paseo-rust")] {
            let candidate = root.join(".baselines/paseo-runtime/node_modules/.bin/esbuild");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    panic!("set PASEO_ESBUILD_BIN to the pinned baseline esbuild executable");
}

#[test]
fn client_source_compiles_registers_contributions_and_survives_reconnect_restart() {
    let root = TestDir::new();
    let entry = root.path().join("index.client.tsx");
    fs::write(
        &entry,
        r#"import { getPaseoClient } from "@getpaseo/plugin/client";

export default function contribute(plugin: any) {
  function Main() { return null; }
  plugin.addSurface("main", Main);
  plugin.addSidebarItem({ id: "main", title: "Pilot", icon: "Blocks", surface: "main" });
  plugin.addCommandCenterItem({
    id: "host-generation",
    title: "Host generation",
    icon: "Server",
    context: "global",
    onSelect() { return getPaseoClient("host-a").connectionGeneration; },
  });
  return () => {};
}
"#,
    )
    .expect("write client source");

    let compiled = compile_plugin_client(&entry, &esbuild(), Duration::from_secs(10))
        .expect("compile client source");
    assert!(compiled.bundle().contains("host-generation"));

    let mut runtime = compiled
        .start(Duration::from_secs(5))
        .expect("evaluate bundle");
    assert_eq!(
        runtime.contributions(),
        &[
            ClientContribution::Surface { id: "main".into() },
            ClientContribution::SidebarItem {
                id: "main".into(),
                surface: "main".into(),
            },
            ClientContribution::CommandCenterItem {
                id: "host-generation".into(),
            },
        ]
    );
    assert_eq!(
        runtime
            .invoke_command("host-generation", Duration::from_secs(5))
            .expect("invoke online host"),
        serde_json::json!(1)
    );

    runtime
        .set_host_online(false, Duration::from_secs(5))
        .expect("disconnect host");
    assert_eq!(
        runtime.invoke_command("host-generation", Duration::from_secs(5)),
        Err(PluginError::ClientDisconnected)
    );
    runtime
        .set_host_online(true, Duration::from_secs(5))
        .expect("reconnect host");
    assert_eq!(
        runtime
            .invoke_command("host-generation", Duration::from_secs(5))
            .expect("invoke reconnected host"),
        serde_json::json!(2)
    );
    runtime
        .shutdown(Duration::from_secs(5))
        .expect("shutdown runtime");

    let mut restarted = compiled
        .start(Duration::from_secs(5))
        .expect("restart evaluator");
    assert_eq!(restarted.contributions(), runtime.contributions());
    assert_eq!(
        restarted
            .invoke_command("host-generation", Duration::from_secs(5))
            .expect("invoke after restart"),
        serde_json::json!(1)
    );
    restarted
        .shutdown(Duration::from_secs(5))
        .expect("shutdown restart");
}

#[test]
fn client_compile_and_evaluation_failures_do_not_poison_next_runtime() {
    let root = TestDir::new();
    let invalid_import = root.path().join("invalid-import.ts");
    fs::write(
        &invalid_import,
        "import fs from 'node:fs'; export default function() { return () => fs; }\n",
    )
    .expect("write invalid import");
    assert!(matches!(
        compile_plugin_client(&invalid_import, &esbuild(), Duration::from_secs(10)),
        Err(PluginError::ClientCompileFailed(_))
    ));

    let broken_setup = root.path().join("broken-setup.ts");
    fs::write(
        &broken_setup,
        "export default function() { throw new Error('setup exploded'); }\n",
    )
    .expect("write broken setup");
    let broken = compile_plugin_client(&broken_setup, &esbuild(), Duration::from_secs(10))
        .expect("compile broken setup");
    assert!(matches!(
        broken.start(Duration::from_secs(5)),
        Err(PluginError::ClientEvaluationFailed(message)) if message.contains("setup exploded")
    ));

    let recovered = root.path().join("recovered.ts");
    fs::write(
        &recovered,
        "export default function(plugin: any) { plugin.addSurface('recovered', function() {}); return () => {}; }\n",
    )
    .expect("write recovered setup");
    let compiled = compile_plugin_client(&recovered, &esbuild(), Duration::from_secs(10))
        .expect("compile recovered setup");
    let mut runtime = compiled
        .start(Duration::from_secs(5))
        .expect("evaluate recovered setup");
    assert_eq!(
        runtime.contributions(),
        &[ClientContribution::Surface {
            id: "recovered".into()
        }]
    );
    runtime
        .shutdown(Duration::from_secs(5))
        .expect("shutdown recovered runtime");
}
