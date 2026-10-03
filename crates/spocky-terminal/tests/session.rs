//! Terminal sessions against the pinned `createTerminal`: the same scripted
//! shell scenarios run through `terminal.js` on the pinned Node and through
//! [`TerminalSession`]. The first subscriber message type, the full output
//! text, title events, OSC 633 events, the states captured at each step (cell
//! grid, scrollback, wrap flags, cursor, title), and the exit info must match
//! as text. Chunk boundaries and revisions depend on the kernel and are not
//! compared; revisions are checked for order on the Rust side.
//!
//! Each script starts with a short sleep so its output arrives after the
//! subscriber attached, as in a live terminal, and ends with a short sleep
//! after its last output: the baseline can handle the PTY exit before the
//! parse of the output just before it (the port always parses first), so a
//! script that exits at once would compare a Node race. Every wait is bounded and only recorded children are signalled.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_terminal::restore::{SnapshotMode, SnapshotOptions};
use spocky_terminal::session::{ClientMessage, ServerMessage, SessionOptions, TerminalSession};
use spocky_wire::encode_terminal_snapshot;

const DEADLINE: Duration = Duration::from_secs(25);

/// One step: wait until the output holds `wait_for`, then act.
struct Step {
    wait_for: &'static str,
    input: Option<&'static str>,
    resize: Option<(u16, u16)>,
    title: Option<&'static str>,
    state: bool,
    kill: Option<&'static str>,
}

const fn step(wait_for: &'static str) -> Step {
    Step {
        wait_for,
        input: None,
        resize: None,
        title: None,
        state: false,
        kill: None,
    }
}

struct Scenario {
    name: &'static str,
    args: &'static [&'static str],
    title: Option<&'static str>,
    steps: Vec<Step>,
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "titles-and-osc633",
            args: &[
                "-c",
                "sleep 0.5; printf 'hello\\r\\n'; printf '\\033]0;mytitle\\007x\\033]633;D;7\\007y\\033]633;D\\007z'; read l; printf 'got:%s\\n' \"$l\"; sleep 0.5",
            ],
            title: None,
            steps: vec![
                Step {
                    state: true,
                    ..step("z")
                },
                Step {
                    input: Some("line\r"),
                    ..step("z")
                },
            ],
        },
        Scenario {
            name: "manual-title",
            args: &[
                "-c",
                "sleep 0.5; printf 'a\\033]0;ignored\\007b'; read l; printf done; sleep 0.5",
            ],
            title: Some("preset"),
            steps: vec![
                Step {
                    title: Some("  renamed  "),
                    state: true,
                    ..step("b")
                },
                Step {
                    input: Some("\r"),
                    ..step("b")
                },
            ],
        },
        Scenario {
            name: "scrollback-resize",
            args: &[
                "-c",
                "sleep 0.5; i=0; while [ $i -lt 40 ]; do printf '\\033[3%dmline %d some wrapping text beyond thirty columns\\033[0m\\r\\n' $((i % 8)) $i; i=$((i+1)); done; printf 'end'; read l; printf 'after'; sleep 0.5",
            ],
            title: None,
            steps: vec![
                Step {
                    state: true,
                    ..step("end")
                },
                Step {
                    resize: Some((12, 8)),
                    state: true,
                    ..step("end")
                },
                Step {
                    resize: Some((40, 3)),
                    input: Some("\r"),
                    state: true,
                    ..step("end")
                },
            ],
        },
        Scenario {
            name: "protocol-replies",
            args: &[
                "-c",
                "sleep 0.5; stty raw -echo; printf '\\033[c\\033[5n\\033[6n\\033[?6n\\033]11;?\\033\\\\'; dd bs=1 count=53 2>/dev/null | od -An -c; printf 'END'; sleep 0.5",
            ],
            title: None,
            steps: vec![Step {
                state: true,
                ..step("END")
            }],
        },
        Scenario {
            name: "kill",
            args: &["-c", "sleep 0.5; printf 'ready'; read l"],
            title: None,
            steps: vec![Step {
                kill: Some("kill"),
                ..step("ready")
            }],
        },
        Scenario {
            name: "kill-and-wait",
            args: &["-c", "sleep 0.5; printf 'ready'; trap '' HUP; read l"],
            title: None,
            steps: vec![Step {
                kill: Some("killAndWait"),
                ..step("ready")
            }],
        },
    ]
}

