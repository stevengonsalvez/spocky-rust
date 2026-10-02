//! Differential check of `CreationService` against the pinned build's
//! `server/creation/index.js`: the same steps, run against a fresh home on
//! each side, must produce the same observer snapshots, callback calls,
//! results, errors, and `creations/` files (names, modes and bytes).
//!
//! Every id is fixed by the steps, so nothing is normalized. File listings
//! are sorted by name on both sides, as directory order is the file
//! system's. The legacy receipt's file name and fingerprint come from
//! [`digest`], so node also checks the Rust digest.
//!
//! Also checks that `creation_schema.rs` is what `creation-zod.mjs`
//! generates from the pinned `messages.js`.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use spocky_contracts::request::CreationKind;
use spocky_session::creation::{
    CreateAgent, CreationError, CreationFuture, CreationInput, CreationService, CreationTarget,
    Observer, OnReady, Provision, Provisioned, digest,
};
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

const WORKSPACE: &str = r#"{"id":"wks_00000000000000aa","projectId":"prj_1","projectDisplayName":"Repo","projectCustomName":null,"projectCustomIconRevision":null,"projectRootPath":"/r","workspaceDirectory":"/r/w","worktreeSlug":"w","projectKind":"git","workspaceKind":"worktree","name":"w","title":null,"pinnedAt":null,"labels":["a"],"archivingAt":null,"status":"done","statusEnteredAt":null,"activityAt":null,"diffStat":null,"scripts":[],"project":{"projectKey":"prj_1","projectName":"Repo","workspaceName":"w","checkout":{"cwd":"/r/w","currentBranch":"b","remoteUrl":null,"worktreeRoot":"/r/w","isGit":true,"isPaseoOwnedWorktree":true,"mainRepoRoot":"/r"}}}"#;

const AGENT: &str = r#"{"id":"agent-1","provider":"codex","cwd":"/w","workspaceId":"wks_00000000000000aa","model":"gpt","thinkingOptionId":"high","effectiveThinkingOptionId":"high","runtimeInfo":{"provider":"codex","sessionId":"s","model":"gpt"},"createdAt":"2023-11-14T22:13:20.000Z","updatedAt":"2023-11-14T22:13:22.000Z","lastUserMessageAt":"2023-11-14T22:13:21.500Z","status":"running","activeTurn":null,"capabilities":{"supportsStreaming":true,"supportsSessionPersistence":true,"supportsSessionListing":true,"supportsDynamicModes":false,"supportsMcpServers":true,"supportsReasoningStream":true,"supportsToolInvocations":true,"supportsRewindConversation":true,"supportsRewindFiles":false,"supportsRewindBoth":false},"currentModeId":"auto","availableModes":[{"id":"auto","label":"Auto","description":"d"}],"features":[{"id":"f","type":"toggle","label":"F","value":true}],"pendingPermissions":[],"persistence":{"provider":"codex","sessionId":"s","metadata":{"cwd":"/w"}},"title":"T","labels":{"a":"b"},"lastUsage":{"inputTokens":5,"outputTokens":2},"requiresAttention":false,"attentionReason":null,"attentionTimestamp":null}"#;

