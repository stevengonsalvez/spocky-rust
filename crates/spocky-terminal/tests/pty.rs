//! PTY lifecycle differential: the same scripted `/bin/sh` sessions run
//! through the pinned node-pty (`pty.spawn` with fixed TERM name, size, env,
//! and cwd) and through [`Pty`], and the full decoded output and the exit
//! event must match byte for byte. Chunk boundaries are not compared: the
//! kernel decides them for both.
//!
//! Scenarios cover termios (`stty -a`), environment order and the `PWD` and
//! `TERM` assignment, UTF-8 and a large burst before exit, self-signal exit,
//! input echo, resize, `^C` through the controlling terminal, default and
//! explicit kill, a missing cwd, and a missing command. Every wait is bounded.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_terminal::pty::{Pty, PtyEvent, PtySpawnOptions, RESIZE_ERROR};

const SCENARIO_DEADLINE: Duration = Duration::from_secs(20);

/// A kill step: node-pty's default signal, or `SIGTERM` by name.
#[derive(Clone, Copy)]
enum Kill {
    Default,
    Term,
}

struct Step {
    wait_for: &'static str,
    write: Option<&'static str>,
    resize: Option<(u16, u16)>,
    kill: Option<Kill>,
}

const fn write(wait_for: &'static str, data: &'static str) -> Step {
    Step {
        wait_for,
        write: Some(data),
        resize: None,
        kill: None,
    }
}

struct Scenario {
    name: &'static str,
    file: &'static str,
    args: &'static [&'static str],
    missing_cwd: bool,
    extra_env: &'static [(&'static str, &'static str)],
    steps: Vec<Step>,
}

fn scenario(name: &'static str, file: &'static str, args: &'static [&'static str]) -> Scenario {
    Scenario {
        name,
        file,
        args,
        missing_cwd: false,
        extra_env: &[],
        steps: Vec::new(),
    }
}

fn scenarios() -> Vec<Scenario> {
    vec![
        scenario("termios", "/bin/sh", &["-c", "stty -a; stty size; exit 3"]),
        Scenario {
            extra_env: &[("ZED", "1"), ("TERM", "dumb"), ("ALPHA", "2")],
            ..scenario("env-order", "/usr/bin/env", &[])
        },
        // The burst has no newline: when the PTY output queue fills, the
        // macOS line discipline can emit ONLCR's `\r` twice, which depends
        // on how fast the reader drains, not on the implementation.
        scenario(
            "burst",
            "/bin/sh",
            &[
                "-c",
                "printf '\\033[31m\\344\\270\\255\\n\\360\\237\\230\\200'; head -c 40000 /dev/zero | tr '\\000' x; printf done",
            ],
        ),
        scenario("self-signal", "/bin/sh", &["-c", "kill -TERM $$"]),
        Scenario {
            steps: vec![write("ready", "h\u{e9}llo\r")],
            ..scenario(
                "input-echo",
                "/bin/sh",
                &["-c", "printf ready; read line; printf 'got:%s' \"$line\""],
            )
        },
        Scenario {
            steps: vec![
                Step {
                    resize: Some((100, 30)),
                    ..write("ready", "")
                },
                write("ready", "\r"),
            ],
            ..scenario(
                "resize",
                "/bin/sh",
                &["-c", "printf ready; read x; stty size"],
            )
        },
        Scenario {
            steps: vec![write("ready", "\u{3}")],
            ..scenario(
                "interrupt",
                "/bin/sh",
                &["-c", "printf ready; read x; echo after"],
            )
        },
        Scenario {
            steps: vec![Step {
                kill: Some(Kill::Default),
                ..write("ready", "")
            }],
            ..scenario("kill-default", "/bin/sh", &["-c", "printf ready; read x"])
        },
        Scenario {
            steps: vec![Step {
                kill: Some(Kill::Term),
                ..write("ready", "")
            }],
            ..scenario("kill-term", "/bin/sh", &["-c", "printf ready; read x"])
        },
        Scenario {
            missing_cwd: true,
            ..scenario("missing-cwd", "/bin/sh", &["-c", "echo hi"])
        },
        scenario(
            "missing-command",
            "spocky-missing-command-for-pty-test",
            &[],
        ),
    ]
}

