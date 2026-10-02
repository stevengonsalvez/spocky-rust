//! Differential check of message receipts against the pinned Paseo build:
//! node imports the built `server/message-receipts/index.js` and runs a
//! scripted step list in one disposable root, the Rust port runs the same
//! steps in another, and both print, after every step, each send's result
//! (`{ name, message, ...error }` for a rejection), the delivery and prepare
//! counts, and every file under the root with its mode, SHA-256, and text.
//! The two outputs must be identical after normalization.
//!
//! Normalization (each covered by a test below) replaces only the
//! disposable root path with `<root>`, and the pid, `Date.now()`, and
//! `randomUUID()` parts of `writeFileAtomic` temp names with `<pid>`,
//! `<ms>`, and `<uuid>`. Results, error text, ordering, file bytes, and
//! modes are never normalized.
//!
//! Needs `SPOCKY_PINNED_NODE` (node 22.20.0) and `SPOCKY_PASEO_DIST` (the
//! pinned build's `packages/server/dist/server`). Without them the tests
//! FAIL; `SPOCKY_ALLOW_SKIP=1` (exactly) skips them explicitly outside the
//! gate. `SPOCKY_RECEIPTS_EVIDENCE` names a directory that receives the raw
//! and normalized outputs of both sides.
//!
//! Failure injection uses directory permissions, so the scenarios run on
//! unix only and must not run as root.
#![cfg(unix)]

use std::collections::HashMap;
use std::fmt::{Display, Write as _};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_message_receipts::node_fs::FsError;
use spocky_message_receipts::{Delivery, MessageReceipts, ReceiptError, digest};

/// A disposable root; restores write permission everywhere before removal.
struct Disposable(PathBuf);

impl Drop for Disposable {
    fn drop(&mut self) {
        fn writable(path: &Path) {
            if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
                for entry in fs::read_dir(path).into_iter().flatten().flatten() {
                    writable(&entry.path());
                }
            }
        }
        writable(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn disposable_root(side: &str) -> (Disposable, String) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "spocky-receipts-{side}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create disposable root");
    let text = root.to_string_lossy().into_owned();
    (Disposable(root), text)
}

/// SHA-256 of the pinned dist modules under test, recorded in
/// `evidence/phase3/receipts-differential.md`.
const PINNED_MODULES: [(&str, &str); 2] = [
    (
        "server/message-receipts/index.js",
        "e99ca1a266f038efbceaf398b45ccb2e904a58ca46e4422546dc22ea498c4559",
    ),
    (
        "server/atomic-file.js",
        "835d68e580f2d1d5ae344559bf6eca4829ede020ea2416e8e8e2115303121c25",
    ),
];

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// Fails unless `node` reports v22.20.0 and `dist` holds the pinned modules.
fn verify_pinned(node: &std::ffi::OsStr, dist: &std::ffi::OsStr) {
    let version = Command::new(node)
        .args(["-p", "process.version"])
        .output()
        .expect("run pinned node");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "v22.20.0",
        "SPOCKY_PINNED_NODE is not node 22.20.0"
    );
    for (module, expected) in PINNED_MODULES {
        let path = Path::new(dist).join(module);
        let bytes = fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert_eq!(
            sha256_hex(&bytes),
            expected,
            "{} is not the pinned build",
            path.display()
        );
    }
}

/// The pinned node and dist paths, verified, or `None` when skipping was
/// requested.
fn pinned_inputs() -> Option<(std::ffi::OsString, std::ffi::OsString)> {
    match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => {
            verify_pinned(&node, &dist);
            Some((node, dist))
        }
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: pinned differential not run");
            None
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    }
}

/// `gtimeout` on macOS with coreutils, else `timeout`.
fn timeout_program() -> &'static str {
    if Command::new("gtimeout").arg("--version").output().is_ok() {
        "gtimeout"
    } else {
        "timeout"
    }
}

fn run_node(node: &std::ffi::OsStr, script: &str, args: &[&std::ffi::OsStr]) -> String {
    let output = Command::new(timeout_program())
        .args(["--kill-after=5", "120"])
        .arg(node)
        .args(["--input-type=module", "-e", script])
        .args(args)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("node prints UTF-8")
}