const NODE_SCRIPT: &str = r#"
const [terminalDir, scenarioJson] = process.argv.slice(1);
const { createTerminal } = await import(`${terminalDir}/terminal.js`);
const s = JSON.parse(scenarioJson);
const session = await createTerminal({
  cwd: s.cwd,
  workspaceId: "ws",
  command: "/bin/sh",
  args: s.args,
  rows: 5,
  cols: 30,
  ...(s.title === null ? {} : { title: s.title }),
});
const result = { first: null, out: "", titles: [], commandFinished: [], states: [], exit: null };
let step = 0;
const act = (current) => {
  if (current.resize) session.send({ type: "resize", rows: current.resize[1], cols: current.resize[0] });
  if (current.title !== null) session.setTitle(current.title);
  if (current.state) result.states.push(session.getState({ includeWrapFlags: true }));
  if (current.input !== null) session.send({ type: "input", data: current.input });
  if (current.kill === "kill") session.kill();
  if (current.kill === "killAndWait") void session.killAndWait({ gracefulTimeoutMs: 600, forceTimeoutMs: 2000 });
};
let scheduled = false;
const schedule = () => {
  if (scheduled || step >= s.steps.length || !result.out.includes(s.steps[step].waitFor)) return;
  scheduled = true;
  const current = s.steps[step++];
  setTimeout(() => {
    scheduled = false;
    act(current);
    schedule();
  }, 400);
};
session.subscribe((message) => {
  if (result.first === null) result.first = message.type;
  if (message.type !== "output") return;
  result.out += message.data;
  schedule();
}, { initialSnapshot: "state" });
session.onTitleChange((title) => result.titles.push(title ?? null));
session.onCommandFinished((info) => result.commandFinished.push(info.exitCode));
const finish = () => setTimeout(() => {
  process.stdout.write(JSON.stringify(result), () => process.exit(0));
}, 900);
session.onExit((info) => { result.exit = info; finish(); });
setTimeout(() => {
  result.timeout = true;
  process.stdout.write(JSON.stringify(result), () => process.exit(0));
}, 24000).unref();
"#;

fn base_env(home: &Path) -> Vec<(&'static str, String)> {
    vec![
        ("PATH", "/usr/bin:/bin".to_owned()),
        ("HOME", home.to_string_lossy().into_owned()),
        ("LANG", "en_US.UTF-8".to_owned()),
    ]
}

fn scenario_json(scenario: &Scenario, cwd: &Path) -> String {
    let mut object = JsObject::new();
    object.insert("cwd", JsValue::String(cwd.to_string_lossy().into_owned()));
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
    object.insert(
        "title",
        scenario
            .title
            .map_or(JsValue::Null, |t| JsValue::String(t.to_owned())),
    );
    let optional = |value: Option<&str>| {
        value.map_or(JsValue::Null, |value| JsValue::String(value.to_owned()))
    };
    object.insert(
        "steps",
        JsValue::Array(
            scenario
                .steps
                .iter()
                .map(|step| {
                    let mut entry = JsObject::new();
                    entry.insert("waitFor", JsValue::String(step.wait_for.to_owned()));
                    entry.insert("input", optional(step.input));
                    entry.insert("title", optional(step.title));
                    entry.insert("kill", optional(step.kill));
                    entry.insert("state", JsValue::Bool(step.state));
                    if let Some((cols, rows)) = step.resize {
                        entry.insert(
                            "resize",
                            JsValue::Array(vec![
                                JsValue::Number(f64::from(cols)),
                                JsValue::Number(f64::from(rows)),
                            ]),
                        );
                    }
                    JsValue::Object(entry)
                })
                .collect(),
        ),
    );
    stringify(&JsValue::Object(object))
}

#[derive(Default)]
struct Collected {
    first: Option<&'static str>,
    out: String,
    revisions: Vec<u64>,
    titles: Vec<Option<String>>,
    command_finished: Vec<Option<f64>>,
    exit: Option<spocky_terminal::session::ExitInfo>,
}