const NODE_SCRIPT: &str = r#"
import { createRequire } from "node:module";
const [terminalDir, scenarioJson] = process.argv.slice(1);
const require = createRequire(`${terminalDir}/terminal.js`);
const pty = require("node-pty");
const s = JSON.parse(scenarioJson);
const p = pty.spawn(s.file, s.args, {
  name: "xterm-256color",
  cols: 80,
  rows: 24,
  cwd: s.cwd,
  env: Object.fromEntries(s.env),
});
let out = "";
let step = 0;
p.onData((data) => {
  out += data;
  while (step < s.steps.length && out.includes(s.steps[step].waitFor)) {
    const current = s.steps[step++];
    if (current.resize) p.resize(current.resize[0], current.resize[1]);
    if (current.write) p.write(current.write);
    if (current.kill !== undefined) p.kill(current.kill === null ? undefined : current.kill);
  }
});
p.onExit(({ exitCode, signal }) => {
  process.stdout.write(JSON.stringify({ out, exitCode, signal }));
  process.exit(0);
});
setTimeout(() => {
  process.stdout.write(JSON.stringify({ out, timeout: true }));
  process.exit(0);
}, 20000).unref();
"#;

fn base_env(cwd: &Path) -> Vec<(String, String)> {
    vec![
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ("HOME".to_owned(), cwd.to_string_lossy().into_owned()),
        ("LANG".to_owned(), "en_US.UTF-8".to_owned()),
    ]
}

fn env_for(scenario: &Scenario, cwd: &Path) -> Vec<(String, String)> {
    let mut env = base_env(cwd);
    env.extend(
        scenario
            .extra_env
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
    );
    env
}

fn scenario_json(scenario: &Scenario, cwd: &Path) -> String {
    let mut object = JsObject::new();
    object.insert("file", JsValue::String(scenario.file.to_owned()));
    object.insert(
        "args",
        JsValue::Array(
            scenario
                .args
                .iter()
                .map(|a| JsValue::String((*a).to_owned()))
                .collect(),
        ),
    );
    object.insert("cwd", JsValue::String(cwd.to_string_lossy().into_owned()));
    object.insert(
        "env",
        JsValue::Array(
            env_for(scenario, cwd)
                .into_iter()
                .map(|(k, v)| JsValue::Array(vec![JsValue::String(k), JsValue::String(v)]))
                .collect(),
        ),
    );
    object.insert(
        "steps",
        JsValue::Array(
            scenario
                .steps
                .iter()
                .map(|step| {
                    let mut entry = JsObject::new();
                    entry.insert("waitFor", JsValue::String(step.wait_for.to_owned()));
                    if let Some(data) = step.write.filter(|data| !data.is_empty()) {
                        entry.insert("write", JsValue::String(data.to_owned()));
                    }
                    if let Some((cols, rows)) = step.resize {
                        entry.insert(
                            "resize",
                            JsValue::Array(vec![
                                JsValue::Number(f64::from(cols)),
                                JsValue::Number(f64::from(rows)),
                            ]),
                        );
                    }
                    if let Some(kill) = step.kill {
                        entry.insert(
                            "kill",
                            match kill {
                                Kill::Default => JsValue::Null,
                                Kill::Term => JsValue::String("SIGTERM".to_owned()),
                            },
                        );
                    }
                    JsValue::Object(entry)
                })
                .collect(),
        ),
    );
    stringify(&JsValue::Object(object))
}

