//! The errno table of `process.rs` must be the one the pinned Node reports:
//! for every errno from 1 to 1000 the name in `util.getSystemErrorMap()`
//! (negated number) is `errno_name`'s answer, and a number the map lacks has
//! none. Runs on the host it is built for: macOS locally, Linux in CI
//! (`.github/workflows` on branch `ci/p4_provider_claude/errno`). Needs only
//! `SPOCKY_PINNED_NODE`. A second test spawns targets that cannot start and
//! compares how Node reports each (error event or throw) and its message.

use std::collections::BTreeMap;
use std::process::Command;

use spocky_contracts::js_value::{JsObject, JsValue};
use spocky_provider_claude::process::{ChildProcess, SpawnFailureKind, SpawnRequest, errno_name};

const NODE_SCRIPT: &str = r#"
const map = require("util").getSystemErrorMap();
for (let errno = 1; errno <= 1000; errno++) {
  const entry = map.get(-errno);
  if (entry) console.log(`${errno} ${entry[0]}`);
}
"#;

#[test]
fn the_errno_table_is_the_pinned_nodes() {
    let Some(node) = std::env::var_os("SPOCKY_PINNED_NODE") else {
        assert_eq!(
            std::env::var("SPOCKY_ALLOW_SKIP").as_deref(),
            Ok("1"),
            "set SPOCKY_PINNED_NODE (or SPOCKY_ALLOW_SKIP=1)"
        );
        return;
    };
    let output = Command::new(node)
        .args(["-e", NODE_SCRIPT])
        .env_clear()
        .output()
        .expect("run the pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: BTreeMap<i32, String> = String::from_utf8(output.stdout)
        .expect("utf8")
        .lines()
        .map(|line| {
            let (number, name) = line.split_once(' ').expect("number and name");
            (number.parse().expect("number"), name.to_owned())
        })
        .collect();
    let actual: BTreeMap<i32, String> = (1..=1000)
        .filter_map(|number| errno_name(number).map(|name| (number, name.to_owned())))
        .collect();
    assert!(!expected.is_empty());
    assert_eq!(
        actual, expected,
        "regenerate with scripts/phase4/provider-claude-errno-table.sh"
    );
}

/// `(command, cwd)` spawn attempts that fail; `<dir>` is a scratch directory
/// holding `plain` (mode 0644), `sub` (a directory) and `file`.
const SPAWN_CASES: [(&str, Option<&str>); 7] = [
    ("<dir>/missing", None),
    ("<dir>/plain", None),
    ("<dir>/sub", None),
    ("no-such-command-xyz", None),
    ("/bin/echo", Some("<dir>/missing")),
    ("/bin/echo", Some("<dir>/file")),
    ("/bin/echo", Some("<dir>/sub-without-search")),
];

const SPAWN_SCRIPT: &str = r#"
const { spawn } = require("child_process");
const dir = process.argv[1];
const cases = JSON.parse(process.argv[2]);
(async () => {
  for (const [command, cwd] of cases) {
    const resolve = (text) => text.replace("<dir>", dir);
    const result = await new Promise((done) => {
      try {
        const child = spawn(resolve(command), [], { cwd: cwd && resolve(cwd), env: { PATH: "/usr/bin:/bin" }, stdio: "pipe" });
        child.on("error", (error) => done(`event ${error.message}`));
        setTimeout(() => { child.kill("SIGKILL"); done("spawned"); }, 300);
      } catch (error) {
        done(`throw ${error.message}`);
      }
    });
    console.log(result.split(dir).join("<dir>"));
  }
})();
"#;

#[test]
fn spawn_failures_are_reported_as_the_pinned_node_reports_them() {
    use std::os::unix::fs::PermissionsExt;
    let Some(node) = std::env::var_os("SPOCKY_PINNED_NODE") else {
        assert_eq!(
            std::env::var("SPOCKY_ALLOW_SKIP").as_deref(),
            Ok("1"),
            "set SPOCKY_PINNED_NODE (or SPOCKY_ALLOW_SKIP=1)"
        );
        return;
    };
    let dir = std::fs::canonicalize(std::env::temp_dir())
        .expect("temp")
        .join(format!("spocky-errno-diff-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).expect("dir");
    std::fs::create_dir_all(dir.join("sub-without-search")).expect("dir");
    std::fs::write(dir.join("plain"), "").expect("plain");
    std::fs::write(dir.join("file"), "").expect("file");
    std::fs::set_permissions(dir.join("plain"), std::fs::Permissions::from_mode(0o644))
        .expect("chmod");
    std::fs::set_permissions(
        dir.join("sub-without-search"),
        std::fs::Permissions::from_mode(0o000),
    )
    .expect("chmod");
    let cases = format!(
        "[{}]",
        SPAWN_CASES
            .iter()
            .map(|(command, cwd)| format!(
                "[\"{command}\",{}]",
                cwd.map_or_else(|| "null".to_owned(), |cwd| format!("\"{cwd}\""))
            ))
            .collect::<Vec<_>>()
            .join(",")
    );
    let output = Command::new(node)
        .args(["-e", SPAWN_SCRIPT])
        .arg(&dir)
        .arg(&cases)
        .env_clear()
        .output()
        .expect("run the pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = String::from_utf8(output.stdout).expect("utf8");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let local = tokio::task::LocalSet::new();
    let actual = local.block_on(&runtime, async {
        let mut lines = String::new();
        for (command, cwd) in SPAWN_CASES {
            let resolve = |text: &str| text.replace("<dir>", &dir.to_string_lossy());
            let mut env = JsObject::new();
            env.insert("PATH", JsValue::String("/usr/bin:/bin".to_owned()));
            let spawned = ChildProcess::spawn(
                &SpawnRequest {
                    command: resolve(command),
                    args: Vec::new(),
                    cwd: cwd.map(resolve),
                    env,
                },
                None,
            );
            let line = match spawned {
                Ok(child) => {
                    child.kill("SIGKILL");
                    "spawned".to_owned()
                }
                Err(failure) => {
                    let how = match failure.kind {
                        SpawnFailureKind::Thrown => "throw",
                        SpawnFailureKind::Event | SpawnFailureKind::EventWithoutStdio => "event",
                    };
                    format!("{how} {}", failure.message)
                }
            };
            lines.push_str(&line.replace(&*dir.to_string_lossy(), "<dir>"));
            lines.push('\n');
        }
        lines
    });
    let _ = std::fs::set_permissions(
        dir.join("sub-without-search"),
        std::fs::Permissions::from_mode(0o755),
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(actual, expected);
}