/// The scripted steps. A path is relative to the root, or
/// `{"receipt": [dir, agentId, messageId]}` for that send's receipt file. A
/// callback runs its filesystem `ops`, then rejects with `fail` if present.
/// `count: 2` issues two identical sends at once on one instance; `racer`
/// names a second instance on the same directory that sends at the same
/// time.
const STEPS: &str = r#"[
  {"label":"first delivery","op":"send","instance":"a","dir":"receipts","agentId":"agent","messageId":"m1","request":{"text":"hello","b":{"y":1,"x":[{"d":1,"c":2}]},"10":1,"2":2,"B":true,"_k":null,"a b":"é"},"prepare":{"ops":[]},"send":{"ops":[]}},
  {"label":"concurrent duplicates","op":"send","instance":"a","dir":"receipts","agentId":"agent","messageId":"m1","request":{"text":"hello","b":{"y":1,"x":[{"d":1,"c":2}]},"10":1,"2":2,"B":true,"_k":null,"a b":"é"},"send":{"ops":[]},"count":2},
  {"label":"concurrent first sends deliver once","op":"send","instance":"a","dir":"receipts","agentId":"agent","messageId":"m10","request":{"text":"fresh"},"prepare":{"ops":[]},"send":{"ops":[]},"count":2},
  {"label":"two instances racing one key both deliver","op":"send","instance":"j","racer":"k","dir":"receipts","agentId":"agent","messageId":"m11","request":{"text":"raced"},"prepare":{"ops":[]},"send":{"ops":[]}},
  {"label":"duplicate after restart","op":"send","instance":"b","dir":"receipts","agentId":"agent","messageId":"m1","request":{"text":"hello","b":{"y":1,"x":[{"d":1,"c":2}]},"10":1,"2":2,"B":true,"_k":null,"a b":"é"},"send":{"ops":[]}},
  {"label":"reordered request keys are the same request","op":"send","instance":"b","dir":"receipts","agentId":"agent","messageId":"m1","request":{"a b":"é","_k":null,"B":true,"2":2,"b":{"x":[{"c":2,"d":1}],"y":1},"text":"hello","10":1},"send":{"ops":[]}},
  {"label":"key conflict","op":"send","instance":"b","dir":"receipts","agentId":"agent","messageId":"m1","request":{"text":"other"},"send":{"ops":[]}},
  {"label":"another agent delivers","op":"send","instance":"b","dir":"receipts","agentId":"another","messageId":"m1","request":{"text":"hello"},"send":{"ops":[]}},
  {"label":"provider send fails","op":"send","instance":"b","dir":"receipts","agentId":"agent","messageId":"m2","request":{},"send":{"ops":[],"fail":"connection lost"}},
  {"label":"pending receipt after restart is unknown","op":"send","instance":"c","dir":"receipts","agentId":"agent","messageId":"m2","request":{},"send":{"ops":[]}},
  {"label":"failed prepare leaves no receipt","op":"send","instance":"c","dir":"receipts","agentId":"agent","messageId":"m3","request":{},"prepare":{"ops":[],"fail":"load failed"},"send":{"ops":[]}},
  {"label":"prepared send after restart","op":"send","instance":"d","dir":"receipts","agentId":"agent","messageId":"m3","request":{},"prepare":{"ops":[]},"send":{"ops":[]}},
  {"label":"completed receipt skips a failing prepare","op":"send","instance":"c","dir":"receipts","agentId":"agent","messageId":"m3","request":{},"prepare":{"ops":[],"fail":"load failed"},"send":{"ops":[]}},
  {"label":"truncate receipt","op":"write","path":{"receipt":["receipts","agent","m1"]},"text":""},
  {"label":"empty receipt","op":"send","instance":"d","dir":"receipts","agentId":"agent","messageId":"m1","request":{},"send":{"ops":[]}},
  {"label":"cut receipt","op":"write","path":{"receipt":["receipts","agent","m1"]},"text":"{\"fingerprint\":\"ab"},
  {"label":"cut receipt rejects","op":"send","instance":"d","dir":"receipts","agentId":"agent","messageId":"m1","request":{},"send":{"ops":[]}},
  {"label":"garbage receipt","op":"write","path":{"receipt":["receipts","agent","m1"]},"text":"the receipt was overwritten by something else"},
  {"label":"garbage receipt rejects","op":"send","instance":"d","dir":"receipts","agentId":"agent","messageId":"m1","request":{},"send":{"ops":[]}},
  {"label":"multi-line receipt","op":"write","path":{"receipt":["receipts","agent","m1"]},"text":"{\n  \"fingerprint\": \"a\",\n  \"state\": pending\n}"},
  {"label":"multi-line receipt rejects","op":"send","instance":"d","dir":"receipts","agentId":"agent","messageId":"m1","request":{},"send":{"ops":[]}},
  {"label":"wrong shape receipt","op":"write","path":{"receipt":["receipts","agent","m1"]},"text":"{\"fingerprint\":1,\"state\":\"done\"}"},
  {"label":"wrong shape receipt rejects","op":"send","instance":"d","dir":"receipts","agentId":"agent","messageId":"m1","request":{},"send":{"ops":[]}},
  {"label":"array receipt","op":"write","path":{"receipt":["receipts","agent","m1"]},"text":"[]"},
  {"label":"array receipt rejects","op":"send","instance":"d","dir":"receipts","agentId":"agent","messageId":"m1","request":{},"send":{"ops":[]}},
  {"label":"null receipt","op":"write","path":{"receipt":["receipts","agent","m1"]},"text":"null"},
  {"label":"null receipt rejects","op":"send","instance":"d","dir":"receipts","agentId":"agent","messageId":"m1","request":{},"send":{"ops":[]}},
  {"label":"byte order mark receipt","op":"write","path":{"receipt":["receipts","agent","m1"]},"text":"﻿{}"},
  {"label":"byte order mark receipt rejects","op":"send","instance":"d","dir":"receipts","agentId":"agent","messageId":"m1","request":{},"send":{"ops":[]}},
  {"label":"invalid UTF-8 receipt","op":"write","path":{"receipt":["receipts","agent","m1"]},"hex":"7b22f09f98222c22ff223a317d"},
  {"label":"invalid UTF-8 receipt rejects","op":"send","instance":"d","dir":"receipts","agentId":"agent","messageId":"m1","request":{},"send":{"ops":[]}},
  {"label":"truncated UTF-8 sequences","op":"write","path":{"receipt":["receipts","agent","m1"]},"hex":"fffef09f987bc37d"},
  {"label":"truncated UTF-8 sequences reject","op":"send","instance":"d","dir":"receipts","agentId":"agent","messageId":"m1","request":{},"send":{"ops":[]}},
  {"label":"remove corrupt receipt","op":"rm","path":{"receipt":["receipts","agent","m1"]}},
  {"label":"removed receipt redelivers","op":"send","instance":"d","dir":"receipts","agentId":"agent","messageId":"m1","request":{},"send":{"ops":[]}},
  {"label":"receipt path is a directory","op":"mkdir","path":{"receipt":["receipts","agent","m9"]}},
  {"label":"directory receipt rejects","op":"send","instance":"d","dir":"receipts","agentId":"agent","messageId":"m9","request":{},"send":{"ops":[]}},
  {"label":"remove directory receipt","op":"rm","path":{"receipt":["receipts","agent","m9"]}},
  {"label":"lock receipts directory","op":"chmod","path":"receipts","mode":365},
  {"label":"pending write fails","op":"send","instance":"d","dir":"receipts","agentId":"agent","messageId":"m4","request":{},"prepare":{"ops":[]},"send":{"ops":[]}},
  {"label":"unlock receipts directory","op":"chmod","path":"receipts","mode":493},
  {"label":"completed write fails after delivery","op":"send","instance":"d","dir":"receipts","agentId":"agent","messageId":"m5","request":{},"send":{"ops":[{"op":"chmod","path":"receipts","mode":365}]}},
  {"label":"unlock after completed write failure","op":"chmod","path":"receipts","mode":493},
  {"label":"delivered message is now an unknown outcome","op":"send","instance":"e","dir":"receipts","agentId":"agent","messageId":"m5","request":{},"send":{"ops":[]}},
  {"label":"rename over a directory fails","op":"send","instance":"e","dir":"receipts","agentId":"agent","messageId":"m6","request":{},"prepare":{"ops":[{"op":"mkdir","path":{"receipt":["receipts","agent","m6"]}}]},"send":{"ops":[]}},
  {"label":"directory receipt read fails","op":"send","instance":"e","dir":"receipts","agentId":"agent","messageId":"m6","request":{},"send":{"ops":[]}},
  {"label":"directory parent replaced by a file","op":"send","instance":"f","dir":"sub/nested","agentId":"agent","messageId":"m7","request":{},"prepare":{"ops":[{"op":"write","path":"sub","text":"x"}]},"send":{"ops":[]}},
  {"label":"read through a file fails","op":"send","instance":"f","dir":"sub/nested","agentId":"agent","messageId":"m7","request":{},"send":{"ops":[]}},
  {"label":"directory replaced by a file","op":"send","instance":"g","dir":"flat","agentId":"agent","messageId":"m7","request":{},"prepare":{"ops":[{"op":"write","path":"flat","text":"x"}]},"send":{"ops":[]}},
  {"label":"make locked parent","op":"mkdir","path":"ro"},
  {"label":"lock parent","op":"chmod","path":"ro","mode":365},
  {"label":"missing parents cannot be created","op":"send","instance":"h","dir":"ro/a/b","agentId":"agent","messageId":"m7","request":{},"send":{"ops":[]}},
  {"label":"unlock parent","op":"chmod","path":"ro","mode":493},
  {"label":"directory is joined like path.join","op":"send","instance":"i","dir":"receipts/../norm/./x//","agentId":"agent","messageId":"m8","request":[1,{"z":[],"y":{}}],"send":{"ops":[]}}
]"#;