fn run_rust(scenario: &Scenario, cwd: &Path) -> String {
    let options = PtySpawnOptions {
        file: scenario.file.to_owned(),
        args: scenario.args.iter().map(|a| (*a).to_owned()).collect(),
        cwd: cwd.to_path_buf(),
        env: env_for(scenario, cwd),
        name: "xterm-256color".to_owned(),
        cols: 80,
        rows: 24,
        helper: PathBuf::from(env!("CARGO_BIN_EXE_spocky-pty-helper")),
    };
    let (pty, events) = Pty::spawn(&options).expect("spawn");
    let deadline = Instant::now() + SCENARIO_DEADLINE;
    let mut out = String::new();
    let mut step = 0;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Ok(event) = events.recv_timeout(remaining) else {
            // Bounded: stop the child we started, then report the timeout.
            pty.kill(Some(rustix::process::Signal::KILL));
            let mut object = JsObject::new();
            object.insert("out", JsValue::String(out));
            object.insert("timeout", JsValue::Bool(true));
            return stringify(&JsValue::Object(object));
        };
        match event {
            PtyEvent::Data(data) => {
                out.push_str(&data);
                while step < scenario.steps.len() && out.contains(scenario.steps[step].wait_for) {
                    let current = &scenario.steps[step];
                    step += 1;
                    if let Some((cols, rows)) = current.resize {
                        pty.resize(cols, rows).expect("resize");
                    }
                    if let Some(data) = current.write.filter(|data| !data.is_empty()) {
                        pty.write(data);
                    }
                    if let Some(kill) = current.kill {
                        pty.kill(match kill {
                            Kill::Default => None,
                            Kill::Term => Some(rustix::process::Signal::TERM),
                        });
                    }
                }
            }
            PtyEvent::Exit(exit) => {
                let mut object = JsObject::new();
                object.insert("out", JsValue::String(out));
                object.insert("exitCode", JsValue::Number(f64::from(exit.exit_code)));
                object.insert("signal", JsValue::Number(f64::from(exit.signal)));
                return stringify(&JsValue::Object(object));
            }
        }
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "spocky-terminal-pty-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        Self(path.canonicalize().expect("canonical temp dir"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn resize_rejects_zero_dimensions_with_the_node_pty_error() {
    let dir = TempDir::new();
    let (pty, events) = Pty::spawn(&PtySpawnOptions {
        file: "/bin/sh".to_owned(),
        args: vec!["-c".to_owned(), "exit 0".to_owned()],
        cwd: dir.0.clone(),
        env: base_env(&dir.0),
        name: "xterm-256color".to_owned(),
        cols: 80,
        rows: 24,
        helper: PathBuf::from(env!("CARGO_BIN_EXE_spocky-pty-helper")),
    })
    .expect("spawn");
    assert_eq!(
        pty.resize(0, 24).expect_err("zero cols").to_string(),
        RESIZE_ERROR
    );
    let deadline = Instant::now() + SCENARIO_DEADLINE;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match events.recv_timeout(remaining).expect("bounded exit") {
            PtyEvent::Exit(exit) => {
                assert_eq!((exit.exit_code, exit.signal), (0, 0));
                break;
            }
            PtyEvent::Data(_) => {}
        }
    }
}

/// Where two captures first differ, with context, and both lengths.
fn first_difference(expected: &str, actual: &str) -> String {
    let expected: Vec<char> = expected.chars().collect();
    let actual: Vec<char> = actual.chars().collect();
    let at = expected
        .iter()
        .zip(&actual)
        .position(|(a, b)| a != b)
        .unwrap_or(expected.len().min(actual.len()));
    let window = |text: &[char]| -> String {
        text[at.saturating_sub(40)..(at + 80).min(text.len())]
            .iter()
            .collect()
    };
    format!(
        "differs at char {at} (node {} chars, rust {} chars)\n  node: {:?}\n  rust: {:?}",
        expected.len(),
        actual.len(),
        window(&expected),
        window(&actual)
    )
}

#[test]
fn pty_sessions_match_pinned_node_pty() {
    let Some(pinned) = support::pinned("pty differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let mut failures = Vec::new();
    for scenario in scenarios() {
        let dir = TempDir::new();
        let cwd = if scenario.missing_cwd {
            dir.0.join("missing")
        } else {
            dir.0.clone()
        };
        let input = scenario_json(&scenario, &cwd);
        let expected = support::run_node(&pinned, NODE_SCRIPT, &[&input]);
        let actual = run_rust(&scenario, &cwd);
        let parsed = parse(&expected).expect("node json");
        assert!(
            parsed.get("timeout").is_none(),
            "{}: node timed out",
            scenario.name
        );
        if actual != expected {
            failures.push(format!(
                "{}: {}",
                scenario.name,
                first_difference(&expected, &actual)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
