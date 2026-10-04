//! Differential check of `terminateWithTreeKill`: the pinned `tree-kill`
//! helper and `terminate_with_tree_kill` stop the same process tree (a root
//! shell with two children, one of which has a child of its own) and must
//! signal the same roles in the same order, report the same result and leave
//! `child.killed` the same. Pids differ per run, so each is printed as its
//! role (`root`, `a`, `b`, `g`).

mod support;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_provider_claude::process::{
    ChildProcess, SpawnRequest, TerminateResult, observe_signals, terminate_with_tree_kill,
};

const CHILD_SCENARIO: &str = "tree-kill-child.txt";
const CHILD_OUT: &str = "tree-kill-child.out";
const ROLES: [&str; 4] = ["root", "a", "b", "g"];
const GRACEFUL_MS: u64 = 500;
const FORCE_MS: u64 = 2000;

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "utils/tree-kill.js",
        "1ec1fb831b3aefb2acd8e89ea02fe4201d1dc6260e7bfa76940e7a7055413734",
    ),
    (
        "../../../../node_modules/tree-kill/index.js",
        "15bad404883f6967c282dbd5a0ce54088353dc5ad9ac9fb690d8868f8fd57315",
    ),
];

const NODE_SCRIPT: &str = r#"
import { spawn } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
const [dist, dir, graceful, force, mode] = process.argv.slice(1);
const { terminateWithTreeKill } = await import(`${dist}/utils/tree-kill.js`);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const child = spawn("/bin/sh", [`${dir}/root.sh`], { env: { DIR: dir, IGNORE: mode === "ignore" ? "1" : "" }, stdio: "ignore" });
for (let i = 0; i < 200 && !existsSync(`${dir}/ready`); i++) await sleep(25);
const roles = {};
for (const role of ["root", "a", "b", "g"]) roles[readFileSync(`${dir}/${role}.pid`, "utf8").trim()] = role;
const lines = [];
const realKill = process.kill.bind(process);
process.kill = (pid, signal) => {
  lines.push(`kill ${roles[pid] ?? "other"} ${signal ?? "SIGTERM"}`);
  return realKill(pid, signal);
};
const childKill = child.kill.bind(child);
child.kill = (signal) => {
  lines.push(`child.kill ${signal ?? "SIGTERM"}`);
  return childKill(signal);
};
if (mode === "exited") {
  childKill("SIGKILL");
  await new Promise((resolve) => child.once("exit", resolve));
}
const result = await terminateWithTreeKill(child, { gracefulTimeoutMs: Number(graceful), forceTimeoutMs: Number(force) });
lines.push(`result ${result}`);
lines.push(`killed ${child.killed}`);
process.stdout.write(lines.join("\n") + "\n");
process.exit(0);
"#;

const ROOT: &str = r#"#!/bin/sh
echo $$ > "$DIR/root.pid"
"$DIR/a.sh" &
"$DIR/b.sh" &
if [ -n "$IGNORE" ]; then trap '' TERM; fi
until [ -s "$DIR/a.pid" ] && [ -s "$DIR/b.pid" ] && [ -s "$DIR/g.pid" ]; do sleep 0.05; done
: > "$DIR/ready"
wait
"#;
const A: &str = "#!/bin/sh\necho $$ > \"$DIR/a.pid\"\nexec sleep 60\n";
const B: &str = "#!/bin/sh\necho $$ > \"$DIR/b.pid\"\n\"$DIR/g.sh\"\n";
const G: &str = "#!/bin/sh\necho $$ > \"$DIR/g.pid\"\nexec sleep 60\n";

fn timeout_binary() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH");
    ["gtimeout", "timeout"]
        .iter()
        .flat_map(|name| std::env::split_paths(&path).map(move |dir| dir.join(name)))
        .find(|candidate| candidate.is_file())
        .expect("gtimeout or timeout on PATH")
}