/// One step's actions, in the order the pinned script runs them.
fn act(session: &TerminalSession, current: &Step, states: &mut Vec<JsValue>) {
    if let Some((cols, rows)) = current.resize {
        session.send(ClientMessage::Resize { rows, cols });
    }
    if let Some(title) = current.title {
        session.set_title(title);
    }
    if current.state {
        let state = session
            .state(SnapshotOptions {
                scrollback_lines: None,
                include_wrap_flags: true,
            })
            .expect("state");
        states.push(
            parse(
                &String::from_utf8(encode_terminal_snapshot(&state).expect("json")).expect("utf8"),
            )
            .expect("state json"),
        );
    }
    if let Some(input) = current.input {
        session.send(ClientMessage::Input(input.to_owned()));
    }
    match current.kill {
        Some("kill") => session.kill(),
        Some(_) => {
            let handle = session.clone();
            std::thread::spawn(move || {
                handle.kill_and_wait(Duration::from_millis(600), Duration::from_millis(2000));
            });
        }
        None => {}
    }
}

fn create_session(scenario: &Scenario, cwd: &Path) -> TerminalSession {
    let mut process_env = JsObject::new();
    for (key, value) in base_env(cwd) {
        process_env.insert(key, JsValue::String(value));
    }
    TerminalSession::create(SessionOptions {
        id: None,
        cwd: cwd.to_path_buf(),
        workspace_id: "ws".to_owned(),
        shell: None,
        env: JsObject::new(),
        activity_env: JsObject::new(),
        rows: Some(5),
        cols: Some(30),
        name: None,
        title: scenario.title.map(str::to_owned),
        command: Some("/bin/sh".to_owned()),
        args: scenario.args.iter().map(|a| (*a).to_owned()).collect(),
        process_env,
        paseo_cli_bin_dir: None,
        paseo_hook_cli_path: None,
        zsh_integration_dir: None,
        tmpdir: std::env::temp_dir(),
        username: "spocky".to_owned(),
        pid: std::process::id(),
        process_cwd: cwd.to_string_lossy().into_owned(),
        helper: PathBuf::from(env!("CARGO_BIN_EXE_spocky-pty-helper")),
    })
    .expect("create session")
}

fn run_rust(scenario: &Scenario, cwd: &Path) -> String {
    let session = create_session(scenario, cwd);
    let collected = Arc::new(Mutex::new(Collected::default()));
    let sink = Arc::clone(&collected);
    session.subscribe(
        move |message| {
            let mut collected = sink.lock().expect("lock");
            let kind = match &message {
                ServerMessage::Output { .. } => "output",
                ServerMessage::Snapshot { .. } => "snapshot",
                ServerMessage::SnapshotReady { .. } => "snapshotReady",
                ServerMessage::TitleChange { .. } => "titleChange",
            };
            collected.first.get_or_insert(kind);
            if let ServerMessage::Output { data, revision } = message {
                collected.out.push_str(&data);
                collected.revisions.push(revision);
            }
        },
        SnapshotMode::State,
    );
    let sink = Arc::clone(&collected);
    session.on_title_change(move |title| {
        sink.lock()
            .expect("lock")
            .titles
            .push(title.map(str::to_owned));
    });
    let sink = Arc::clone(&collected);
    session.on_command_finished(move |code| sink.lock().expect("lock").command_finished.push(code));
    let sink = Arc::clone(&collected);
    session.on_exit(move |info| sink.lock().expect("lock").exit = Some(info));

    let mut states = Vec::new();
    let deadline = Instant::now() + DEADLINE;
    let mut step = 0;
    let mut acted_at: Option<Instant> = None;
    let mut exited_at: Option<Instant> = None;
    loop {
        std::thread::sleep(Duration::from_millis(5));
        let now = Instant::now();
        if now > deadline {
            session.kill_and_wait(Duration::from_millis(200), Duration::from_millis(500));
            return stringify(&JsValue::String("timeout".to_owned()));
        }
        let seen = {
            let collected = collected.lock().expect("lock");
            if collected.exit.is_some() && exited_at.is_none() {
                exited_at = Some(now);
            }
            step < scenario.steps.len() && collected.out.contains(scenario.steps[step].wait_for)
        };
        if exited_at.is_some_and(|at| now.duration_since(at) > Duration::from_millis(900)) {
            break;
        }
        // The pinned script acts 400 ms after the marker is seen.
        if seen && acted_at.is_none() {
            acted_at = Some(now);
        }
        if let Some(at) = acted_at
            && now.duration_since(at) >= Duration::from_millis(400)
        {
            acted_at = None;
            act(&session, &scenario.steps[step], &mut states);
            step += 1;
        }
    }

    let collected = collected.lock().expect("lock");
    result_json(&collected, states)
}