/// The steps, with `@WORKSPACE`, `@AGENT`, `@LEGACY_FILE`, `@LEGACY2_FILE`
/// and `@LEGACY_FINGERPRINT` filled in by [`steps`].
///
/// `mark` remembers the receipt files; `corrupt` rewrites the one receipt
/// (`.json` file) added since the last `mark`, with `text` or with its first `replace[0]`
/// swapped for `replace[1]`. The next `create` then reads a corrupt or
/// schema-invalid receipt, and its error text must match node's `ZodError`
/// message.
const STEPS: &str = r#"[
  {"op":"create","kind":"workspace","key":"ws-1","request":{"cwd":"/r","b":1,"a":{"10":1,"2":2}},"workspaceId":"wks_00000000000000aa","hasAgent":false,"hasPrompt":false,"exists":[],
   "provision":{"workspace":@WORKSPACE,"setupSkippedReason":"no_setup"}},
  {"op":"files"},
  {"op":"create","kind":"workspace","key":"ws-1","request":{"a":{"2":2,"10":1},"b":1,"cwd":"/r"},"workspaceId":"wks_00000000000000aa","hasAgent":false,"hasPrompt":false,"exists":[],
   "provision":{"workspace":@WORKSPACE}},
  {"op":"create","kind":"workspace","key":"ws-1","request":{"cwd":"/other"},"hasAgent":false,"hasPrompt":false,"exists":[]},
  {"op":"create","kind":"agent","key":"ag-1","request":{"provider":"codex","prompt":"hi"},"workspaceId":"wks_00000000000000aa","agentId":"agent-1","hasAgent":true,"hasPrompt":true,"exists":[],
   "createAgent":{"ready":@AGENT,"result":@AGENT}},
  {"op":"files"},
  {"op":"create","kind":"agent","key":"ag-1","request":{"provider":"codex","prompt":"hi"},"agentId":"agent-1","hasAgent":true,"hasPrompt":true,"exists":[]},
  {"op":"create","kind":"agent","key":"ag-2","request":{"provider":"codex"},"workspaceId":"wks_00000000000000aa","agentId":"agent-2","hasAgent":true,"hasPrompt":false,"exists":[],
   "createAgent":{"ready":null,"result":@AGENT}},
  {"op":"concurrent","creates":[
    {"kind":"agent","key":"ag-3","request":{"n":3},"workspaceId":"wks_00000000000000aa","agentId":"shared","hasAgent":true,"hasPrompt":false,"exists":[],"createAgent":{"ready":null,"result":@AGENT}},
    {"kind":"agent","key":"ag-4","request":{"n":4},"workspaceId":"wks_00000000000000aa","agentId":"shared","hasAgent":true,"hasPrompt":false,"exists":[],"createAgent":{"ready":null,"result":@AGENT}}
  ]},
  {"op":"create","kind":"agent","key":"ag-5","request":{"n":5},"workspaceId":"wks_00000000000000aa","agentId":"taken","hasAgent":true,"hasPrompt":false,"exists":["agent:taken"]},
  {"op":"files"},
  {"op":"create","kind":"workspace","key":"ws-2","request":{"cwd":"/missing"},"workspaceId":"wks_00000000000000bb","hasAgent":false,"hasPrompt":false,"exists":[],
   "provision":{"throw":{"message":"Directory does not exist","code":"directory_not_found"}}},
  {"op":"files"},
  {"op":"create","kind":"workspace","key":"ws-2","request":{"cwd":"/missing"},"workspaceId":"wks_00000000000000bb","hasAgent":false,"hasPrompt":false,"exists":[],
   "provision":{"workspace":@WORKSPACE}},
  {"op":"files"},
  {"op":"create","kind":"workspace","key":"ws-3","request":{"cwd":"/half"},"workspaceId":"wks_00000000000000cc","hasAgent":false,"hasPrompt":false,"exists":[],
   "provision":{"throw":{"message":"git worktree add failed"}}},
  {"op":"create","kind":"agent","key":"ag-6","request":{"n":6},"workspaceId":"wks_00000000000000aa","agentId":"agent-6","hasAgent":true,"hasPrompt":true,"exists":[],
   "createAgent":{"ready":@AGENT,"throw":{"message":"prompt send failed"}}},
  {"op":"subscribe","kind":"agent","key":"ag-6"},
  {"op":"subscribe","kind":"agent","key":"ag-6"},
  {"op":"subscribe","kind":"workspace","key":"nope"},
  {"op":"create","kind":"agent","key":"ag-6","request":{"n":6},"agentId":"agent-6","hasAgent":true,"hasPrompt":true,"exists":[]},
  {"op":"create","kind":"agent","key":"ag-7","request":{"n":7},"workspaceId":"wks_00000000000000aa","agentId":"agent-7","hasAgent":true,"hasPrompt":false,"exists":["agent:agent-7"],
   "createAgent":{"ready":null,"throw":{"message":"spawn failed","code":"ENOENT"}}},
  {"op":"files"},
  {"op":"writeLegacy","file":"@LEGACY_FILE","text":"{\"fingerprint\":\"@LEGACY_FINGERPRINT\",\"state\":\"completed\",\"agentId\":\"legacy-agent\",\"extra\":1}"},
  {"op":"create","kind":"agent","key":"legacy-1","request":{"provider":"codex","cwd":"/w"},"hasAgent":true,"hasPrompt":false,"exists":[],"readAgent":@AGENT},
  {"op":"files"},
  {"op":"mark"},
  {"op":"create","kind":"workspace","key":"rc-1","request":{"cwd":"/rc1"},"workspaceId":"wks_00000000000000d1","hasAgent":false,"hasPrompt":false,"exists":[],
   "provision":{"workspace":@WORKSPACE}},
  {"op":"corrupt","text":"{\"fingerprint\":"},
  {"op":"create","kind":"workspace","key":"rc-1","request":{"cwd":"/rc1"},"workspaceId":"wks_00000000000000d1","hasAgent":false,"hasPrompt":false,"exists":[]},
  {"op":"mark"},
  {"op":"create","kind":"workspace","key":"rc-2","request":{"cwd":"/rc2"},"workspaceId":"wks_00000000000000d2","hasAgent":false,"hasPrompt":false,"exists":[],
   "provision":{"workspace":@WORKSPACE}},
  {"op":"corrupt","text":"[]"},
  {"op":"create","kind":"workspace","key":"rc-2","request":{"cwd":"/rc2"},"workspaceId":"wks_00000000000000d2","hasAgent":false,"hasPrompt":false,"exists":[]},
  {"op":"mark"},
  {"op":"create","kind":"workspace","key":"rc-3","request":{"cwd":"/rc3"},"workspaceId":"wks_00000000000000d3","hasAgent":false,"hasPrompt":false,"exists":[],
   "provision":{"workspace":@WORKSPACE}},
  {"op":"corrupt","text":"{\"fingerprint\":1,\"snapshot\":{},\"inFlight\":\"x\"}"},
  {"op":"create","kind":"workspace","key":"rc-3","request":{"cwd":"/rc3"},"workspaceId":"wks_00000000000000d3","hasAgent":false,"hasPrompt":false,"exists":[]},
  {"op":"mark"},
  {"op":"create","kind":"workspace","key":"rc-4","request":{"cwd":"/rc4"},"workspaceId":"wks_00000000000000d4","hasAgent":false,"hasPrompt":false,"exists":[],
   "provision":{"workspace":@WORKSPACE}},
  {"op":"corrupt","replace":["\"phase\": \"completed\"","\"phase\": \"bogus\""]},
  {"op":"create","kind":"workspace","key":"rc-4","request":{"cwd":"/rc4"},"workspaceId":"wks_00000000000000d4","hasAgent":false,"hasPrompt":false,"exists":[]},
  {"op":"mark"},
  {"op":"create","kind":"workspace","key":"rc-5","request":{"cwd":"/rc5"},"workspaceId":"wks_00000000000000d5","hasAgent":false,"hasPrompt":false,"exists":[],
   "provision":{"workspace":@WORKSPACE}},
  {"op":"corrupt","replace":["\"fingerprint\"","\"fingerprinX\""]},
  {"op":"create","kind":"workspace","key":"rc-5","request":{"cwd":"/rc5"},"workspaceId":"wks_00000000000000d5","hasAgent":false,"hasPrompt":false,"exists":[]},
  {"op":"mark"},
  {"op":"create","kind":"workspace","key":"rc-6","request":{"cwd":"/rc6"},"workspaceId":"wks_00000000000000d6","hasAgent":false,"hasPrompt":false,"exists":[],
   "provision":{"workspace":@WORKSPACE}},
  {"op":"corrupt","replace":["\"workspaceDirectory\": \"/r/w\"","\"workspaceDirectory\": 5"]},
  {"op":"create","kind":"workspace","key":"rc-6","request":{"cwd":"/rc6"},"workspaceId":"wks_00000000000000d6","hasAgent":false,"hasPrompt":false,"exists":[]},
  {"op":"mark"},
  {"op":"create","kind":"workspace","key":"rc-7","request":{"cwd":"/rc7"},"workspaceId":"wks_00000000000000d7","hasAgent":false,"hasPrompt":false,"exists":[],
   "provision":{"workspace":@WORKSPACE}},
  {"op":"corrupt","replace":["\"inFlight\": null","\"inFlight\": 5"]},
  {"op":"create","kind":"workspace","key":"rc-7","request":{"cwd":"/rc7"},"workspaceId":"wks_00000000000000d7","hasAgent":false,"hasPrompt":false,"exists":[]},
  {"op":"files"},
  {"op":"writeLegacy","file":"@LEGACY2_FILE","text":"{\"fingerprint\":1,\"state\":\"x\"}"},
  {"op":"create","kind":"agent","key":"legacy-2","request":{"provider":"codex","cwd":"/w"},"hasAgent":true,"hasPrompt":false,"exists":[],"readAgent":@AGENT}
]"#;