const NODE_SCRIPT: &str = r#"
const [dist, root, stepsText] = process.argv.slice(1);
const { MessageReceipts } = await import(`${dist}/server/message-receipts/index.js`);
const fs = await import("node:fs/promises");
const path = await import("node:path");
const { createHash } = await import("node:crypto");
const steps = JSON.parse(stepsText);
const sha256 = (data) => createHash("sha256").update(data).digest("hex");
const resolve = (spec) => typeof spec === "string"
  ? path.join(root, spec)
  : path.join(root, spec.receipt[0], `${sha256(JSON.stringify(["send", spec.receipt[1], spec.receipt[2]]))}.json`);
async function apply(op) {
  const target = resolve(op.path);
  if (op.op === "write") await fs.writeFile(target, op.text !== undefined ? Buffer.from(op.text, "utf8") : Buffer.from(op.hex, "hex"));
  else if (op.op === "chmod") await fs.chmod(target, op.mode);
  else if (op.op === "mkdir") await fs.mkdir(target, { recursive: true });
  else if (op.op === "rm") await fs.rm(target, { recursive: true, force: true });
  else throw new Error(`unknown op ${op.op}`);
}
let deliveries = 0;
let prepares = 0;
const callback = (spec, count) => async () => {
  count();
  for (const op of spec.ops) await apply(op);
  if (spec.fail !== undefined) throw new Error(spec.fail);
};
async function listing() {
  const out = [];
  async function walk(rel) {
    for (const name of (await fs.readdir(path.join(root, rel))).sort()) {
      const relPath = rel ? `${rel}/${name}` : name;
      const stats = await fs.lstat(path.join(root, relPath));
      if (stats.isDirectory()) {
        out.push([relPath, stats.mode & 0o777, null, null]);
        await walk(relPath);
      } else {
        const bytes = await fs.readFile(path.join(root, relPath));
        out.push([relPath, stats.mode & 0o777, sha256(bytes), bytes.toString("utf8")]);
      }
    }
  }
  await walk("");
  return out;
}
const instances = new Map();
const rows = [];
for (const step of steps) {
  let results = [];
  if (step.op === "send") {
    if (!instances.has(step.instance)) instances.set(step.instance, new MessageReceipts(`${root}/${step.dir}`));
    if (step.racer && !instances.has(step.racer)) instances.set(step.racer, new MessageReceipts(`${root}/${step.dir}`));
    const receipts = instances.get(step.instance);
    const senders = step.racer ? [receipts, instances.get(step.racer)] : Array.from({ length: step.count ?? 1 }, () => receipts);
    const input = { agentId: step.agentId, messageId: step.messageId, request: step.request, send: callback(step.send, () => deliveries++) };
    if (step.prepare) input.prepare = callback(step.prepare, () => prepares++);
    const settled = await Promise.allSettled(senders.map((sender) => sender.send(input)));
    results = settled.map((outcome) => outcome.status === "fulfilled"
      ? { ok: true }
      : { error: { name: outcome.reason.name, message: outcome.reason.message, ...outcome.reason } });
  } else {
    await apply(step);
  }
  rows.push({ step: step.label, results, deliveries, prepares, files: await listing() });
}
process.stdout.write(JSON.stringify(rows));
"#;