fn result_json(collected: &Collected, states: Vec<JsValue>) -> String {
    assert!(
        collected.revisions.windows(2).all(|pair| pair[0] < pair[1]),
        "output revisions must increase: {:?}",
        collected.revisions
    );
    let mut result = JsObject::new();
    result.insert(
        "first",
        collected
            .first
            .map_or(JsValue::Null, |kind| JsValue::String(kind.to_owned())),
    );
    result.insert("out", JsValue::String(collected.out.clone()));
    result.insert(
        "titles",
        JsValue::Array(
            collected
                .titles
                .iter()
                .map(|t| t.clone().map_or(JsValue::Null, JsValue::String))
                .collect(),
        ),
    );
    result.insert(
        "commandFinished",
        JsValue::Array(
            collected
                .command_finished
                .iter()
                .map(|c| c.map_or(JsValue::Null, JsValue::Number))
                .collect(),
        ),
    );
    result.insert("states", JsValue::Array(states));
    let exit = collected.exit.as_ref().expect("exit");
    let mut info = JsObject::new();
    info.insert(
        "exitCode",
        exit.exit_code
            .map_or(JsValue::Null, |c| JsValue::Number(f64::from(c))),
    );
    info.insert(
        "signal",
        exit.signal
            .map_or(JsValue::Null, |c| JsValue::Number(f64::from(c))),
    );
    info.insert(
        "lastOutputLines",
        JsValue::Array(
            exit.last_output_lines
                .iter()
                .cloned()
                .map(JsValue::String)
                .collect(),
        ),
    );
    result.insert("exit", JsValue::Object(info));
    stringify(&JsValue::Object(result))
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "spocky-terminal-session-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        Self(path.canonicalize().expect("canonical"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn first_difference(expected: &str, actual: &str) -> String {
    let expected: Vec<char> = expected.chars().collect();
    let actual: Vec<char> = actual.chars().collect();
    let at = expected
        .iter()
        .zip(&actual)
        .position(|(a, b)| a != b)
        .unwrap_or(expected.len().min(actual.len()));
    let window = |text: &[char]| -> String {
        text[at.saturating_sub(60)..(at + 120).min(text.len())]
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
fn sessions_match_pinned_create_terminal() {
    let Some(pinned) = support::pinned("session differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let mut failures = Vec::new();
    for scenario in scenarios() {
        let dir = TempDir::new();
        let input = scenario_json(&scenario, &dir.0);
        let expected = run_node(&pinned, &dir.0, &input);
        let parsed = parse(&expected).expect("node json");
        assert!(
            parsed.get("timeout").is_none(),
            "{}: node timed out: {expected}",
            scenario.name
        );
        let actual = run_rust(&scenario, &dir.0);
        let expected = normalized(&parsed);
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

/// The Node result without the keys Rust does not report.
fn normalized(result: &JsValue) -> String {
    let mut object = JsObject::new();
    for key in [
        "first",
        "out",
        "titles",
        "commandFinished",
        "states",
        "exit",
    ] {
        object.insert(key, result.get(key).cloned().unwrap_or(JsValue::Undefined));
    }
    stringify(&JsValue::Object(object))
}

fn run_node(pinned: &support::Pinned, home: &Path, input: &str) -> String {
    let timeout = ["gtimeout", "timeout"]
        .into_iter()
        .flat_map(|name| {
            std::env::var_os("PATH")
                .into_iter()
                .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
                .map(move |dir| dir.join(name))
        })
        .find(|candidate| candidate.is_file())
        .expect("gtimeout or timeout on PATH");
    let mut command = std::process::Command::new(timeout);
    command
        .args(["--kill-after=5", "60"])
        .arg(&pinned.node)
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .arg(&pinned.terminal_dir)
        .arg(input)
        .env_clear()
        .current_dir(home);
    for (key, value) in base_env(home) {
        command.env(key, value);
    }
    let output = command.output().expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8")
}