fn steps() -> String {
    let legacy_request = parse(r#"{"type":"create_agent_request","provider":"codex","cwd":"/w"}"#)
        .expect("legacy request");
    STEPS
        .replace("@WORKSPACE", WORKSPACE)
        .replace("@AGENT", AGENT)
        .replace(
            "@LEGACY_FILE",
            &format!(
                "{}.json",
                digest(&parse(r#"["create","legacy-1"]"#).expect("key"))
            ),
        )
        .replace(
            "@LEGACY2_FILE",
            &format!(
                "{}.json",
                digest(&parse(r#"["create","legacy-2"]"#).expect("key"))
            ),
        )
        .replace("@LEGACY_FINGERPRINT", &digest(&legacy_request))
}

const NODE_SCRIPT: &str = r#"
const [dist, stepsJson, home] = process.argv.slice(1);
if (process.version !== "v22.20.0") {
  throw new Error(`node ${process.version} is not the pinned v22.20.0`);
}
const fs = await import("node:fs");
const path = await import("node:path");
const { CreationService } = await import(`${dist}/server/creation/index.js`);
const logger = { warn() {}, child() { return this; } };
const service = new CreationService(
  path.join(home, "creations"),
  logger,
  undefined,
  path.join(home, "agent-requests"),
);
const thrown = (spec) => {
  const error = new Error(spec.message);
  if (spec.code) error.code = spec.code;
  return error;
};
const input = (spec, events) => {
  const base = {
    key: spec.key,
    request: spec.request,
    hasAgent: spec.hasAgent,
    hasPrompt: spec.hasPrompt,
    exists: async (kind, id) => {
      events.push(`exists ${kind} ${id}`);
      return spec.exists.includes(`${kind}:${id}`);
    },
  };
  if (spec.workspaceId !== undefined) base.workspaceId = spec.workspaceId;
  if (spec.agentId !== undefined) base.agentId = spec.agentId;
  if (spec.provision) {
    base.provision = async (workspaceId) => {
      events.push(`provision ${workspaceId}`);
      if (spec.provision.throw) throw thrown(spec.provision.throw);
      return spec.provision.setupSkippedReason === undefined
        ? { workspace: spec.provision.workspace }
        : { workspace: spec.provision.workspace, setupSkippedReason: spec.provision.setupSkippedReason };
    };
  }
  if (spec.createAgent) {
    base.createAgent = async (agentId, workspace, onReady) => {
      events.push(`createAgent ${agentId} ${JSON.stringify(workspace ?? null)}`);
      if (spec.createAgent.ready) await onReady(spec.createAgent.ready);
      if (spec.createAgent.throw) throw thrown(spec.createAgent.throw);
      return spec.createAgent.result;
    };
  }
  if (spec.kind === "workspace") return { ...base, kind: "workspace" };
  return {
    ...base,
    kind: "agent",
    readAgent: async (id) => {
      events.push(`readAgent ${id}`);
      return spec.readAgent ?? null;
    },
  };
};
const observer = (events) => (snapshot) => events.push(`snapshot ${JSON.stringify(snapshot)}`);
const settle = async (promise, events) => {
  try {
    return { events, result: await promise };
  } catch (error) {
    return { events, error: { message: error.message, code: error.code ?? null } };
  }
};
const files = () => {
  const directory = path.join(home, "creations");
  return fs.readdirSync(directory).sort().map((name) => {
    const file = path.join(directory, name);
    return [name, (fs.statSync(file).mode & 0o777).toString(8), fs.readFileSync(file, "utf8")];
  });
};
const out = [];
let marked = new Set();
for (const step of JSON.parse(stepsJson)) {
  if (step.op === "create") {
    const events = [];
    out.push(await settle(service.create(input(step, events), observer(events)), events));
  } else if (step.op === "concurrent") {
    const runs = step.creates.map((spec) => {
      const events = [];
      return settle(service.create(input(spec, events), observer(events)), events);
    });
    for (const run of runs) out.push(await run);
  } else if (step.op === "subscribe") {
    const events = [];
    const { snapshot, unsubscribe } = await service.subscribe(step.kind, step.key, observer(events));
    unsubscribe();
    out.push({ events, snapshot });
  } else if (step.op === "files") {
    out.push({ files: files() });
  } else if (step.op === "mark") {
    marked = new Set(fs.readdirSync(path.join(home, "creations")));
  } else if (step.op === "corrupt") {
    const directory = path.join(home, "creations");
    const added = fs.readdirSync(directory).filter((entry) => !marked.has(entry) && entry.endsWith(".json"));
    if (added.length !== 1) throw new Error(`receipts added since the mark: ${JSON.stringify(added)}`);
    const file = path.join(directory, added[0]);
    const before = fs.readFileSync(file, "utf8");
    const after = step.text !== undefined ? step.text : before.replace(step.replace[0], step.replace[1]);
    if (after === before) throw new Error(`corrupt left ${added[0]} unchanged`);
    fs.writeFileSync(file, after);
  } else if (step.op === "writeLegacy") {
    fs.mkdirSync(path.join(home, "agent-requests"), { recursive: true });
    fs.writeFileSync(path.join(home, "agent-requests", step.file), step.text);
  }
}
process.stdout.write(JSON.stringify(out));
"#;

type Events = Arc<Mutex<Vec<String>>>;

fn push(events: &Events, line: String) {
    events.lock().expect("events").push(line);
}

fn text(value: Option<&JsValue>) -> Option<String> {
    value.and_then(JsValue::as_str).map(str::to_owned)
}

fn thrown(spec: &JsValue) -> CreationError {
    CreationError {
        message: text(spec.get("message")).expect("message"),
        code: text(spec.get("code")),
    }
}

fn input(spec: &JsValue, events: &Events) -> CreationInput {
    let exists_ids: Vec<String> = spec
        .get("exists")
        .and_then(JsValue::as_array)
        .expect("exists")
        .iter()
        .filter_map(|id| id.as_str().map(str::to_owned))
        .collect();
    let exists_events = Arc::clone(events);
    let provision: Option<Provision> = spec.get("provision").cloned().map(|provision| {
        let events = Arc::clone(events);
        Box::new(move |workspace_id: Option<String>| {
            push(
                &events,
                format!("provision {}", workspace_id.as_deref().unwrap_or("null")),
            );
            let result = match provision.get("throw") {
                Some(error) => Err(thrown(error)),
                None => Ok(Provisioned {
                    workspace: provision.get("workspace").cloned().expect("workspace"),
                    setup_skipped_reason: text(provision.get("setupSkippedReason")),
                }),
            };
            let future: CreationFuture<Provisioned> = Box::pin(async move { result });
            future
        }) as Provision
    });
    let create_agent: Option<CreateAgent> = spec.get("createAgent").cloned().map(|create| {
        let events = Arc::clone(events);
        Box::new(
            move |agent_id: Option<String>, workspace: Option<JsValue>, on_ready: OnReady| {
                push(
                    &events,
                    format!(
                        "createAgent {} {}",
                        agent_id.as_deref().unwrap_or("null"),
                        stringify(&workspace.unwrap_or(JsValue::Null))
                    ),
                );
                let future: CreationFuture<JsValue> = Box::pin(async move {
                    if let Some(ready) = create
                        .get("ready")
                        .filter(|ready| !matches!(ready, JsValue::Null))
                    {
                        on_ready(ready.clone()).await?;
                    }
                    if let Some(error) = create.get("throw") {
                        return Err(thrown(error));
                    }
                    Ok(create.get("result").cloned().expect("result"))
                });
                future
            },
        ) as CreateAgent
    });
    let target = if text(spec.get("kind")).as_deref() == Some("workspace") {
        CreationTarget::Workspace
    } else {
        let read_events = Arc::clone(events);
        let agent = spec.get("readAgent").cloned();
        CreationTarget::Agent {
            read_agent: Box::new(move |id| {
                push(&read_events, format!("readAgent {id}"));
                Box::pin(async move { Ok(agent) })
            }),
        }
    };
    CreationInput {
        target,
        key: text(spec.get("key")).expect("key"),
        request: spec.get("request").cloned().expect("request"),
        workspace_id: text(spec.get("workspaceId")),
        agent_id: text(spec.get("agentId")),
        has_agent: matches!(spec.get("hasAgent"), Some(JsValue::Bool(true))),
        has_prompt: matches!(spec.get("hasPrompt"), Some(JsValue::Bool(true))),
        exists: Arc::new(move |kind, id| {
            let name = match kind {
                CreationKind::Workspace => "workspace",
                CreationKind::Agent => "agent",
            };
            push(&exists_events, format!("exists {name} {id}"));
            let found = exists_ids.contains(&format!("{name}:{id}"));
            Box::pin(async move { Ok(found) })
        }),
        provision,
        create_agent,
    }
}

fn observer(events: &Events) -> Observer {
    let events = Arc::clone(events);
    Arc::new(move |snapshot: &JsValue| push(&events, format!("snapshot {}", stringify(snapshot))))
}

fn events_value(events: &Events) -> JsValue {
    JsValue::Array(
        events
            .lock()
            .expect("events")
            .iter()
            .map(|line| JsValue::String(line.clone()))
            .collect(),
    )
}

fn settled(result: Result<JsValue, CreationError>, events: &Events) -> JsValue {
    let mut out = JsObject::new();
    out.insert("events", events_value(events));
    match result {
        Ok(snapshot) => out.insert("result", snapshot),
        Err(error) => {
            let mut thrown = JsObject::new();
            thrown.insert("message", JsValue::String(error.message));
            thrown.insert("code", error.code.map_or(JsValue::Null, JsValue::String));
            out.insert("error", JsValue::Object(thrown));
        }
    }
    JsValue::Object(out)
}

fn files(home: &Path) -> JsValue {
    let directory = home.join("creations");
    let mut names: Vec<String> = std::fs::read_dir(&directory)
        .expect("creations")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    let rows = names
        .into_iter()
        .map(|name| {
            let file = directory.join(&name);
            let mode = std::fs::metadata(&file).expect("stat").permissions().mode() & 0o777;
            JsValue::Array(vec![
                JsValue::String(name),
                JsValue::String(format!("{mode:o}")),
                JsValue::String(std::fs::read_to_string(&file).expect("read")),
            ])
        })
        .collect();
    let mut out = JsObject::new();
    out.insert("files", JsValue::Array(rows));
    JsValue::Object(out)
}

async fn rust_output(home: &Path) -> String {
    let service = CreationService::new(home, None);
    let mut out = Vec::new();
    let mut marked = std::collections::BTreeSet::new();
    for step in parse(&steps()).expect("steps").as_array().expect("steps") {
        let events: Events = Arc::default();
        match text(step.get("op")).as_deref() {
            Some("create") => {
                let result = service
                    .create(input(step, &events), Some(observer(&events)))
                    .await;
                out.push(settled(result, &events));
            }
            Some("concurrent") => {
                let runs: Vec<_> = step
                    .get("creates")
                    .and_then(JsValue::as_array)
                    .expect("creates")
                    .iter()
                    .map(|spec| {
                        let events: Events = Arc::default();
                        (
                            service.create(input(spec, &events), Some(observer(&events))),
                            events,
                        )
                    })
                    .collect();
                for (run, events) in runs {
                    out.push(settled(run.await, &events));
                }
            }
            Some("subscribe") => {
                let kind = if text(step.get("kind")).as_deref() == Some("workspace") {
                    CreationKind::Workspace
                } else {
                    CreationKind::Agent
                };
                let (snapshot, guard) = service
                    .subscribe(
                        kind,
                        &text(step.get("key")).expect("key"),
                        observer(&events),
                    )
                    .await
                    .expect("subscribe");
                drop(guard);
                let mut row = JsObject::new();
                row.insert("events", events_value(&events));
                row.insert("snapshot", snapshot.unwrap_or(JsValue::Null));
                out.push(JsValue::Object(row));
            }
            Some("files") => out.push(files(home)),
            Some("mark") => marked = receipt_names(home),
            Some("corrupt") => {
                let directory = home.join("creations");
                let added: Vec<_> = receipt_names(home)
                    .difference(&marked)
                    .filter(|name| {
                        Path::new(name)
                            .extension()
                            .is_some_and(|extension| extension == "json")
                    })
                    .cloned()
                    .collect();
                assert_eq!(added.len(), 1, "receipts added since the mark");
                let file = directory.join(&added[0]);
                let before = std::fs::read_to_string(&file).expect("receipt");
                let after = if let Some(replacement) = text(step.get("text")) {
                    replacement
                } else {
                    let swap = step
                        .get("replace")
                        .and_then(JsValue::as_array)
                        .expect("replace");
                    before.replacen(
                        swap[0].as_str().expect("from"),
                        swap[1].as_str().expect("to"),
                        1,
                    )
                };
                assert_ne!(after, before, "corrupt left {} unchanged", file.display());
                std::fs::write(&file, after).expect("corrupt receipt");
            }
            Some("writeLegacy") => {
                let directory = home.join("agent-requests");
                std::fs::create_dir_all(&directory).expect("legacy directory");
                std::fs::write(
                    directory.join(text(step.get("file")).expect("file")),
                    text(step.get("text")).expect("text"),
                )
                .expect("legacy receipt");
            }
            other => panic!("unknown step {other:?}"),
        }
    }
    stringify(&JsValue::Array(out))
}

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/creation/index.js",
        "678d1801975b6abc00613eed000ef539c5cfe9731cbfe8f1be36432c1c4064b4",
    ),
    (
        "server/atomic-file.js",
        "835d68e580f2d1d5ae344559bf6eca4829ede020ea2416e8e8e2115303121c25",
    ),
];

fn assert_pinned_modules(dist: &std::ffi::OsStr) {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    for (path, expected) in PINNED_MODULES {
        let bytes = std::fs::read(Path::new(dist).join(path)).expect("pinned module");
        let actual = Sha256::digest(&bytes)
            .iter()
            .fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            });
        assert_eq!(&actual, expected, "{path} is not the pinned build");
    }
}

fn timeout_command() -> &'static str {
    if Command::new("gtimeout").arg("--version").output().is_ok() {
        "gtimeout"
    } else {
        "timeout"
    }
}