fn string(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

fn number(value: impl Into<f64>) -> JsValue {
    JsValue::Number(value.into())
}

fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key, value);
    }
    JsValue::Object(object)
}

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or_else(|| panic!("step field {key}"))
}

fn text_field<'a>(value: &'a JsValue, key: &str) -> &'a str {
    field(value, key).as_str().expect("string field")
}

fn decode_hex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("hex byte"))
        .collect()
}

/// Rust side of `resolve(spec)`.
fn resolve(root: &str, spec: &JsValue) -> String {
    if let Some(relative) = spec.as_str() {
        return format!("{root}/{relative}");
    }
    let parts = field(spec, "receipt").as_array().expect("receipt triple");
    let text = |index: usize| parts[index].as_str().expect("receipt part");
    let key = digest(&JsValue::Array(vec![
        string("send"),
        string(text(1)),
        string(text(2)),
    ]));
    format!("{root}/{}/{key}.json", text(0))
}

fn apply(root: &str, op: &JsValue) {
    let target = resolve(root, field(op, "path"));
    match text_field(op, "op") {
        "write" => {
            let bytes = op.get("text").and_then(JsValue::as_str).map_or_else(
                || decode_hex(text_field(op, "hex")),
                |text| text.as_bytes().to_vec(),
            );
            fs::write(&target, bytes).expect("write op");
        }
        "chmod" => {
            let mode = field(op, "mode").as_f64().expect("mode");
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let mode = mode as u32;
            fs::set_permissions(&target, fs::Permissions::from_mode(mode)).expect("chmod op");
        }
        "mkdir" => fs::create_dir_all(&target).expect("mkdir op"),
        "rm" => {
            let _ = fs::remove_dir_all(&target).or_else(|_| fs::remove_file(&target));
        }
        other => panic!("unknown op {other}"),
    }
}

#[derive(Debug)]
struct Thrown(String);

impl Display for Thrown {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The scripted callbacks of one send step.
#[derive(Clone)]
struct Scripted {
    root: String,
    prepare: Option<JsValue>,
    send: JsValue,
    prepares: Arc<AtomicUsize>,
    deliveries: Arc<AtomicUsize>,
}

fn run_callback(root: &str, spec: &JsValue) -> Result<(), Thrown> {
    for op in field(spec, "ops").as_array().expect("ops") {
        apply(root, op);
    }
    spec.get("fail")
        .and_then(JsValue::as_str)
        .map_or(Ok(()), |message| Err(Thrown(message.to_owned())))
}

impl Delivery for Scripted {
    type Error = Thrown;

    async fn prepare(&mut self) -> Result<(), Thrown> {
        let Some(spec) = &self.prepare else {
            return Ok(());
        };
        self.prepares.fetch_add(1, Ordering::SeqCst);
        run_callback(&self.root, spec)
    }

