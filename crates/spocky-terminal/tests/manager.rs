//! The terminal registry against the pinned `createTerminalManager`: the same
//! operation script runs through `terminal-manager.js` on the pinned Node and
//! through [`TerminalManager`] with real sessions. Every operation result, the
//! `terminals changed` events in order, the captured text (which includes the
//! environment each shell received, in order) and the cell states must match
//! as text. The activity tracker is not part of this port, so list items are
//! compared without `activity`.
//!
//! Terminal ids and activity tokens are given explicitly. Every wait is
//! bounded and only sessions the script created are signalled.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_terminal::capture::CaptureOptions;
use spocky_terminal::manager::{
    CreateOptions, SessionDefaults, TerminalManager, TerminalsChangedEvent,
};
use spocky_terminal::restore::SnapshotOptions;
use spocky_terminal::session::TerminalSession;
use spocky_wire::encode_terminal_snapshot;

const WAIT: Duration = Duration::from_secs(10);
const ENV_SCRIPT: &str = "env | grep -E '^(FOO|BAR|ORDER|PASEO_TERMINAL_ID|PASEO_ACTIVITY_TOKEN|PASEO_TERMINAL_ACTIVITY_URL)='; printf ready; read l";
const IDLE_SCRIPT: &str = "printf ready; read l";
const HUP_SCRIPT: &str = "trap '' HUP; printf ready; read l";
const EXIT_SCRIPT: &str = "sleep 0.6; printf ready";
const ACTIVITY_URL: &str = "http://127.0.0.1:9/terminal-activity";

