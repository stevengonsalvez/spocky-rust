use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use paseo_plugin_pilot::{
    Contribution, PluginHost, PluginSourceIdentity, acquire_git, acquire_npm_tarball,
};
use serde_json::json;

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "paseo-plugin-runtime-{}-{nonce}",
            std::process::id()
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
        fs::remove_dir_all(&self.0).expect("remove test directory");
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
        format!(r#"{{"id":"{id}","server":"runtime.js"}}"#),
    )
    .expect("write manifest");
    fs::write(
        root.join("runtime.js"),
        format!(
            r#"const readline = require("node:readline");
const lines = readline.createInterface({{ input: process.stdin }});
lines.on("line", (line) => {{
  const message = JSON.parse(line);
  if (message.type === "initialize") console.log(JSON.stringify({{ type: "ready", contributions: [{{ kind: "rpc", id: "{contribution}" }}] }}));
  if (message.type === "shutdown") {{ console.log(JSON.stringify({{ type: "stopped" }})); process.exit(0); }}
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
        .load(Duration::from_secs(5))
        .expect("run acquired plugin process");
    assert_eq!(loaded.id().as_str(), "git-review");
    assert_eq!(
        loaded.contributions(),
        &[Contribution::Rpc("review.start".into())]
    );
    assert_eq!(loaded.traffic().len(), 4);
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
fn npm_tarball_acquisition_runs_contribution_process_at_package_version() {
    let root = TestDir::new();
    let package = root.path().join("package");
    fs::create_dir_all(&package).expect("create package");
    fs::write(
        package.join("package.json"),
        r#"{"name":"@acme/review","version":"1.2.3","files":["paseo-plugin.json","runtime.js"]}"#,
    )
    .expect("write package manifest");
    write_runtime_plugin(&package, "npm-review", "review.settings");
    let archive_name = run(Command::new("npm")
        .args(["pack", "--silent"])
        .current_dir(&package));
    let archive = package.join(archive_name.lines().last().expect("archive name"));

    let acquired = acquire_npm_tarball(
        &archive,
        "@acme/review",
        ".",
        root.path().join("installation"),
        Duration::from_secs(15),
    )
    .expect("acquire npm tarball");
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
    assert_eq!(loaded.traffic().len(), 4);
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

    fs::write(repository.join("runtime.js"), "process.exit(7);\n").expect("break runtime");
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