    async fn send(&mut self) -> Result<(), Thrown> {
        self.deliveries.fetch_add(1, Ordering::SeqCst);
        run_callback(&self.root, &self.send)
    }
}

fn result_row(result: Result<(), ReceiptError<Thrown>>) -> JsValue {
    let Err(error) = result else {
        return object(vec![("ok", JsValue::Bool(true))]);
    };
    let name = match &error {
        ReceiptError::Syntax(_) => "SyntaxError",
        ReceiptError::Schema(_) => "ZodError",
        _ => "Error",
    };
    let mut entries = vec![
        ("name", string(name)),
        ("message", string(&error.to_string())),
    ];
    if let ReceiptError::Fs(fs_error) = &error {
        entries.push(("errno", number(fs_error.errno)));
        entries.push(("code", string(&fs_error.code())));
        entries.push(("syscall", string(fs_error.syscall)));
        if let Some(path) = &fs_error.path {
            entries.push(("path", string(path)));
        }
        if let Some(dest) = &fs_error.dest {
            entries.push(("dest", string(dest)));
        }
    }
    object(vec![("error", object(entries))])
}

fn listing(root: &str) -> JsValue {
    fn walk(root: &str, relative: &str, out: &mut Vec<JsValue>) {
        let directory = if relative.is_empty() {
            root.to_owned()
        } else {
            format!("{root}/{relative}")
        };
        let mut names: Vec<String> = fs::read_dir(&directory)
            .expect("list directory")
            .map(|entry| {
                entry
                    .expect("directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        for name in names {
            let path = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            let full = format!("{root}/{path}");
            let metadata = fs::symlink_metadata(&full).expect("lstat entry");
            let mode = number(metadata.permissions().mode() & 0o777);
            if metadata.is_dir() {
                out.push(JsValue::Array(vec![
                    string(&path),
                    mode,
                    JsValue::Null,
                    JsValue::Null,
                ]));
                walk(root, &path, out);
            } else {
                let bytes = fs::read(&full).expect("read entry");
                let hash = sha256_hex(&bytes);
                out.push(JsValue::Array(vec![
                    string(&path),
                    mode,
                    string(&hash),
                    string(&String::from_utf8_lossy(&bytes)),
                ]));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", &mut out);
    JsValue::Array(out)
}

async fn run_rust(root: &str, steps: &JsValue) -> String {
    let prepares = Arc::new(AtomicUsize::new(0));
    let deliveries = Arc::new(AtomicUsize::new(0));
    let mut instances: HashMap<String, MessageReceipts> = HashMap::new();
    let mut rows = Vec::new();
    for step in steps.as_array().expect("steps") {
        let mut results = Vec::new();
        if text_field(step, "op") == "send" {
            let racer = step.get("racer").and_then(JsValue::as_str);
            for name in std::iter::once(text_field(step, "instance")).chain(racer) {
                instances.entry(name.to_owned()).or_insert_with(|| {
                    MessageReceipts::new(format!("{root}/{}", text_field(step, "dir")))
                });
            }
            let receipts = &instances[text_field(step, "instance")];
            let second = match racer {
                Some(name) => Some(&instances[name]),
                None if step.get("count").and_then(JsValue::as_f64) == Some(2.0) => Some(receipts),
                None => None,
            };
            let scripted = Scripted {
                root: root.to_owned(),
                prepare: step.get("prepare").cloned(),
                send: field(step, "send").clone(),
                prepares: Arc::clone(&prepares),
                deliveries: Arc::clone(&deliveries),
            };
            let (agent, message) = (text_field(step, "agentId"), text_field(step, "messageId"));
            let request = field(step, "request");
            if let Some(other) = second {
                let (first, second) = tokio::join!(
                    receipts.send(agent, message, request, scripted.clone()),
                    other.send(agent, message, request, scripted),
                );
                results.push(result_row(first));
                results.push(result_row(second));
            } else {
                let result = receipts.send(agent, message, request, scripted).await;
                results.push(result_row(result));
            }
        } else {
            apply(root, step);
        }
        let count = |counter: &AtomicUsize| {
            number(u32::try_from(counter.load(Ordering::SeqCst)).expect("small count"))
        };
        rows.push(object(vec![
            ("step", string(text_field(step, "label"))),
            ("results", JsValue::Array(results)),
            ("deliveries", count(&deliveries)),
            ("prepares", count(&prepares)),
            ("files", listing(root)),
        ]));
    }
    stringify(&JsValue::Array(rows))
}

/// Replaces the disposable `root` with `<root>`, and the generated parts of
/// `writeFileAtomic` temp names (`.<name>.json.<pid>.<ms>.<uuid>.tmp`) with
/// `<pid>`, `<ms>`, and `<uuid>`. Nothing else changes.
fn normalize(text: &str, root: &str) -> String {
    let text = text.replace(root, "<root>");
    let marker = ".json.";
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(index) = rest.find(marker) {
        let (head, tail) = rest.split_at(index + marker.len());
        out.push_str(head);
        rest = tail;
        if let Some(length) = temp_suffix_length(rest) {
            out.push_str("<pid>.<ms>.<uuid>.tmp");
            rest = &rest[length..];
        }
    }
    out.push_str(rest);
    out
}

/// Length of `<digits>.<digits>.<uuid v4>.tmp` at the start of `text`.
fn temp_suffix_length(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = 0;
    for _ in 0..2 {
        let start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == start || bytes.get(index) != Some(&b'.') {
            return None;
        }
        index += 1;
    }
    let uuid = bytes.get(index..index + 36)?;
    let is_uuid = uuid.iter().enumerate().all(|(position, byte)| {
        if [8, 13, 18, 23].contains(&position) {
            *byte == b'-'
        } else {
            byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)
        }
    });
    (is_uuid && bytes.get(index + 36..index + 40) == Some(b".tmp")).then_some(index + 40)
}

#[test]
fn normalization_replaces_only_the_root() {
    assert_eq!(
        normalize(r#"{"path":"/tmp/r1/x","other":"/tmp/r10"}"#, "/tmp/r1/"),
        r#"{"path":"<root>x","other":"/tmp/r10"}"#
    );
}

#[test]
fn normalization_replaces_only_generated_temp_name_parts() {
    let name = ".7ca1.json.92922.1790878162052.0fc92c86-fcb5-4941-9894-9c226eee6342.tmp";
    assert_eq!(
        normalize(&format!("open '/d/{name}'"), "/unused"),
        "open '/d/.7ca1.json.<pid>.<ms>.<uuid>.tmp'"
    );
    for kept in [
        "a.json.12.34.not-a-uuid.tmp",
        "a.json.x.34.0fc92c86-fcb5-4941-9894-9c226eee6342.tmp",
        "a.json.12.34.0FC92C86-fcb5-4941-9894-9c226eee6342.tmp",
        "a.json.12.34.0fc92c86-fcb5-4941-9894-9c226eee6342.txt",
        "a.json",
    ] {
        assert_eq!(normalize(kept, "/unused"), kept);
    }
}

fn record_evidence(name: &str, contents: &str) {
    if let Some(directory) = std::env::var_os("SPOCKY_RECEIPTS_EVIDENCE") {
        fs::create_dir_all(&directory).expect("evidence directory");
        fs::write(Path::new(&directory).join(name), contents).expect("write evidence");
    }
}

#[tokio::test]
async fn receipts_match_pinned_build() {
    let Some((node, dist)) = pinned_inputs() else {
        return;
    };
    let steps = parse(STEPS).expect("steps are JSON");
    let (node_guard, node_root) = disposable_root("node");
    let expected = run_node(
        &node,
        NODE_SCRIPT,
        &[dist.as_os_str(), node_root.as_ref(), STEPS.as_ref()],
    );
    drop(node_guard);
    let (rust_guard, rust_root) = disposable_root("rust");
    let actual = run_rust(&rust_root, &steps).await;
    drop(rust_guard);
    let (expected_normal, actual_normal) = (
        normalize(&expected, &node_root),
        normalize(&actual, &rust_root),
    );
    record_evidence("receipts-node-raw.json", &expected);
    record_evidence("receipts-rust-raw.json", &actual);
    record_evidence("receipts-node-normalized.json", &expected_normal);
    record_evidence("receipts-rust-normalized.json", &actual_normal);
    // The scripted scenarios did run: every step produced a row.
    let rows = parse(&expected).expect("node rows");
    assert_eq!(
        rows.as_array().map(<[JsValue]>::len),
        steps.as_array().map(<[JsValue]>::len)
    );
    assert_eq!(actual_normal, expected_normal);
}

/// `digest({})`, the fingerprint of an empty request, for receipts written by
/// hand.
const EMPTY_REQUEST_FINGERPRINT: &str =
    "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";

/// The steps that build an old home: deliveries, a failed delivery, receipts
/// of other agents and request key orders, then receipts damaged or written
/// by hand. `@F` is [`EMPTY_REQUEST_FINGERPRINT`].
const OLD_HOME_STEPS: &str = r#"[
  {"label":"delivered","op":"send","instance":"w","dir":"receipts","agentId":"agent","messageId":"m1","request":{"text":"hello","10":1,"2":2,"b":{"y":1,"x":[{"d":1,"c":2}]}},"prepare":{"ops":[]},"send":{"ops":[]}},
  {"label":"delivery fails after the pending receipt","op":"send","instance":"w","dir":"receipts","agentId":"agent","messageId":"m2","request":{},"send":{"ops":[],"fail":"connection lost"}},
  {"label":"same message id, another agent","op":"send","instance":"w","dir":"receipts","agentId":"another","messageId":"m1","request":{"text":"hello"},"send":{"ops":[]}},
  {"label":"keys in another order","op":"send","instance":"w","dir":"receipts","agentId":"agent","messageId":"m3","request":{"z":1,"a":[{"y":1,"x":2}]},"send":{"ops":[]}},
  {"label":"delivered in another directory","op":"send","instance":"w","dir":"other","agentId":"agent","messageId":"m1","request":{"text":"hello"},"send":{"ops":[]}},
  {"label":"c1","op":"send","instance":"w","dir":"receipts","agentId":"agent","messageId":"c1","request":{},"send":{"ops":[]}},
  {"label":"c2","op":"send","instance":"w","dir":"receipts","agentId":"agent","messageId":"c2","request":{},"send":{"ops":[]}},
  {"label":"c3","op":"send","instance":"w","dir":"receipts","agentId":"agent","messageId":"c3","request":{},"send":{"ops":[]}},
  {"label":"c4","op":"send","instance":"w","dir":"receipts","agentId":"agent","messageId":"c4","request":{},"send":{"ops":[]}},
  {"label":"c5","op":"send","instance":"w","dir":"receipts","agentId":"agent","messageId":"c5","request":{},"send":{"ops":[]}},
  {"label":"c6","op":"send","instance":"w","dir":"receipts","agentId":"agent","messageId":"c6","request":{},"send":{"ops":[]}},
  {"label":"c7","op":"send","instance":"w","dir":"receipts","agentId":"agent","messageId":"c7","request":{},"send":{"ops":[]}},
  {"label":"c8","op":"send","instance":"w","dir":"receipts","agentId":"agent","messageId":"c8","request":{},"send":{"ops":[]}},
  {"label":"empty receipt","op":"write","path":{"receipt":["receipts","agent","c1"]},"text":""},
  {"label":"cut receipt","op":"write","path":{"receipt":["receipts","agent","c2"]},"text":"{\"fingerprint\":\"ab"},
  {"label":"garbage receipt","op":"write","path":{"receipt":["receipts","agent","c3"]},"text":"not a receipt"},
  {"label":"wrong shape receipt","op":"write","path":{"receipt":["receipts","agent","c4"]},"text":"{\"fingerprint\":1,\"state\":\"done\"}"},
  {"label":"array receipt","op":"write","path":{"receipt":["receipts","agent","c5"]},"text":"[]"},
  {"label":"null receipt","op":"write","path":{"receipt":["receipts","agent","c6"]},"text":"null"},
  {"label":"byte order mark receipt","op":"write","path":{"receipt":["receipts","agent","c7"]},"text":"﻿{}"},
  {"label":"invalid UTF-8 receipt","op":"write","path":{"receipt":["receipts","agent","c8"]},"hex":"7b22f09f98222c22ff223a317d"},
  {"label":"completed receipt by hand","op":"write","path":{"receipt":["receipts","agent","l1"]},"text":"{\"state\":\"completed\",\"extra\":1,\"agentId\":\"agent\",\"fingerprint\":\"@F\"}\n"},
  {"label":"pending receipt by hand","op":"write","path":{"receipt":["receipts","agent","l2"]},"text":"{\"fingerprint\":\"@F\",\"agentId\":\"agent\",\"state\":\"pending\"}"},
  {"label":"other fingerprint by hand","op":"write","path":{"receipt":["receipts","agent","l3"]},"text":"{\"fingerprint\":\"0000\",\"agentId\":\"agent\",\"state\":\"completed\"}"},
  {"label":"receipt path is a directory","op":"mkdir","path":{"receipt":["receipts","agent","d1"]}}
]"#;

/// The lookups run against a copy of an old home, each from a fresh
/// instance: the same request and a different one, receipts that are
/// pending, completed, damaged, or by hand, concurrent duplicates, and ids
/// the home has never seen.
const OLD_HOME_PROBES: &str = r#"[
  {"label":"delivered, same request","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"m1","request":{"10":1,"b":{"x":[{"c":2,"d":1}],"y":1},"2":2,"text":"hello"},"send":{"ops":[]}},
  {"label":"delivered, other request","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"m1","request":{"text":"other"},"send":{"ops":[]}},
  {"label":"concurrent duplicates of a delivered message","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"m1","request":{"text":"hello","10":1,"2":2,"b":{"y":1,"x":[{"d":1,"c":2}]}},"send":{"ops":[]},"count":2},
  {"label":"pending, same request","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"m2","request":{},"send":{"ops":[]}},
  {"label":"pending, other request","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"m2","request":{"x":1},"send":{"ops":[]}},
  {"label":"another agent, same message id","op":"send","instance":"p","dir":"receipts","agentId":"another","messageId":"m1","request":{"text":"hello"},"send":{"ops":[]}},
  {"label":"keys in another order","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"m3","request":{"a":[{"x":2,"y":1}],"z":1},"send":{"ops":[]}},
  {"label":"other directory","op":"send","instance":"p","dir":"other","agentId":"agent","messageId":"m1","request":{"text":"hello"},"send":{"ops":[]}},
  {"label":"empty receipt","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"c1","request":{},"send":{"ops":[]}},
  {"label":"cut receipt","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"c2","request":{},"send":{"ops":[]}},
  {"label":"garbage receipt","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"c3","request":{},"send":{"ops":[]}},
  {"label":"wrong shape receipt","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"c4","request":{},"send":{"ops":[]}},
  {"label":"array receipt","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"c5","request":{},"send":{"ops":[]}},
  {"label":"null receipt","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"c6","request":{},"send":{"ops":[]}},
  {"label":"byte order mark receipt","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"c7","request":{},"send":{"ops":[]}},
  {"label":"invalid UTF-8 receipt","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"c8","request":{},"send":{"ops":[]}},
  {"label":"completed receipt by hand","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"l1","request":{},"send":{"ops":[]}},
  {"label":"pending receipt by hand","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"l2","request":{},"send":{"ops":[]}},
  {"label":"other fingerprint by hand","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"l3","request":{},"send":{"ops":[]}},
  {"label":"receipt path is a directory","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"d1","request":{},"send":{"ops":[]}},
  {"label":"unseen message delivers","op":"send","instance":"p","dir":"receipts","agentId":"agent","messageId":"f1","request":{"fresh":true},"prepare":{"ops":[]},"send":{"ops":[]}},
  {"label":"unseen message again is a duplicate","op":"send","instance":"q","dir":"receipts","agentId":"agent","messageId":"f1","request":{"fresh":true},"send":{"ops":[]}},
  {"label":"unseen message in an unseen directory","op":"send","instance":"q","dir":"fresh/dir","agentId":"agent","messageId":"f1","request":{"fresh":true},"prepare":{"ops":[]},"send":{"ops":[]}}
]"#;

/// Copies the contents of `from` into the existing `to`, keeping modes.
fn copy_home(from: &str, to: &str) {
    let status = Command::new("cp")
        .args(["-Rp", &format!("{from}/."), to])
        .status()
        .expect("run cp");
    assert!(status.success(), "cp -Rp {from} {to}");
}

/// Runs the old-home probes against two copies of `home`, one with node and
/// one with the Rust port, and returns both outputs, normalized.
async fn probe_old_home(
    node: &std::ffi::OsStr,
    dist: &std::ffi::OsStr,
    home: &str,
) -> (String, String) {
    let probes = parse(OLD_HOME_PROBES).expect("probes are JSON");
    let (node_guard, node_root) = disposable_root("old-home-node");
    copy_home(home, &node_root);
    let expected = run_node(
        node,
        NODE_SCRIPT,
        &[dist, node_root.as_ref(), OLD_HOME_PROBES.as_ref()],
    );
    drop(node_guard);
    let (rust_guard, rust_root) = disposable_root("old-home-rust");
    copy_home(home, &rust_root);
    let actual = run_rust(&rust_root, &probes).await;
    drop(rust_guard);
    (
        normalize(&expected, &node_root),
        normalize(&actual, &rust_root),
    )
}

/// The probes did reach every outcome a loaded home can give, so agreeing
/// on them is not agreeing on nothing.
fn assert_probes_reach_every_outcome(output: &str) {
    for needle in [
        "\"ok\":true",
        "agent_request_key_conflict",
        "agent_request_outcome_unknown",
        "SyntaxError",
        "ZodError",
        "EISDIR",
    ] {
        assert!(output.contains(needle), "no probe produced {needle}");
    }
    let rows = parse(output).expect("rows");
    assert_eq!(
        rows.as_array().map(<[JsValue]>::len),
        parse(OLD_HOME_PROBES)
            .expect("probes")
            .as_array()
            .map(<[JsValue]>::len)
    );
}

#[tokio::test]
async fn a_home_written_by_node_loads_the_same_in_rust() {
    let Some((node, dist)) = pinned_inputs() else {
        return;
    };
    let steps = OLD_HOME_STEPS.replace("@F", EMPTY_REQUEST_FINGERPRINT);
    let (writer_guard, writer_root) = disposable_root("old-home-writer-node");
    run_node(
        &node,
        NODE_SCRIPT,
        &[dist.as_os_str(), writer_root.as_ref(), steps.as_ref()],
    );
    let (expected, actual) = probe_old_home(&node, &dist, &writer_root).await;
    drop(writer_guard);
    assert_probes_reach_every_outcome(&expected);
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn a_home_written_by_rust_loads_the_same_in_node() {
    let Some((node, dist)) = pinned_inputs() else {
        return;
    };
    let steps = OLD_HOME_STEPS.replace("@F", EMPTY_REQUEST_FINGERPRINT);
    let (node_guard, node_writer) = disposable_root("old-home-writer-node");
    let node_written = run_node(
        &node,
        NODE_SCRIPT,
        &[dist.as_os_str(), node_writer.as_ref(), steps.as_ref()],
    );
    let (rust_guard, rust_writer) = disposable_root("old-home-writer-rust");
    let rust_written = run_rust(&rust_writer, &parse(&steps).expect("steps")).await;
    // Both writers leave the same home, so the cross reads below start from
    // the same bytes.
    assert_eq!(
        normalize(&rust_written, &rust_writer),
        normalize(&node_written, &node_writer)
    );
    drop(node_guard);
    let (expected, actual) = probe_old_home(&node, &dist, &rust_writer).await;
    drop(rust_guard);
    assert_probes_reach_every_outcome(&expected);
    assert_eq!(actual, expected);
}

const ERRNO_SCRIPT: &str = r#"
const util = await import("node:util");
const rows = [];
for (let errno = -1; errno >= -200; errno--) {
  rows.push(`${util.getSystemErrorName(errno)}: ${util.getSystemErrorMessage(errno)}, open '/x'`);
}
process.stdout.write(JSON.stringify(rows));
"#;

#[test]
fn errno_names_and_descriptions_match_node() {
    let Some((node, _dist)) = pinned_inputs() else {
        return;
    };
    let expected = run_node(&node, ERRNO_SCRIPT, &[]);
    let rows = (1..=200)
        .map(|errno| {
            string(
                &FsError {
                    errno: -errno,
                    syscall: "open",
                    path: Some("/x".to_owned()),
                    dest: None,
                }
                .to_string(),
            )
        })
        .collect();
    assert_eq!(stringify(&JsValue::Array(rows)), expected);
}