/// `activityUrl` of a create: left out, `null`, or a string.
#[derive(Clone, Copy)]
enum Url {
    Unset,
    Null,
    Text(&'static str),
}

struct Create {
    id: &'static str,
    cwd: &'static str,
    workspace_id: &'static str,
    name: Option<&'static str>,
    title: Option<&'static str>,
    env: &'static [(&'static str, &'static str)],
    script: &'static str,
    token: Option<&'static str>,
    url: Url,
}

enum Op {
    RegisterEnv(&'static str, &'static [(&'static str, &'static str)]),
    Create(Create),
    GetTerminals(&'static str, Option<&'static str>),
    Token(&'static str, &'static str),
    SetTitle(&'static str, &'static str),
    ListDirectories,
    Observe(&'static str),
    Get(&'static str),
    Kill(&'static str),
    KillWait(&'static str),
    KillAll,
    WaitGone(&'static str),
}

const fn create(id: &'static str, cwd: &'static str, script: &'static str) -> Create {
    Create {
        id,
        cwd,
        workspace_id: "ws1",
        name: None,
        title: None,
        env: &[],
        script,
        token: None,
        url: Url::Unset,
    }
}

/// Directories made under the temp root.
const DIRS: &[&str] = &["/a", "/a/deep", "/b", "/ab"];

fn ops() -> Vec<Op> {
    vec![
        Op::ListDirectories,
        Op::GetTerminals("relative/path", None),
        Op::Create(create("rel", "relative/path", IDLE_SCRIPT)),
        Op::RegisterEnv("relative", &[]),
        Op::RegisterEnv("C:\\Users\\win", &[("FOO", "windows")]),
        Op::RegisterEnv("", &[]),
        Op::RegisterEnv("/a", &[("FOO", "root"), ("ORDER", "1")]),
        Op::RegisterEnv("/a/deep", &[("FOO", "deep"), ("BAR", "inherited")]),
        Op::Create(Create {
            token: Some("tok-env"),
            env: &[("BAR", "own"), ("ORDER", "2"), ("__proto__", "x")],
            url: Url::Unset,
            ..create("env", "/a/deep", ENV_SCRIPT)
        }),
        Op::Observe("env"),
        Op::Create(Create {
            token: Some("tok-null"),
            url: Url::Null,
            ..create("null-url", "/a", ENV_SCRIPT)
        }),
        Op::Observe("null-url"),
        Op::Create(Create {
            token: Some("tok-url"),
            url: Url::Text("http://127.0.0.1:9/explicit"),
            env: &[("FOO", "own-a")],
            ..create("text-url", "/a", ENV_SCRIPT)
        }),
        Op::Observe("text-url"),
        Op::Create(Create {
            token: Some("tok-empty"),
            url: Url::Text(""),
            ..create("empty-url", "/b", ENV_SCRIPT)
        }),
        Op::Observe("empty-url"),
        Op::Create(Create {
            name: Some("named"),
            title: Some("preset"),
            workspace_id: "ws2",
            ..create("named", "/b", IDLE_SCRIPT)
        }),
        Op::Create(Create {
            name: Some(""),
            ..create("empty-name", "/ab", IDLE_SCRIPT)
        }),
        Op::Create(create("root", "/", IDLE_SCRIPT)),
        Op::GetTerminals("/", None),
        Op::GetTerminals("/", Some("ws1")),
        Op::GetTerminals("/", Some("ws2")),
        Op::GetTerminals("/", Some("")),
        Op::GetTerminals("/a", None),
        Op::GetTerminals("/a/", None),
        Op::GetTerminals("/a/deep", Some("ws1")),
        Op::GetTerminals("/ab", None),
        Op::GetTerminals("/b", Some("ws2")),
        Op::GetTerminals("/elsewhere", None),
        Op::GetTerminals("C:\\a", None),
        Op::ListDirectories,
        Op::Token("env", "tok-env"),
        Op::Token("env", "wrong"),
        Op::Token("env", ""),
        Op::Token("missing", "tok-env"),
        Op::SetTitle("env", "  my title  "),
        Op::SetTitle("missing", "x"),
        Op::Observe("env"),
        Op::Get("env"),
        Op::Get("missing"),
        Op::Create(Create {
            token: Some("tok-fail"),
            ..create("fail", "/missing-directory", IDLE_SCRIPT)
        }),
        Op::Token("fail", "tok-fail"),
        Op::Get("fail"),
        Op::Create(create("exits", "/a/deep", EXIT_SCRIPT)),
        Op::WaitGone("exits"),
        Op::Token("exits", "anything"),
        Op::Kill("null-url"),
        Op::Kill("null-url"),
        Op::Get("null-url"),
        Op::Create(create("hup", "/a", HUP_SCRIPT)),
        Op::Observe("hup"),
        Op::KillWait("hup"),
        Op::Get("hup"),
        Op::KillWait("missing"),
        Op::ListDirectories,
        Op::GetTerminals("/", None),
        Op::KillAll,
        Op::ListDirectories,
        Op::GetTerminals("/", None),
        Op::Token("env", "tok-env"),
    ]
}

fn js_text(text: &str) -> JsValue {
    JsValue::String(text.to_owned())
}

fn path_of(root: &Path, suffix: &str) -> String {
    // Relative, empty and drive-letter paths are passed through unchanged.
    if suffix == "/" {
        root.to_string_lossy().into_owned()
    } else if suffix.starts_with('/') {
        let trimmed = suffix.trim_end_matches('/');
        let trailing = &suffix[trimmed.len()..];
        if DIRS.contains(&trimmed) || trimmed == "/missing-directory" || trimmed == "/elsewhere" {
            format!("{}{trimmed}{trailing}", root.to_string_lossy())
        } else {
            suffix.to_owned()
        }
    } else {
        suffix.to_owned()
    }
}

fn pairs_json(pairs: &[(&str, &str)]) -> JsValue {
    // `JSON.parse` keeps `__proto__` as an own key, which the revival in the
    // Node script then spreads like the baseline callers do.
    let mut object = JsObject::new();
    for (key, value) in pairs {
        object.insert(*key, js_text(value));
    }
    JsValue::Object(object)
}

fn ops_json(root: &Path) -> String {
    let entries = ops()
        .iter()
        .map(|op| {
            let mut entry = JsObject::new();
            let mut set = |key: &str, value: JsValue| entry.insert(key, value);
            match op {
                Op::RegisterEnv(cwd, env) => {
                    set("op", js_text("registerEnv"));
                    set("cwd", js_text(&path_of(root, cwd)));
                    set("env", pairs_json(env));
                }
                Op::Create(c) => {
                    set("op", js_text("create"));
                    set("id", js_text(c.id));
                    set("cwd", js_text(&path_of(root, c.cwd)));
                    set("workspaceId", js_text(c.workspace_id));
                    set("name", c.name.map_or(JsValue::Null, js_text));
                    set("title", c.title.map_or(JsValue::Null, js_text));
                    set("env", pairs_json(c.env));
                    set("hasEnv", JsValue::Bool(!c.env.is_empty()));
                    set("script", js_text(c.script));
                    set("token", c.token.map_or(JsValue::Null, js_text));
                    set(
                        "url",
                        match c.url {
                            Url::Unset => js_text("unset"),
                            Url::Null => js_text("null"),
                            Url::Text(text) => js_text(&format!("text:{text}")),
                        },
                    );
                }
                Op::GetTerminals(cwd, workspace) => {
                    set("op", js_text("getTerminals"));
                    set("cwd", js_text(&path_of(root, cwd)));
                    set("workspaceId", workspace.map_or(JsValue::Null, js_text));
                }
                Op::Token(id, token) => {
                    set("op", js_text("token"));
                    set("id", js_text(id));
                    set("token", js_text(token));
                }
                Op::SetTitle(id, title) => {
                    set("op", js_text("setTitle"));
                    set("id", js_text(id));
                    set("title", js_text(title));
                }
                Op::ListDirectories => set("op", js_text("listDirectories")),
                Op::Observe(id) => {
                    set("op", js_text("observe"));
                    set("id", js_text(id));
                }
                Op::Get(id) => {
                    set("op", js_text("get"));
                    set("id", js_text(id));
                }
                Op::Kill(id) => {
                    set("op", js_text("kill"));
                    set("id", js_text(id));
                }
                Op::KillWait(id) => {
                    set("op", js_text("killWait"));
                    set("id", js_text(id));
                }
                Op::KillAll => set("op", js_text("killAll")),
                Op::WaitGone(id) => {
                    set("op", js_text("waitGone"));
                    set("id", js_text(id));
                }
            }
            JsValue::Object(entry)
        })
        .collect();
    stringify(&JsValue::Array(entries))
}

const NODE_SCRIPT: &str = r#"
const [terminalDir, opsJson, activityUrl] = process.argv.slice(1);
const { createTerminalManager } = await import(`${terminalDir}/terminal-manager.js`);
const ops = JSON.parse(opsJson);
const manager = createTerminalManager({ getTerminalActivityUrl: () => activityUrl });
const events = [];
manager.subscribeTerminalsChanged((event) => {
  events.push({
    cwd: event.cwd,
    terminals: event.terminals.map(({ id, name, cwd, workspaceId, title }) => ({ id, name, cwd, workspaceId, title })),
  });
});
const item = ({ id, name, cwd, workspaceId }, title) => ({ id, name, cwd, workspaceId, title });
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
const until = async (test) => {
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    if (await test()) return true;
    await sleep(20);
  }
  return false;
};
const run = async (op) => {
  switch (op.op) {
    case "registerEnv":
      manager.registerCwdEnv({ cwd: op.cwd, env: op.env });
      return null;
    case "create": {
      const session = await manager.createTerminal({
        id: op.id,
        cwd: op.cwd,
        workspaceId: op.workspaceId,
        ...(op.name === null ? {} : { name: op.name }),
        ...(op.title === null ? {} : { title: op.title }),
        ...(op.hasEnv ? { env: op.env } : {}),
        command: "/bin/sh",
        args: ["-c", op.script],
        rows: 5,
        cols: 100,
        ...(op.token === null ? {} : { activityToken: op.token }),
        ...(op.url === "unset" ? {} : { activityUrl: op.url === "null" ? null : op.url.slice(5) }),
      });
      return item(session, session.getTitle());
    }
    case "getTerminals":
      return (await manager.getTerminals(op.cwd, op.workspaceId === null ? undefined : { workspaceId: op.workspaceId })).map((s) => s.id);
    case "token":
      return manager.validateTerminalActivityToken(op.id, op.token);
    case "setTitle":
      return manager.setTerminalTitle(op.id, op.title);
    case "listDirectories":
      return manager.listDirectories();
    case "observe": {
      const seen = await until(async () => (await manager.captureTerminal(op.id, {})).lines.join("\n").includes("ready"));
      if (!seen) return "timeout";
      const capture = await manager.captureTerminal(op.id, {});
      const snapshot = await manager.getTerminalState(op.id, { includeWrapFlags: true });
      return { capture, title: manager.getTerminal(op.id).getTitle(), state: snapshot.state };
    }
    case "get":
      return manager.getTerminal(op.id) !== undefined;
    case "kill":
      manager.killTerminal(op.id);
      return null;
    case "killWait":
      await manager.killTerminalAndWait(op.id);
      return null;
    case "killAll":
      manager.killAll();
      return null;
    case "waitGone":
      return await until(async () => manager.getTerminal(op.id) === undefined);
    default:
      throw new Error(`unknown op ${op.op}`);
  }
};
const results = [];
for (const op of ops) {
  try {
    results.push({ ok: await run(op) });
  } catch (error) {
    results.push({ error: error.message });
  }
}
process.stdout.write(JSON.stringify({ results, events }), () => process.exit(0));
"#;

fn base_env(home: &Path) -> Vec<(&'static str, String)> {
    vec![
        ("PATH", "/usr/bin:/bin".to_owned()),
        ("HOME", home.to_string_lossy().into_owned()),
        ("LANG", "en_US.UTF-8".to_owned()),
    ]
}

fn defaults(root: &Path) -> SessionDefaults {
    let mut process_env = JsObject::new();
    for (key, value) in base_env(root) {
        process_env.insert(key, JsValue::String(value));
    }
    SessionDefaults {
        process_env,
        paseo_cli_bin_dir: None,
        paseo_hook_cli_path: None,
        zsh_integration_dir: None,
        tmpdir: std::env::temp_dir(),
        username: "spocky".to_owned(),
        pid: std::process::id(),
        process_cwd: root.to_string_lossy().into_owned(),
        helper: PathBuf::from(env!("CARGO_BIN_EXE_spocky-pty-helper")),
    }
}

fn env_object(pairs: &[(&str, &str)]) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in pairs {
        object.insert(*key, js_text(value));
    }
    object
}

fn item_json(session: &TerminalSession, title: Option<String>) -> JsValue {
    let mut item = JsObject::new();
    item.insert("id", js_text(&session.id));
    item.insert("name", js_text(&session.name));
    item.insert("cwd", js_text(&session.cwd.to_string_lossy()));
    item.insert("workspaceId", js_text(&session.workspace_id));
    item.insert("title", title.map_or(JsValue::Undefined, JsValue::String));
    JsValue::Object(item)
}

fn ok(value: JsValue) -> JsValue {
    let mut object = JsObject::new();
    object.insert("ok", value);
    JsValue::Object(object)
}

fn failure(message: &str) -> JsValue {
    let mut object = JsObject::new();
    object.insert("error", js_text(message));
    JsValue::Object(object)
}

fn until(test: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if test() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn snapshot_json(state: &spocky_wire::TerminalState) -> JsValue {
    parse(&String::from_utf8(encode_terminal_snapshot(state).expect("json")).expect("utf8"))
        .expect("state json")
}

fn run_rust(root: &Path) -> String {
    let manager = TerminalManager::new(
        defaults(root),
        Some(Arc::new(|| Some(ACTIVITY_URL.to_owned()))),
    );
    let events: Arc<Mutex<Vec<JsValue>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    manager.subscribe_terminals_changed(move |event: &TerminalsChangedEvent| {
        let mut object = JsObject::new();
        object.insert("cwd", js_text(&event.cwd));
        object.insert(
            "terminals",
            JsValue::Array(
                event
                    .terminals
                    .iter()
                    .map(|terminal| {
                        let mut item = JsObject::new();
                        item.insert("id", js_text(&terminal.id));
                        item.insert("name", js_text(&terminal.name));
                        item.insert("cwd", js_text(&terminal.cwd));
                        item.insert("workspaceId", js_text(&terminal.workspace_id));
                        item.insert(
                            "title",
                            terminal
                                .title
                                .clone()
                                .map_or(JsValue::Undefined, JsValue::String),
                        );
                        JsValue::Object(item)
                    })
                    .collect(),
            ),
        );
        sink.lock().expect("lock").push(JsValue::Object(object));
    });

    let mut results = Vec::new();
    for op in ops() {
        results.push(run_op(&manager, root, &op));
    }
    let mut object = JsObject::new();
    object.insert("results", JsValue::Array(results));
    object.insert(
        "events",
        JsValue::Array(std::mem::take(&mut *events.lock().expect("lock"))),
    );
    stringify(&JsValue::Object(object))
}

fn id_list(sessions: &[TerminalSession]) -> JsValue {
    JsValue::Array(sessions.iter().map(|s| js_text(&s.id)).collect())
}

fn run_op(manager: &TerminalManager, root: &Path, op: &Op) -> JsValue {
    match op {
        Op::RegisterEnv(cwd, env) => {
            match manager.register_cwd_env(&path_of(root, cwd), &env_object(env)) {
                Ok(()) => ok(JsValue::Null),
                Err(error) => failure(&error.0),
            }
        }
        Op::Create(c) => {
            let created = manager.create_terminal(&CreateOptions {
                id: Some(c.id.to_owned()),
                cwd: path_of(root, c.cwd),
                workspace_id: c.workspace_id.to_owned(),
                name: c.name.map(str::to_owned),
                title: c.title.map(str::to_owned),
                env: (!c.env.is_empty()).then(|| env_object(c.env)),
                command: Some("/bin/sh".to_owned()),
                args: Some(vec!["-c".to_owned(), c.script.to_owned()]),
                rows: Some(5),
                cols: Some(100),
                activity_token: c.token.map(str::to_owned),
                activity_url: match c.url {
                    Url::Unset => None,
                    Url::Null => Some(None),
                    Url::Text(text) => Some(Some(text.to_owned())),
                },
            });
            match created {
                Ok(session) => ok(item_json(&session, session.title())),
                Err(error) => failure(&error.0),
            }
        }
        Op::GetTerminals(cwd, workspace) => {
            match manager.get_terminals(&path_of(root, cwd), *workspace) {
                Ok(sessions) => ok(id_list(&sessions)),
                Err(error) => failure(&error.0),
            }
        }
        Op::Token(id, token) => {
            let text = match manager.validate_activity_token(id, token) {
                spocky_terminal::manager::TokenCheck::Valid => "valid",
                spocky_terminal::manager::TokenCheck::Unknown => "unknown",
                spocky_terminal::manager::TokenCheck::Invalid => "invalid",
            };
            ok(js_text(text))
        }
        Op::SetTitle(id, title) => ok(JsValue::Bool(manager.set_terminal_title(id, title))),
        Op::ListDirectories => ok(JsValue::Array(
            manager
                .list_directories()
                .iter()
                .map(|d| js_text(d))
                .collect(),
        )),
        Op::Observe(id) => observe(manager, id),
        Op::Get(id) => ok(JsValue::Bool(manager.get_terminal(id).is_some())),
        Op::Kill(id) => {
            manager.kill_terminal(id);
            ok(JsValue::Null)
        }
        Op::KillWait(id) => {
            manager.kill_terminal_and_wait(id, None, None);
            ok(JsValue::Null)
        }
        Op::KillAll => {
            manager.kill_all();
            ok(JsValue::Null)
        }
        Op::WaitGone(id) => ok(JsValue::Bool(until(|| manager.get_terminal(id).is_none()))),
    }
}

/// `capture`, title and cell state once the shell printed its marker.
fn observe(manager: &TerminalManager, id: &str) -> JsValue {
    let seen = until(|| {
        manager
            .capture_terminal(id, &CaptureOptions::default())
            .lines
            .join("\n")
            .contains("ready")
    });
    if !seen {
        return ok(js_text("timeout"));
    }
    let capture = manager.capture_terminal(id, &CaptureOptions::default());
    let snapshot = manager
        .get_terminal_state(
            id,
            SnapshotOptions {
                scrollback_lines: None,
                include_wrap_flags: true,
            },
        )
        .expect("state");
    let mut capture_object = JsObject::new();
    capture_object.insert(
        "lines",
        JsValue::Array(capture.lines.iter().map(|l| js_text(l)).collect()),
    );
    #[allow(clippy::cast_precision_loss)]
    capture_object.insert("totalLines", JsValue::Number(capture.total_lines as f64));
    let mut object = JsObject::new();
    object.insert("capture", JsValue::Object(capture_object));
    object.insert(
        "title",
        manager
            .get_terminal(id)
            .and_then(|s| s.title())
            .map_or(JsValue::Undefined, JsValue::String),
    );
    object.insert("state", snapshot_json(&snapshot.state));
    ok(JsValue::Object(object))
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "spocky-terminal-manager-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("temp dir");
        for dir in DIRS {
            std::fs::create_dir_all(format!("{}{dir}", path.display())).expect("temp dir");
        }
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
        text[at.saturating_sub(80)..(at + 200).min(text.len())]
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

fn run_node(pinned: &support::Pinned, root: &Path, input: &str) -> String {
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
        .args(["--kill-after=5", "120"])
        .arg(&pinned.node)
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .arg(&pinned.terminal_dir)
        .arg(input)
        .arg(ACTIVITY_URL)
        .env_clear()
        .current_dir(root);
    for (key, value) in base_env(root) {
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

#[test]
fn manager_matches_pinned_create_terminal_manager() {
    let Some(pinned) = support::pinned("manager differential") else {
        return;
    };
    support::assert_pinned_modules(&pinned.terminal_dir);
    let root = TempDir::new();
    let input = ops_json(&root.0);
    let expected = run_node(&pinned, &root.0, &input);
    let expected = stringify(&parse(&expected).expect("node json"));
    let actual = run_rust(&root.0);
    assert!(
        actual == expected,
        "{}",
        first_difference(&expected, &actual)
    );
}