fn pinned() -> Option<(std::ffi::OsString, std::ffi::OsString)> {
    match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => Some((node, dist)),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: creation differential not run");
            None
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creation_service_matches_pinned_build() {
    let Some((node, dist)) = pinned() else {
        return;
    };
    assert_pinned_modules(&dist);
    let node_home = tempfile_home("node");
    let rust_home = tempfile_home("rust");
    let output = Command::new(timeout_command())
        .args(["--kill-after=5", "120"])
        .arg(&node)
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .arg(&dist)
        .arg(steps())
        .arg(&node_home)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rust = rust_output(&rust_home).await;
    std::fs::remove_dir_all(&node_home).expect("remove node home");
    std::fs::remove_dir_all(&rust_home).expect("remove rust home");
    assert_eq!(rust, String::from_utf8_lossy(&output.stdout));
}

#[test]
fn creation_schema_is_generated_from_the_pinned_messages() {
    let Some((node, dist)) = pinned() else {
        return;
    };
    // `SPOCKY_PASEO_DIST` is `<runtime>/packages/server/dist/server`.
    let runtime = Path::new(&dist).join("../../../..");
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/phase3/creation-zod.mjs");
    let output = Command::new(timeout_command())
        .args(["--kill-after=5", "120"])
        .arg(&node)
        .arg(script)
        .arg("--runtime")
        .arg(runtime)
        .arg("--check")
        .output()
        .expect("run creation-zod");
    assert!(
        output.status.success(),
        "creation-zod --check failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The names of the files in `home/creations`.
fn receipt_names(home: &Path) -> std::collections::BTreeSet<String> {
    std::fs::read_dir(home.join("creations"))
        .expect("creations directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

/// A fresh disposable home under the system temp directory.
fn tempfile_home(side: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!(
        "spocky-creation-{side}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&home).expect("home");
    home
}