/// Fresh tree directory holding the four scripts.
fn tree_dir(scratch: &Path, name: &str) -> PathBuf {
    let dir = scratch.join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tree dir");
    for (file, body) in [("root.sh", ROOT), ("a.sh", A), ("b.sh", B), ("g.sh", G)] {
        let path = dir.join(file);
        std::fs::write(&path, body).expect("script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    dir
}

fn run_pinned(node: &std::ffi::OsString, dist: &Path, dir: &Path, mode: &str) -> String {
    let output = Command::new(timeout_binary())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(["--kill-after=5", "60"])
        .arg(node)
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .arg(dist)
        .arg(dir)
        .arg(GRACEFUL_MS.to_string())
        .arg(FORCE_MS.to_string())
        .arg(mode)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8")
}

fn run_rust(dir: &Path, mode: &str, scratch: &Path) -> String {
    std::fs::write(
        scratch.join(CHILD_SCENARIO),
        format!("{}\n{mode}\n", dir.display()),
    )
    .expect("child scenario");
    let _ = std::fs::remove_file(scratch.join(CHILD_OUT));
    let output = Command::new(timeout_binary())
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(["--kill-after=5", "60"])
        .arg(std::env::current_exe().expect("test exe"))
        .args([
            "--exact",
            "child_terminates_the_tree",
            "--nocapture",
            "--test-threads=1",
        ])
        .current_dir(scratch)
        .output()
        .expect("run the child");
    assert!(
        output.status.success(),
        "child failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    std::fs::read_to_string(scratch.join(CHILD_OUT)).unwrap_or_default()
}

fn read_pid(dir: &Path, role: &str) -> u32 {
    std::fs::read_to_string(dir.join(format!("{role}.pid")))
        .expect("pid file")
        .trim()
        .parse()
        .expect("pid")
}

/// The child half: builds the tree named in its working directory's scenario
/// file and stops it. Does nothing outside the differential.
#[test]
fn child_terminates_the_tree() {
    let Ok(text) = std::fs::read_to_string(CHILD_SCENARIO) else {
        return;
    };
    let mut parts = text.lines();
    let dir = PathBuf::from(parts.next().expect("dir"));
    let mode = parts.next().expect("mode").to_owned();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let local = tokio::task::LocalSet::new();
    let lines = local.block_on(&runtime, async move {
        let mut env = JsObject::new();
        env.insert("DIR", JsValue::String(dir.to_string_lossy().into_owned()));
        env.insert(
            "IGNORE",
            JsValue::String(if mode == "ignore" { "1" } else { "" }.to_owned()),
        );
        env.insert("PATH", JsValue::String("/usr/bin:/bin".to_owned()));
        let child: Rc<ChildProcess> = ChildProcess::spawn(
            &SpawnRequest {
                command: "/bin/sh".to_owned(),
                args: vec![dir.join("root.sh").to_string_lossy().into_owned()],
                cwd: None,
                env,
            },
            None,
        )
        .expect("spawn the tree");
        let start = Instant::now();
        while !dir.join("ready").exists() && start.elapsed() < Duration::from_secs(5) {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let pids: Vec<(u32, &str)> = ROLES
            .iter()
            .map(|role| (read_pid(&dir, role), *role))
            .collect();
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = Arc::clone(&log);
        observe_signals(Some(Rc::new(move |pid, signal| {
            let role = pids
                .iter()
                .find(|(known, _)| *known == pid)
                .map_or("other", |(_, role)| *role);
            sink.lock()
                .expect("log")
                .push(format!("kill {role} {signal}"));
        })));
        if mode == "exited" {
            child.kill("SIGKILL");
            log.lock().expect("log").clear();
            child.wait_exit().await;
        }
        let result = terminate_with_tree_kill(
            &child,
            Duration::from_millis(GRACEFUL_MS),
            Duration::from_millis(FORCE_MS),
        )
        .await;
        observe_signals(None);
        let mut lines = log.lock().expect("log").clone();
        lines.push(format!(
            "result {}",
            match result {
                TerminateResult::AlreadyExited => "already-exited",
                TerminateResult::Terminated => "terminated",
                TerminateResult::Killed => "killed",
                TerminateResult::KillTimeout => "kill-timeout",
            }
        ));
        // `child.killed` is true only after `child.kill`, which the exited
        // scenario calls itself.
        lines.push(format!("killed {}", child.killed()));
        lines
    });
    std::fs::write(CHILD_OUT, lines.join("\n") + "\n").expect("write");
}

/// Stops what a scenario left running: only the pids it recorded, and only
/// while they still run this tree's scripts or its `sleep 60`.
fn reap(dir: &Path) {
    for role in ROLES {
        let Ok(text) = std::fs::read_to_string(dir.join(format!("{role}.pid"))) else {
            continue;
        };
        let pid = text.trim();
        let command = Command::new("/bin/ps")
            .args(["-o", "command=", "-p", pid])
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .unwrap_or_default();
        if command.contains("sleep 60") || command.contains(&*dir.to_string_lossy()) {
            let _ = Command::new("/bin/kill").args(["-s", "KILL", pid]).status();
        }
    }
}

#[test]
fn trees_stop_as_the_pinned_helper_stops_them() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let scratch = std::fs::canonicalize(std::env::temp_dir())
        .expect("temp")
        .join(format!("spocky-tree-kill-diff-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("scratch");
    let mut failures = Vec::new();
    for mode in ["graceful", "ignore", "exited"] {
        let pinned_dir = tree_dir(&scratch, "pinned");
        let rust_dir = tree_dir(&scratch, "rust");
        let expected = run_pinned(&node, &dist, &pinned_dir, mode);
        let actual = run_rust(&rust_dir, mode, &scratch);
        reap(&pinned_dir);
        reap(&rust_dir);
        if expected != actual {
            failures.push(format!("{mode}\n--- node\n{expected}--- rust\n{actual}"));
        }
        assert!(
            expected.contains("result "),
            "{mode}: the pinned run printed {expected}"
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        failures.is_empty(),
        "{} scenarios differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
