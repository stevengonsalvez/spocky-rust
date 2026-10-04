//! Differential check of the agent manager against the pinned build's
//! `AgentManager`: the same scripted fake providers drive both, and the
//! outcomes, subscriber feeds (agent payloads and stream events), provider
//! calls, timelines, run and wait results, and stored records must match.
//!
//! Scenarios:
//! - `main`: `createAgent`, a completed `runAgent`, and `waitForAgentEvent`.
//! - `errors`: the `createAgent` rejections (working directory, unknown,
//!   unavailable and disabled providers, provider options, tool policy, MCP
//!   support, agent ids) and the unknown-agent errors.
//! - `turns`: a failed turn, a canceled turn, coalescing edges, an idle
//!   `waitForActive`, and aborted waits on a held turn.
//! - `permission`: a wait that finishes on `permission_requested`, then a
//!   wait that returns the pending permission at once.
//!
//! - `lifecycle`: a permission answered by `respondToPermission`, a turn
//!   canceled by `cancelAgentRun` (twice), a cancel before the turn has
//!   started, and `closeAgent` (twice).
//!
//! - `subagents`: provider sub-agent events during a turn, the four
//!   sub-agent queries, and `closeAgent` canceling a running child.
//!
//! - `hydration`: `hydrateTimelineFromProvider` from the provider's
//!   history: already primed (a no-op), forced with and without broadcast,
//!   and an unknown agent.
//!
//! - `resume`: `resumeAgentFromPersistence` priming from history, an
//!   archived agent resumed for history without its working directory,
//!   and the id, client and availability errors.
//!
//! - `titles`: `setTitle` (trimmed, blank, unknown agent) and
//!   `hasInFlightRun` idle, running and unknown.
//!
//! - `runstart`: `waitForAgentRunStart` on an unknown agent, with no
//!   pending run, through a slow start, a failed start, an abort mid-wait
//!   and before the wait, and after the run finished.
//!
//! - `outofband`: `tryRunOutOfBand` declined, a `/goal` command with a
//!   client message id whose handler emits a timeline item and a usage
//!   event, a failing handler, and an unknown agent.
//!
//! - `loading`: `ensureAgentLoaded` on a second manager over the first's
//!   storage: two concurrent loads sharing one resume (one asking for the
//!   timeline broadcast), a record without a persistence handle taking the
//!   create path, an unavailable provider, and a missing record.
//!
//! - `replace`: `replaceAgentRun` on an idle agent, on a running one (its run
//!   is interrupted, then the prompt streams), on one whose run finishes by
//!   itself while the replacement waits, with a run still starting, and with
//!   a cancellation that is never acknowledged (the replacement fails, and
//!   its mark is cleared when the held run later ends).
//! - `import`: `importProviderSession` through a provider that imports
//!   (timeline rows renumbered without the system-injected message, the
//!   first user message as the title, provider sub-agent events replayed),
//!   a provider without `importSession`, an unknown provider, and an
//!   imported config that fails normalization (its session is closed).
//!
//! - `archive`: `archiveAgent` on a parent whose children are archived
//!   with it (live and stored-only), detached (another workspace, an open
//!   tab) or left alone; `unarchiveSnapshot` with a workspace and label
//!   patch, twice and for an unknown agent; `detachAgent`; and, on a fresh
//!   home, `archiveSnapshot` of a closed agent then `unarchiveSnapshotByHandle`.
//!
//! - `shutdown`: `flush` does not wait for an in-flight `createAgent`
//!   (its `createSession` held 150 ms), `flushForShutdown` does, and a
//!   registration after `prepareForShutdown` is refused.
//!
//! A scripted `{"type":"__delay","ms":N}` entry pauses the fake's emission
//! and is never emitted; a leading `{"type":"__startDelay","ms":N}` holds
//! `startTurn` that long before it resolves, and a leading
//! `{"type":"__startFail","ms":N,"message":M}` makes it reject with `M`
//! after `N` ms.
//!
//! Normalized: wall-clock ISO timestamps (`<ISO>`) and random UUIDs such as
//! timeline epochs (`<UUID:n>`, numbered by first appearance so that a reused
//! id and two distinct ids stay different), nothing else. The fixed agent ids
//! stay as they are.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly). `wait_releases_its_subscription` runs
//! without them.

use std::collections::VecDeque;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

use spocky_contracts::js::{date_parse, js_string, spread, spread_into};
use spocky_session::agent_loading::{EnsureAgentLoadedDeps, ensure_agent_loaded};
use spocky_session::agent_manager::{
    AgentArchivedCallback, AgentManager, AgentManagerEvent, AgentManagerOptions,
    AgentMetadataUpdates, AgentSteerOptions, AppendedTimelineItem, AttentionCallback,
    CreateAgentOptions, HydrateBroadcast, HydrateTimelineOptions, ImportProviderSessionRequest,
    ImportablePersistedAgentQueryOptions, ImportableSessionProviderError, NoPluginLifecycle,
    PaseoToolCatalogFactory, PaseoToolRuntimeContext, PluginLifecycle, ProviderDefinition,
    ProviderRegistryUpdate, ReloadAgentOptions, ResumeAgentOptions, SteerDispatch,
    SubscribeOptions, TurnEventStream, UnarchiveUpdates, WaitForAgentOptions,
};
use spocky_session::agent_projection::{AgentAttention, to_agent_payload};
use spocky_session::agent_sdk::{
    AbortController, AbortReason, AbortSignal, AgentClient, AgentCreateSessionOptions, AgentError,
    AgentEventStream, AgentLaunchContext, AgentPromptInput, AgentResult, AgentResumePurpose,
    AgentResumeSessionOptions, AgentRunOptions, AgentSession, AgentStreamEvent, BoxFuture,
    FetchCatalogOptions, ImportProviderSessionContext, ImportProviderSessionInput,
    ImportableProviderSession, ImportedProviderSession, ImportedTimelineEntry,
    ListImportableSessionsOptions, OutOfBandHandler, PaseoToolCatalog, PaseoToolDefinition,
    PaseoToolExecutionContext, ProviderRefreshContext, SteerResult, StreamCallback, Unsubscribe,
};
use spocky_session::agent_storage::AgentStorage;
use spocky_session::rewind::RewindMode;
use spocky_session::timeline::FetchDirection;
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

const AGENT_ID: &str = "00000000-0000-4000-8000-0000000000a1";
const OTHER_ID: &str = "00000000-0000-4000-8000-0000000000b2";
const UNKNOWN_ID: &str = "00000000-0000-4000-8000-0000000000ff";

/// The ids the scenarios choose; [`normalize`] keeps them.
const CHILD_SAME_ID: &str = "00000000-0000-4000-8000-0000000000e1";
const CHILD_OTHER_WORKSPACE_ID: &str = "00000000-0000-4000-8000-0000000000e2";
const CHILD_TAB_ID: &str = "00000000-0000-4000-8000-0000000000e3";
const CHILD_STORED_ID: &str = "00000000-0000-4000-8000-0000000000e4";
const FIXED_IDS: [&str; 7] = [
    AGENT_ID,
    OTHER_ID,
    UNKNOWN_ID,
    CHILD_SAME_ID,
    CHILD_OTHER_WORKSPACE_ID,
    CHILD_TAB_ID,
    CHILD_STORED_ID,
];

/// What the fake session emits after `startTurn` resolves when its script
/// has no turn left.
const TURN_EVENTS: &str = r#"[
  {"type":"thread_started","provider":"fake","sessionId":"sess-1"},
  {"type":"turn_started","provider":"fake","turnId":"turn-1"},
  {"type":"timeline","provider":"fake","turnId":"turn-1","item":{"type":"user_message","text":"hello","clientMessageId":"client-1","messageId":"provider-msg-1"}},
  {"type":"timeline","provider":"fake","turnId":"turn-1","item":{"type":"reasoning","text":"thinking "}},
  {"type":"timeline","provider":"fake","turnId":"turn-1","item":{"type":"reasoning","text":"more"}},
  {"type":"timeline","provider":"fake","turnId":"turn-1","item":{"type":"tool_call","callId":"call-1","name":"shell","status":"running","error":null,"detail":{"type":"shell","command":"ls"}}},
  {"type":"timeline","provider":"fake","turnId":"turn-1","item":{"type":"tool_call","callId":"call-1","name":"shell","status":"completed","error":null,"detail":{"type":"shell","command":"ls","output":"a\nb","exitCode":0}}},
  {"type":"timeline","provider":"fake","turnId":"turn-1","item":{"type":"assistant_message","text":"Hel"}},
  {"type":"timeline","provider":"fake","turnId":"turn-1","item":{"type":"assistant_message","text":"lo!"}},
  {"type":"usage_updated","provider":"fake","turnId":"turn-1","usage":{"inputTokens":3,"outputTokens":5}},
  {"type":"turn_completed","provider":"fake","turnId":"turn-1","usage":{"inputTokens":3,"outputTokens":5,"totalCostUsd":0.25}}
]"#;

/// The `turns` scenario's scripted turns, in `startTurn` order.
const SCENARIO_TURNS: &str = r#"{
  "failed": [
    {"type":"turn_started","provider":"fake","turnId":"turn-2"},
    {"type":"timeline","provider":"fake","turnId":"turn-2","item":{"type":"assistant_message","text":"partial"}},
    {"type":"turn_failed","provider":"fake","turnId":"turn-2","error":" boom ","code":"E_FAKE","diagnostic":"stack trace"}
  ],
  "canceled": [
    {"type":"turn_started","provider":"fake","turnId":"turn-3"},
    {"type":"timeline","provider":"fake","turnId":"turn-3","item":{"type":"assistant_message","text":"half"}},
    {"type":"turn_canceled","provider":"fake","turnId":"turn-3","reason":"user stopped"}
  ],
  "coalesce": [
    {"type":"turn_started","provider":"fake","turnId":"turn-4"},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"assistant_message","text":"A"}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"assistant_message","text":""}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"reasoning","text":"R1"}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"reasoning","text":"R2"}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"assistant_message","text":"B"}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"tool_call","callId":"call-2","name":"shell","status":"running","error":null,"detail":{"type":"shell","command":"pwd"}}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"tool_call","callId":"call-3","name":"shell","status":"running","error":null,"detail":{"type":"shell","command":"id"}}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"tool_call","callId":"call-2","name":"shell","status":"completed","error":null,"detail":{"type":"shell","command":"pwd","output":"/","exitCode":0}}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"assistant_message","text":"C"}},
    {"type":"usage_updated","provider":"fake","turnId":"turn-4","usage":{"inputTokens":1}},
    {"type":"timeline","provider":"fake","turnId":"turn-4","item":{"type":"assistant_message","text":"D"}},
    {"type":"turn_completed","provider":"fake","turnId":"turn-4"}
  ],
  "held": [
    {"type":"turn_started","provider":"fake","turnId":"turn-5"}
  ],
  "ask": [
    {"type":"turn_started","provider":"fake","turnId":"turn-7"},
    {"type":"permission_requested","provider":"fake","turnId":"turn-7","request":{"id":"perm-1","provider":"fake","name":"shell","kind":"tool","input":{"command":"rm x","empty":{}},"actions":[{"id":"allow","label":"Allow","behavior":"allow"}]}}
  ],
  "response": [
    {"type":"permission_resolved","provider":"fake","turnId":"turn-7","requestId":"perm-1","resolution":{"behavior":"allow"}},
    {"type":"timeline","provider":"fake","turnId":"turn-7","item":{"type":"assistant_message","text":"Removed."}},
    {"type":"turn_completed","provider":"fake","turnId":"turn-7"}
  ],
  "long": [
    {"type":"turn_started","provider":"fake","turnId":"turn-8"},
    {"type":"timeline","provider":"fake","turnId":"turn-8","item":{"type":"reasoning","text":"long task"}}
  ],
  "interrupt": [
    {"type":"turn_canceled","provider":"fake","turnId":"turn-8","reason":"interrupted by user"}
  ],
  "startSlowDone": [
    {"type":"__startDelay","ms":150},
    {"type":"turn_started","provider":"fake","turnId":"turn-11"},
    {"type":"turn_completed","provider":"fake","turnId":"turn-11"}
  ],
  "startFail": [
    {"type":"__startFail","ms":50,"message":"spawn failed"}
  ],
  "startSlowAbort": [
    {"type":"__startDelay","ms":300},
    {"type":"turn_started","provider":"fake","turnId":"turn-12"},
    {"type":"turn_completed","provider":"fake","turnId":"turn-12"}
  ],
  "slowStart": [
    {"type":"__startDelay","ms":300},
    {"type":"turn_started","provider":"fake","turnId":"turn-9"}
  ],
  "subagents": [
    {"type":"turn_started","provider":"fake","turnId":"turn-10"},
    {"type":"provider_subagent","provider":"fake","event":{"type":"upsert","id":"child-1","title":"Explore","cwd":"/w/child","status":"running","timestamp":"2026-07-12T10:00:00.000Z"}},
    {"type":"provider_subagent","provider":"fake","event":{"type":"timeline","id":"child-1","item":{"type":"assistant_message","text":"Found it."},"timestamp":"2026-07-12T10:00:01.000Z"}},
    {"type":"provider_subagent","provider":"fake","event":{"type":"upsert","id":"child-2","title":"Review","status":"completed","timestamp":"2026-07-12T09:00:00.000Z"}},
    {"type":"timeline","provider":"fake","turnId":"turn-10","item":{"type":"assistant_message","text":"Delegated."}},
    {"type":"turn_completed","provider":"fake","turnId":"turn-10"}
  ],
  "history": [
    {"type":"timeline","provider":"fake","item":{"type":"user_message","text":"<paseo-system>\ninjected\n</paseo-system>"}},
    {"type":"timeline","provider":"fake","item":{"type":"user_message","text":"old question","messageId":"m-1"},"timestamp":"2026-07-12T08:00:00.000Z"},
    {"type":"usage_updated","provider":"fake","usage":{"inputTokens":9}},
    {"type":"provider_subagent","provider":"fake","event":{"type":"upsert","id":"child-h","title":"From history","status":"completed","timestamp":"2026-07-12T08:00:01.000Z"}},
    {"type":"timeline","provider":"fake","item":{"type":"assistant_message","text":"old answer"},"timestamp":""},
    {"type":"timeline","provider":"fake","item":{"type":"tool_call","callId":"h-1","name":"shell","status":"completed","error":null,"detail":{"type":"shell","command":"ls","output":"a"}}}
  ],
  "impTurn": [
    {"type":"turn_started","provider":"fake","turnId":"turn-18"},
    {"type":"timeline","provider":"fake","turnId":"turn-18","item":{"type":"assistant_message","text":"after import"}},
    {"type":"turn_completed","provider":"fake","turnId":"turn-18"}
  ],
  "rwEcho": [
    {"type":"turn_started","provider":"fake","turnId":"turn-19"},
    {"type":"timeline","provider":"fake","turnId":"turn-19","item":{"type":"user_message","text":"rewind me","clientMessageId":"client-rw","messageId":"provider-rw"}},
    {"type":"timeline","provider":"fake","turnId":"turn-19","item":{"type":"assistant_message","text":"ok"}},
    {"type":"turn_completed","provider":"fake","turnId":"turn-19"}
  ],
  "rwNoEcho": [
    {"type":"turn_started","provider":"fake","turnId":"turn-20"},
    {"type":"timeline","provider":"fake","turnId":"turn-20","item":{"type":"assistant_message","text":"noted"}},
    {"type":"turn_completed","provider":"fake","turnId":"turn-20"}
  ],
  "badItem": [
    {"type":"turn_started","provider":"fake","turnId":"turn-22"},
    {"type":"timeline","provider":"fake","turnId":"turn-22","item":{"type":"tool_call","callId":"x","name":"shell","status":"running","error":null}},
    {"type":"turn_completed","provider":"fake","turnId":"turn-22"}
  ],
  "settingsCases": [
    {"name":"full","spec":{"settable":true,"modeNotice":{"type":"notice","text":"mode changed"},"thinkingNotice":{"type":"notice","text":"thinking changed"}},
     "ops":[["mode","read-only"],["model"," model-b "],["model","  "],["model","model-a"],["model",null],["thinking","high"],["thinking","   "],["feature","fast",true],["feature","effort","max"],["feature","fast",false]]},
    {"name":"nullmode","spec":{"settable":true,"currentModeNull":true},"ops":[["mode","fallback"]]},
    {"name":"runtimethinking","spec":{"settable":true,"runtimeInfoExtra":{"thinkingOptionId":"low"}},"ops":[["thinking","high"]]},
    {"name":"runtimenull","spec":{"settable":true,"runtimeInfoExtra":{"thinkingOptionId":null}},"ops":[["thinking","high"]]},
    {"name":"plain","spec":{},"ops":[["mode","auto"],["model","m"],["thinking","t"],["feature","f",1]]}
  ],
  "steerCases": [
    {"name":"idle","turns":[],"spec":{"steer":{"result":"accepted"}},"running":false,"ops":[["steer","hello",null],["steerOrReplace","hello",null]]},
    {"name":"accepted","turns":["long"],"spec":{"steer":{"result":"accepted","emit":[{"type":"timeline","provider":"fake","turnId":"turn-8","item":{"type":"tool_call","callId":"s1","name":"shell","status":"completed","error":null,"detail":{"type":"shell","command":"ls","output":"a"}}}]}},"running":true,"ops":[["steer","steer me",{"clientMessageId":"steer-1","clearPendingPermissions":true}],["steerOrReplace","again",null]]},
    {"name":"unavailable","turns":["long","rpAfter"],"spec":{"steer":{"result":"unavailable"}},"running":true,"ops":[["steer","nope",null],["steerOrReplace","replace",null]]},
    {"name":"unsupported","turns":["long","rpAfter"],"spec":{},"running":true,"ops":[["steer","nope",null],["steerOrReplace","replace",null]]},
    {"name":"changed","turns":["long"],"spec":{"steer":{"result":"unavailable","emit":[{"type":"turn_canceled","provider":"fake","turnId":"turn-8","reason":"interrupted by user"},{"type":"turn_started","provider":"fake","turnId":"turn-30"}]}},"running":true,"cancel":false,"ops":[["steer","too late",null]]},
    {"name":"race","turns":["long","rpAfter"],"spec":{"steer":{"result":"unavailable"}},"running":true,"hookRace":true,"ops":[["steerOrReplace","replace",null]]}
  ],
  "spontaneousPermission": [
    {"type":"permission_requested","provider":"fake","request":{"id":"perm-9","provider":"fake","name":"shell","kind":"tool","input":{"command":"ls"},"actions":[{"id":"allow","label":"Allow","behavior":"allow"}]}}
  ],
  "rpIdle": [
    {"type":"turn_started","provider":"fake","turnId":"turn-13"},
    {"type":"timeline","provider":"fake","turnId":"turn-13","item":{"type":"assistant_message","text":"idle replace"}},
    {"type":"turn_completed","provider":"fake","turnId":"turn-13"}
  ],
  "rpAfter": [
    {"type":"turn_started","provider":"fake","turnId":"turn-15"},
    {"type":"timeline","provider":"fake","turnId":"turn-15","item":{"type":"assistant_message","text":"replaced"}},
    {"type":"turn_completed","provider":"fake","turnId":"turn-15"}
  ],
  "rpFinishing": [
    {"type":"turn_started","provider":"fake","turnId":"turn-14"},
    {"type":"timeline","provider":"fake","turnId":"turn-14","item":{"type":"assistant_message","text":"finishing"}},
    {"type":"__delay","ms":150},
    {"type":"turn_completed","provider":"fake","turnId":"turn-14"}
  ],
  "rpAfterFinish": [
    {"type":"turn_started","provider":"fake","turnId":"turn-16"},
    {"type":"timeline","provider":"fake","turnId":"turn-16","item":{"type":"assistant_message","text":"after finish"}},
    {"type":"turn_completed","provider":"fake","turnId":"turn-16"}
  ],
  "rpHeld": [
    {"type":"turn_started","provider":"fake","turnId":"turn-17"},
    {"type":"__delay","ms":400},
    {"type":"turn_completed","provider":"fake","turnId":"turn-17"}
  ],
  "import": {
    "config": {"provider":"fake","cwd":"$CWD","title":"  "},
    "persistence": {"provider":"fake","sessionId":"imp-1","metadata":{"cwd":"$CWD"}},
    "timeline": [
      {"item":{"type":"user_message","text":"<paseo-system>\nnoise\n</paseo-system>"}},
      {"item":{"type":"user_message","text":"  first question  ","messageId":"m1"},"timestamp":"2026-07-12T08:00:00.000Z"},
      {"item":{"type":"assistant_message","text":"answer"},"timestamp":"2026-07-12T08:00:01.000Z"},
      {"item":{"type":"tool_call","callId":"i-1","name":"shell","status":"completed","error":null,"detail":{"type":"shell","command":"ls","output":"a"}}}
    ],
    "providerSubagentEvents": [
      {"provider":"fake","event":{"type":"upsert","id":"child-i","title":"Imported child","status":"completed","timestamp":"2026-07-12T08:00:02.000Z"}}
    ]
  },
  "badTimeline": {
    "config": {"provider":"badtimeline","cwd":"$CWD"},
    "persistence": {"provider":"badtimeline","sessionId":"imp-3"},
    "timeline": [{"item":{"type":"tool_call","callId":"t","name":"shell","status":"running","error":null}}]
  },
  "badSubagent": {
    "config": {"provider":"badsubagent","cwd":"$CWD"},
    "persistence": {"provider":"badsubagent","sessionId":"imp-4"},
    "timeline": [],
    "providerSubagentEvents": [
      {"provider":"badsubagent","event":{"type":"timeline","id":"child-b","item":{"type":"tool_call","callId":"x","name":"shell","status":"running","error":null},"timestamp":"2026-07-12T08:00:03.000Z"}}
    ]
  },
  "importable": {
    "alpha": {"sessions": [
      {"providerHandleId":"a-1","cwd":"/work/Alpha-App","title":"Fix login","firstPromptPreview":"fix the login bug","lastPromptPreview":null,"lastActivityAt":1700000300000},
      {"providerHandleId":"a-2","cwd":"C:\\work\\Beta","title":null,"firstPromptPreview":null,"lastPromptPreview":"Ship it","lastActivityAt":1700000500000},
      {"providerHandleId":"a-3","cwd":"/work/gamma/","title":"","firstPromptPreview":"Gamma notes","lastPromptPreview":"","lastActivityAt":1700000100000},
      {"providerHandleId":"a-4","cwd":"/work/twin","title":"Tie twin","firstPromptPreview":null,"lastPromptPreview":null,"lastActivityAt":1700000300000}
    ]},
    "slow": {"delayMs": 120, "sessions": [
      {"providerHandleId":"s-1","cwd":"/work/slow","title":"Slow one","firstPromptPreview":null,"lastPromptPreview":null,"lastActivityAt":1700000300000}
    ]},
    "beta": {"sessions": [
      {"providerHandleId":"b-1","cwd":"/work/beta","title":"Beta tie","firstPromptPreview":null,"lastPromptPreview":null,"lastActivityAt":1700000300000},
      {"providerHandleId":"b-2","cwd":"/work/other","title":"Login again","firstPromptPreview":null,"lastPromptPreview":null,"lastActivityAt":1700000400000}
    ]},
    "failing": {"fail": "listing broke"},
    "slowfail": {"delayMs": 80, "fail": "slow listing broke"},
    "nocap": {"sessions": [
      {"providerHandleId":"n-1","cwd":"/work/nocap","title":"Hidden","firstPromptPreview":null,"lastPromptPreview":null,"lastActivityAt":1700000900000}
    ]},
    "off": {"sessions": [
      {"providerHandleId":"o-1","cwd":"/work/off","title":"Disabled","firstPromptPreview":null,"lastPromptPreview":null,"lastActivityAt":1700000900000}
    ]}
  },
  "badImport": {
    "config": {"provider":"badimport","cwd":"/nonexistent/spocky-import"},
    "persistence": {"provider":"badimport","sessionId":"imp-2"},
    "timeline": []
  },
  "permission": [
    {"type":"turn_started","provider":"fake","turnId":"turn-6"},
    {"type":"__delay","ms":100},
    {"type":"permission_requested","provider":"fake","turnId":"turn-6","request":{"id":"perm-1","provider":"fake","name":"shell","kind":"tool","title":"Run rm","input":{"command":"rm -rf build"},"actions":[{"id":"allow","label":"Allow","behavior":"allow"},{"id":"deny","label":"Deny","behavior":"deny"}]}}
  ]
}"#;

/// `[config, agentId or null]` for each `createAgent` of the `errors`
/// scenario; `$CWD` stands for the disposable working directory.
const ERROR_CASES: &str = r#"[
  [{"provider":"fake","cwd":"/nonexistent/spocky-cwd"}, null],
  [{"provider":"fake","cwd":"/etc/hosts"}, null],
  [{"provider":"nope","cwd":"$CWD"}, null],
  [{"provider":"gone","cwd":"$CWD"}, null],
  [{"provider":"broken","cwd":"$CWD"}, null],
  [{"provider":"off","cwd":"$CWD"}, null],
  [{"provider":"fake","cwd":"$CWD","providerOptions":{"a":1}}, null],
  [{"provider":"fake","cwd":"$CWD","toolPolicy":{"preapproved":[]}}, null],
  [{"provider":"tools","cwd":"$CWD","toolPolicy":{"preapproved":[{"kind":"mcp","server":"s","tool":"t"}]}}, null],
  [{"provider":"tools","cwd":"$CWD","toolPolicy":{"preapproved":[null]}}, null],
  [{"provider":"tools","cwd":"$CWD","mcpServers":{"s":{"type":"stdio","command":"x"}},"toolPolicy":{"preapproved":[{"kind":"mcp","server":"s","tool":"t"}]}}, "00000000-0000-4000-8000-0000000000b2"],
  [{"provider":"nomcp","cwd":"$CWD","mcpServers":{"m":{"type":"stdio","command":"x"}}}, null],
  [{"provider":"fake","cwd":"$CWD"}, "not-a-uuid"],
  [{"provider":"fake","cwd":"$CWD"}, "00000000-0000-4000-8000-0000000000a1"],
  [{"provider":"fake","cwd":"$CWD"}, "00000000-0000-4000-8000-0000000000a1"]
]"#;

const RUNTIME_INFO: &str =
    r#"{"provider":"fake","sessionId":"sess-1","model":"model-default","modeId":"auto"}"#;
const PERSISTENCE: &str =
    r#"{"provider":"fake","sessionId":"sess-1","nativeHandle":"thread-1","metadata":{"x":1}}"#;
const CAPABILITIES: &str = r#"{"supportsStreaming":true,"supportsSessionPersistence":true,"supportsDynamicModes":false,"supportsMcpServers":true,"supportsReasoningStream":true,"supportsToolInvocations":true}"#;
const REWIND_CAPABILITIES: &str = r#"{"supportsStreaming":true,"supportsSessionPersistence":true,"supportsDynamicModes":false,"supportsMcpServers":true,"supportsReasoningStream":true,"supportsToolInvocations":true,"supportsRewindConversation":true,"supportsRewindFiles":true,"supportsRewindBoth":true}"#;
const NO_MCP_CAPABILITIES: &str = r#"{"supportsStreaming":true,"supportsSessionPersistence":true,"supportsDynamicModes":false,"supportsMcpServers":false,"supportsReasoningStream":true,"supportsToolInvocations":true}"#;
const MODES: &str = r#"[{"id":"auto","label":"Auto"},{"id":"read-only","label":"Read only"}]"#;
const CATALOG: &str = r#"{"models":[{"provider":"fake","id":"model-a","label":"A"},{"provider":"fake","id":"model-default","label":"D","isDefault":true}],"modes":[]}"#;

const NODE_SCRIPT: &str = r#"
const [dist, agentId, otherId, unknownId, turnEventsJson, scenarioTurnsJson, errorCasesJson, runtimeInfoJson, persistenceJson, capabilitiesJson, modesJson, catalogJson, cwd, home] = process.argv.slice(1);
if (process.version !== "v22.20.0") {
  throw new Error(`node ${process.version} is not the pinned v22.20.0`);
}
const { AgentManager } = await import(`${dist}/server/agent/agent-manager.js`);
const { AgentStorage } = await import(`${dist}/server/agent/agent-storage.js`);
const { toAgentPayload } = await import(`${dist}/server/agent/agent-projections.js`);
const fs = await import("node:fs");
// pino's child logger: its bindings come first in every line the child logs.
const withChild = (base, bindings) => Object.fromEntries([
  ["child", (more) => withChild(base, { ...bindings, ...more })],
  ...["trace", "debug", "info", "warn", "error"].map((level) => [level, (merged, ...rest) => base[level](merged !== null && typeof merged === "object" ? { ...bindings, ...merged } : merged, ...rest)]),
]);
const logger = { child(bindings) { return withChild(this, bindings); }, trace() {}, debug() {}, info() {}, warn() {}, error() {} };
// pino's default `err` serializer, as the pinned logger applies it to an `err` binding.
// The stack is dropped: its frames are node source locations no Rust error has.
const { validateBeforeRequest } = await import(`${dist}/server/plugins/lifecycle/index.js`);
const { default: pinoStd } = await import(`${dist}/../../../../node_modules/pino-std-serializers/index.js`);
const serializeLogErr = (bindings) => {
  if (!bindings || typeof bindings !== "object" || !("err" in bindings)) return bindings;
  const { stack: _stack, ...err } = { ...pinoStd.err(bindings.err) };
  return { ...bindings, err };
};
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const turnIdOf = (events) => events.find((event) => event.type === "turn_started")?.turnId ?? "turn-1";
class FakeSession {
  constructor(spec, calls) { this.provider = spec.provider; this.id = "sess-1"; this.capabilities = spec.capabilities; this.spec = spec; this.calls = calls; this.listeners = []; if (spec.initialTimeline) this.initialTimeline = spec.initialTimeline;
    if (spec.steer) {
      this.steerActiveTurn = async (prompt, options) => {
        this.calls.push(["steerActiveTurn", prompt, options]);
        for (const event of spec.steer.emit ?? []) for (const listener of this.listeners) listener(event);
        await sleep(20);
        return { status: spec.steer.result };
      };
    }
    if (spec.sessionCommands) this.listCommands = async () => { this.calls.push(["session.listCommands"]); return spec.sessionCommands; };
    if (spec.sessionFeatures !== undefined) this.features = spec.sessionFeatures;
    if (spec.settable) {
      this.setModel = async (modelId) => { this.calls.push(["setModel", modelId]); this.model = modelId; };
      this.setThinkingOption = async (optionId) => { this.calls.push(["setThinkingOption", optionId]); return this.spec.thinkingNotice; };
      this.setFeature = async (featureId, value) => { this.calls.push(["setFeature", featureId, value]); };
    }
    for (const [kind, method] of [["conversation", "revertConversation"], ["files", "revertFiles"], ["both", "revertBoth"]]) {
      if ((spec.revert ?? []).includes(kind)) this[method] = async ({ messageId }) => { this.calls.push([method, messageId]); };
    } }
  subscribe(callback) { this.listeners.push(callback); return () => {}; }
  emitLater(events, ms) {
    setTimeout(async () => {
      for (const event of events) {
        if (event.type === "__delay") { await sleep(event.ms); continue; }
        if (event.type === "__startDelay") continue;
        for (const l of this.listeners) l(event);
      }
    }, ms);
  }
  async startTurn(prompt, options) {
    this.calls.push(["startTurn", prompt, options ?? null]);
    const events = this.spec.turns.shift() ?? JSON.parse(turnEventsJson);
    if (events[0]?.type === "__startFail") { await sleep(events[0].ms); throw new Error(events[0].message); }
    this.emitLater(events, 20);
    if (events[0]?.type === "__startDelay") await sleep(events[0].ms);
    return { turnId: turnIdOf(events) };
  }
  async run() { throw new Error("unused"); }
  tryHandleOutOfBand(prompt) {
    this.calls.push(["tryHandleOutOfBand", prompt]);
    if (typeof prompt !== "string" || !prompt.startsWith("/goal")) return null;
    return {
      run: async ({ emit }) => {
        if (prompt === "/goal fail") throw new Error("goal broke");
        emit({ type: "timeline", provider: "fake", item: { type: "assistant_message", text: "Goal paused." } });
        emit({ type: "usage_updated", provider: "fake", usage: { inputTokens: 1 } });
      },
    };
  }
  async *streamHistory() {
    for (const event of this.spec.history ?? []) yield event;
    if (this.spec.historyFails) throw new Error("history broke");
  }
  async getRuntimeInfo() { return { ...JSON.parse(runtimeInfoJson), ...(this.spec.runtimeInfoExtra ?? {}) }; }
  async getAvailableModes() { return JSON.parse(modesJson); }
  async getCurrentMode() { return this.spec.currentModeNull ? null : "auto"; }
  async setMode(modeId) { this.calls.push(["setMode", modeId]); return this.spec.modeNotice; }
  getPendingPermissions() { return []; }
  async respondToPermission(requestId, response) {
    this.calls.push(["respondToPermission", requestId, response]);
    if (this.spec.response) this.emitLater(this.spec.response, 200);
  }
  describePersistence() {
    if (this.spec.noPersistence) return null;
    const handle = JSON.parse(persistenceJson);
    return this.model === undefined ? handle : { ...handle, nativeHandle: `model-${this.model}` };
  }
  async interrupt() {
    this.calls.push(["interrupt"]);
    if (this.spec.interruptFails) throw new Error("interrupt failed");
    if (this.spec.interruptLateFailMs) { await sleep(this.spec.interruptLateFailMs); throw new Error("interrupt failed late"); }
    if (this.spec.interruptHang) await new Promise(() => {});
    if (this.spec.interrupt) this.emitLater(this.spec.interrupt, 10);
  }
  async close() {
    this.calls.push(["close"]);
    if (this.spec.closeFails) throw new Error("close failed");
    if (this.spec.closeHangs) await new Promise(() => {});
  }
}
const spec = (provider, overrides = {}) => ({ provider, capabilities: JSON.parse(capabilitiesJson), available: true, turns: [], ...overrides });
const fakeClient = (calls, spec) => ({
  provider: spec.provider,
  capabilities: spec.capabilities,
  async createSession(config, launchContext, options) {
    calls.push(["createSession", config, launchContext ?? null, options ?? null]);
    if (spec.createDelay) await sleep(spec.createDelay);
    return new FakeSession(spec, calls);
  },
  async resumeSession(handle, overrides, launchContext, options) {
    calls.push(["resumeSession", handle, overrides ?? null, launchContext ?? null, options ?? null]);
    if (spec.resumeFails) throw new Error("resume failed");
    return new FakeSession(spec, calls);
  },
  ...(spec.import ? {
    async importSession(input, context) {
      calls.push(["importSession", input, { config: context.config, storedConfig: context.storedConfig, launchContext: context.launchContext ?? null }]);
      const imported = JSON.parse(JSON.stringify(spec.import).replaceAll("$CWD", cwd));
      return { session: new FakeSession(spec, calls), config: imported.config, persistence: imported.persistence, timeline: imported.timeline, providerSubagentEvents: imported.providerSubagentEvents };
    },
  } : {}),
  ...(spec.clientCommands ? { async listCommands(config) { calls.push(["listCommands", config]); return spec.clientCommands; } } : {}),
  ...(spec.clientFeatures ? { async listFeatures(config) { calls.push(["listFeatures", config]); return spec.clientFeatures; } } : {}),
  ...(spec.importable ? {
    async listImportableSessions(options) {
      calls.push(["listImportableSessions", spec.provider, options ?? null]);
      if (spec.importable.delayMs) await sleep(spec.importable.delayMs);
      if (spec.importable.fail) throw new Error(spec.importable.fail);
      return (spec.importable.sessions ?? []).map((session) => ({ ...session, lastActivityAt: new Date(session.lastActivityAt) }));
    },
  } : {}),
  async archiveNativeSession(handle) {
    calls.push(["archiveNativeSession", handle]);
    if (spec.archiveFails) throw new Error("native archive failed");
  },
  async unarchiveNativeSession(handle) { calls.push(["unarchiveNativeSession", handle]); },
  async fetchCatalog(options, context) { calls.push(["fetchCatalog", options, context === undefined ? "no context" : "context"]); return JSON.parse(catalogJson); },
  async isAvailable() {
    if (spec.availableDelayMs) await sleep(spec.availableDelayMs);
    if (typeof spec.available === "boolean") return spec.available;
    throw new Error(spec.available);
  },
});
const recordFeed = (manager) => {
  const feed = [];
  manager.subscribe((event) => {
    if (event.type === "agent_state") feed.push(["agent_state", toAgentPayload(event.agent)]);
    else if (event.type === "agent_stream") feed.push(["agent_stream", event.agentId, event.event, event.seq ?? null, event.epoch ?? null, event.timestamp ?? null]);
    else if (event.type === "provider_subagent") feed.push(["provider_subagent", event.event]);
    else feed.push([event.type]);
  });
  return feed;
};
const outcome = async (run) => {
  try {
    return { ok: await run() };
  } catch (error) {
    return { name: error.name, message: error.message };
  }
};
const waitResult = (wait) => ({ status: wait.status, permission: wait.permission, lastMessage: wait.lastMessage });

const main = async () => {
  const calls = [];
  const registry = new AgentStorage(`${home}/main`, logger);
  const manager = new AgentManager({ logger, registry, clients: { fake: fakeClient(calls, spec("fake")) }, providerDefinitions: { fake: { enabled: true } } });
  const feed = recordFeed(manager);
  const created = toAgentPayload(await manager.createAgent(
    { provider: "fake", cwd, title: "  Fake title  ", model: " default " },
    agentId,
    { workspaceId: "wks_1", labels: { surface: "workspace" }, env: { EXTRA: "1" } },
  ));
  const run = await manager.runAgent(agentId, "hello", { clientMessageId: "client-1" });
  const wait = await manager.waitForAgentEvent(agentId);
  await sleep(100);
  await manager.flush();
  await registry.flush();
  const directory = `${home}/main/${fs.readdirSync(`${home}/main`)[0]}`;
  const record = JSON.parse(fs.readFileSync(`${directory}/${agentId}.json`, "utf8"));
  return {
    calls,
    created,
    run,
    wait: waitResult(wait),
    rows: await manager.getTimelineRows(agentId),
    fetch: manager.fetchTimeline(agentId, { direction: "tail", limit: 3 }),
    feed,
    record,
  };
};

const errors = async () => {
  const calls = [];
  const registry = new AgentStorage(`${home}/errors`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: {
      fake: fakeClient(calls, spec("fake")),
      gone: fakeClient(calls, spec("gone", { available: false })),
      broken: fakeClient(calls, spec("broken", { available: "missing binary" })),
      off: fakeClient(calls, spec("off")),
      tools: fakeClient(calls, spec("tools")),
      nomcp: fakeClient(calls, spec("nomcp", { capabilities: { ...JSON.parse(capabilitiesJson), supportsMcpServers: false } })),
    },
    providerDefinitions: {
      fake: { enabled: true },
      gone: { enabled: true },
      broken: { enabled: true },
      off: { enabled: false },
      tools: { enabled: true, applyToolPolicy: (config, toolPolicy) => ({ ...config, toolPolicy }) },
      nomcp: { enabled: true },
    },
  });
  const results = [];
  for (const [config, id] of JSON.parse(errorCasesJson.replaceAll("$CWD", cwd))) {
    results.push(await outcome(async () => (await manager.createAgent(config, id ?? undefined, {})).id));
  }
  results.push(await outcome(() => manager.runAgent(unknownId, "hi")));
  results.push(await outcome(() => manager.waitForAgentEvent(unknownId)));
  results.push(await outcome(async () => { manager.subscribe(() => {}, { agentId: "bad" }); return null; }));
  await manager.flush();
  await registry.flush();
  return { results, calls };
};

const turns = async () => {
  const calls = [];
  const scripted = JSON.parse(scenarioTurnsJson);
  const registry = new AgentStorage(`${home}/turns`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: { fake: fakeClient(calls, spec("fake", { turns: [scripted.failed, scripted.canceled, scripted.coalesce, scripted.held] })) },
    providerDefinitions: { fake: { enabled: true } },
  });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  const run = (text) => outcome(() => manager.runAgent(agentId, text));
  const wait = (options) => outcome(async () => waitResult(await manager.waitForAgentEvent(agentId, options)));
  const results = [];
  results.push(await run("fail"));
  results.push(await wait());
  results.push(await run("cancel"));
  results.push(await wait());
  results.push(await run("coalesce"));
  results.push(await wait({ waitForActive: true }));
  manager.runAgent(agentId, "hold").catch(() => {});
  const started = (entry) => entry[0] === "agent_stream" && entry[2].type === "turn_started" && entry[2].turnId === "turn-5";
  for (let tick = 0; !feed.some(started); tick += 1) {
    if (tick === 2000) throw new Error("turn-5 never started");
    await sleep(5);
  }
  const pre = new AbortController();
  pre.abort("pre");
  results.push(await wait({ signal: pre.signal }));
  const stop = new AbortController();
  setTimeout(() => stop.abort("stop"), 30);
  results.push(await wait({ signal: stop.signal }));
  const timeout = new AbortController();
  setTimeout(() => timeout.abort(new Error("wait timeout")), 30);
  results.push(await wait({ signal: timeout.signal }));
  const plain = new AbortController();
  plain.abort();
  results.push(await wait({ signal: plain.signal }));
  results.push(await run("again"));
  await sleep(100);
  await manager.flush();
  await registry.flush();
  return { results, calls, feed, rows: await manager.getTimelineRows(agentId), stored: await registry.get(agentId) };
};

const permission = async () => {
  const calls = [];
  const scripted = JSON.parse(scenarioTurnsJson);
  const registry = new AgentStorage(`${home}/permission`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: { fake: fakeClient(calls, spec("fake", { turns: [scripted.permission] })) },
    providerDefinitions: { fake: { enabled: true } },
  });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  manager.runAgent(agentId, "ask").catch(() => {});
  const started = (entry) => entry[0] === "agent_stream" && entry[2].type === "turn_started" && entry[2].turnId === "turn-6";
  for (let tick = 0; !feed.some(started); tick += 1) {
    if (tick === 2000) throw new Error("turn-6 never started");
    await sleep(5);
  }
  const wait = () => outcome(async () => waitResult(await manager.waitForAgentEvent(agentId)));
  const results = [await wait(), await wait()];
  await sleep(100);
  await manager.flush();
  await registry.flush();
  return { results, calls, feed, rows: await manager.getTimelineRows(agentId) };
};

const lifecycle = async () => {
  const calls = [];
  const scripted = JSON.parse(scenarioTurnsJson);
  const registry = new AgentStorage(`${home}/lifecycle`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: { fake: fakeClient(calls, spec("fake", { turns: [scripted.ask, scripted.long, scripted.slowStart], response: scripted.response, interrupt: scripted.interrupt })) },
    providerDefinitions: { fake: { enabled: true } },
  });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  const collect = async (stream, events) => { for await (const event of stream) events.push(event); return events; };
  const second = manager.streamAgent(agentId, "remove x");
  const secondEvents = [(await second.next()).value];
  const permissionWait = waitResult(await manager.waitForAgentEvent(agentId));
  const pending = manager.getPendingPermissions(agentId);
  const respond = await manager.respondToPermission(agentId, "perm-1", { behavior: "allow" });
  await collect(second, secondEvents);
  await sleep(100);
  const third = manager.streamAgent(agentId, "long task");
  const thirdEvents = [(await third.next()).value];
  await sleep(50);
  const cancel = (await manager.cancelAgentRun(agentId)).status;
  await collect(third, thirdEvents);
  const cancelAgain = (await manager.cancelAgentRun(agentId)).status;
  await sleep(100);
  const starting = outcome(() => manager.runAgent(agentId, "slow"));
  await sleep(100);
  const cancelStarting = (await manager.cancelAgentRun(agentId)).status;
  const startingResult = await starting;
  await sleep(400);
  const rows = await manager.getTimelineRows(agentId);
  const fetch = manager.fetchTimeline(agentId, { direction: "tail", limit: 3 });
  await manager.closeAgent(agentId);
  await manager.closeAgent(agentId);
  await sleep(100);
  await manager.flush();
  await registry.flush();
  const directory = `${home}/lifecycle/${fs.readdirSync(`${home}/lifecycle`)[0]}`;
  const record = JSON.parse(fs.readFileSync(`${directory}/${agentId}.json`, "utf8"));
  return { secondEvents, permissionWait, pending, respond: respond ?? null, thirdEvents, cancel, cancelAgain, cancelStarting, startingResult, rows, fetch, calls, feed, record };
};

const subagents = async () => {
  const calls = [];
  const scripted = JSON.parse(scenarioTurnsJson);
  const registry = new AgentStorage(`${home}/subagents`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: { fake: fakeClient(calls, spec("fake", { turns: [scripted.subagents] })) },
    providerDefinitions: { fake: { enabled: true } },
  });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  const run = await outcome(() => manager.runAgent(agentId, "delegate"));
  await sleep(100);
  const queries = [
    await outcome(async () => manager.listProviderSubagents(agentId)),
    await outcome(async () => manager.listProviderSubagentActivity()),
    await outcome(async () => manager.getProviderSubagent(agentId, "child-1")),
    await outcome(async () => manager.getProviderSubagent(agentId, "nope")),
    await outcome(async () => manager.fetchProviderSubagentTimeline(agentId, "child-1", { direction: "tail", limit: 5 })),
    await outcome(async () => manager.listProviderSubagents(unknownId)),
  ];
  await manager.closeAgent(agentId);
  await sleep(100);
  await manager.flush();
  await registry.flush();
  return { run, queries, after: manager.listProviderSubagentActivity(), calls, feed };
};

const hydration = async () => {
  const calls = [];
  const scripted = JSON.parse(scenarioTurnsJson);
  const registry = new AgentStorage(`${home}/hydration`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: { fake: fakeClient(calls, spec("fake", { history: scripted.history })) },
    providerDefinitions: { fake: { enabled: true } },
  });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  const steps = [];
  const step = async (options) => {
    steps.push(await outcome(async () => { await manager.hydrateTimelineFromProvider(agentId, options); return null; }));
    steps.push(await manager.getTimelineRows(agentId));
  };
  await step(undefined);
  await step({ force: true, broadcast: true });
  await step({ force: true, broadcast: true, broadcastTimeline: false });
  await step({ force: true });
  steps.push(await outcome(async () => { await manager.hydrateTimelineFromProvider(unknownId); return null; }));
  await sleep(100);
  await manager.flush();
  await registry.flush();
  return { steps, subagents: manager.listProviderSubagents(agentId), feed };
};

const resume = async () => {
  const calls = [];
  const scripted = JSON.parse(scenarioTurnsJson);
  const registry = new AgentStorage(`${home}/resume`, logger);
  const fakeSpec = spec("fake", { history: scripted.history, initialTimeline: [{ item: { type: "assistant_message", text: "startup row" }, timestamp: "2026-07-12T07:00:00.000Z" }] });
  const manager = new AgentManager({
    logger,
    registry,
    clients: { fake: fakeClient(calls, fakeSpec), gone: fakeClient(calls, spec("gone", { available: false })) },
    providerDefinitions: { fake: { enabled: true }, gone: { enabled: true } },
  });
  const feed = recordFeed(manager);
  const handle = { provider: "fake", sessionId: "sess-r", nativeHandle: "thread-r", metadata: { cwd, model: "model-a", title: "Stored" } };
  const results = [];
  results.push(await outcome(async () => toAgentPayload(await manager.resumeAgentFromPersistence(
    handle,
    { modeId: "auto" },
    agentId,
    {
      createdAt: new Date(1700000000000),
      updatedAt: new Date(1700000005000),
      lastUserMessageAt: new Date(1700000004000),
      labels: { surface: "workspace" },
      workspaceId: "wks_9",
      attention: { requiresAttention: true, attentionReason: "finished", attentionTimestamp: new Date(1700000006000) },
    },
    { purpose: "interactive" },
  ))));
  results.push(await manager.getTimelineRows(agentId));
  results.push(await outcome(async () => { await manager.hydrateTimelineFromProvider(agentId); return null; }));
  await registry.upsert({ id: otherId, provider: "fake", cwd: "/nonexistent/spocky-archived", archivedAt: "2026-07-01T00:00:00.000Z" });
  results.push(await outcome(async () => toAgentPayload(await manager.resumeAgentFromPersistence(
    { provider: "fake", sessionId: "sess-a", metadata: { cwd: "/nonexistent/spocky-archived" } },
    undefined,
    otherId,
  ))));
  results.push(await outcome(async () => (await manager.resumeAgentFromPersistence({ provider: "nope", sessionId: "x", metadata: { cwd } })).id));
  results.push(await outcome(async () => (await manager.resumeAgentFromPersistence({ provider: "gone", sessionId: "x", metadata: { cwd } })).id));
  results.push(await outcome(async () => (await manager.resumeAgentFromPersistence(handle, undefined, "not-a-uuid")).id));
  await sleep(100);
  await manager.flush();
  await registry.flush();
  return { results, calls, feed, stored: await registry.get(agentId) };
};

const titles = async () => {
  const calls = [];
  const scripted = JSON.parse(scenarioTurnsJson);
  const registry = new AgentStorage(`${home}/titles`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: { fake: fakeClient(calls, spec("fake", { turns: [scripted.held] })) },
    providerDefinitions: { fake: { enabled: true } },
  });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  const results = [manager.hasInFlightRun(agentId), manager.hasInFlightRun(unknownId)];
  results.push(await outcome(async () => { await manager.setTitle(agentId, "  New title  "); return null; }));
  results.push(await outcome(async () => { await manager.setTitle(agentId, "   "); return null; }));
  results.push(await outcome(async () => { await manager.setTitle(unknownId, "x"); return null; }));
  manager.runAgent(agentId, "hold").catch(() => {});
  const started = (entry) => entry[0] === "agent_stream" && entry[2].type === "turn_started" && entry[2].turnId === "turn-5";
  for (let tick = 0; !feed.some(started); tick += 1) {
    if (tick === 2000) throw new Error("turn-5 never started");
    await sleep(5);
  }
  results.push(manager.hasInFlightRun(agentId));
  await sleep(100);
  await manager.flush();
  await registry.flush();
  return { results, feed, stored: await registry.get(agentId) };
};

const runstart = async () => {
  const calls = [];
  const scripted = JSON.parse(scenarioTurnsJson);
  const registry = new AgentStorage(`${home}/runstart`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: { fake: fakeClient(calls, spec("fake", { turns: [scripted.startSlowDone, scripted.startFail, scripted.startSlowAbort] })) },
    providerDefinitions: { fake: { enabled: true } },
  });
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  const start = (id, options) => outcome(async () => { await manager.waitForAgentRunStart(id, options); return "started"; });
  const finished = async (run) => {
    const settled = await run;
    return typeof settled === "string" ? settled : settled.finalText;
  };
  const results = [];
  results.push(await start(unknownId));
  results.push(await start(agentId));
  const slow = manager.runAgent(agentId, "slow").catch((error) => error.message);
  results.push(await start(agentId));
  results.push(await outcome(() => finished(slow)));
  const failed = manager.runAgent(agentId, "fail").catch((error) => error.message);
  results.push(await start(agentId));
  results.push(await outcome(() => finished(failed)));
  const aborted = manager.runAgent(agentId, "abort").catch((error) => error.message);
  const stop = new AbortController();
  setTimeout(() => stop.abort("stop"), 30);
  results.push(await start(agentId, { signal: stop.signal }));
  const pre = new AbortController();
  pre.abort(new Error("pre"));
  results.push(await start(agentId, { signal: pre.signal }));
  results.push(await start(agentId));
  results.push(await outcome(() => finished(aborted)));
  results.push(await start(agentId));
  await sleep(100);
  await manager.flush();
  await registry.flush();
  return { results, calls };
};

const outofband = async () => {
  const calls = [];
  const registry = new AgentStorage(`${home}/outofband`, logger);
  const manager = new AgentManager({ logger, registry, clients: { fake: fakeClient(calls, spec("fake")) }, providerDefinitions: { fake: { enabled: true } } });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  const results = [];
  results.push(await outcome(async () => manager.tryRunOutOfBand(agentId, "hello")));
  results.push(await outcome(async () => manager.tryRunOutOfBand(agentId, "/goal pause", { clientMessageId: "client-oob" })));
  await sleep(50);
  results.push(await outcome(async () => manager.tryRunOutOfBand(agentId, "/goal fail", { clientMessageId: "" })));
  await sleep(50);
  results.push(await outcome(async () => manager.tryRunOutOfBand(unknownId, "/goal pause")));
  await sleep(50);
  await manager.flush();
  await registry.flush();
  return { results, calls, feed, rows: await manager.getTimelineRows(agentId) };
};

const shutdown = async () => {
  const calls = [];
  const registry = new AgentStorage(`${home}/shutdown`, logger);
  const manager = new AgentManager({ logger, registry, clients: { fake: fakeClient(calls, spec("fake", { createDelay: 150 })) }, providerDefinitions: { fake: { enabled: true } } });
  const order = [];
  const create = (id, name) => manager.createAgent({ provider: "fake", cwd }, id, {}).then(
    () => { order.push(`created ${name}`); },
    (error) => { order.push(`failed ${name}: ${error.message}`); },
  );
  const first = create(agentId, "first");
  await sleep(20);
  await manager.flush();
  order.push("flush done");
  await first;
  const second = create(otherId, "second");
  await sleep(20);
  manager.prepareForShutdown();
  await manager.flushForShutdown();
  order.push("flushForShutdown done");
  await second;
  await create(unknownId, "third");
  await registry.flush();
  return { order, calls };
};

const loading = async () => {
  const { ensureAgentLoaded } = await import(`${dist}/server/agent/agent-loading.js`);
  const calls = [];
  const registry = new AgentStorage(`${home}/loading`, logger);
  const options = () => ({ logger, registry, clients: { fake: fakeClient(calls, spec("fake")) }, providerDefinitions: { fake: { enabled: true } } });
  const first = new AgentManager(options());
  await first.createAgent({ provider: "fake", cwd, model: "m1" }, agentId, { labels: { surface: "x" }, workspaceId: "wks_1" });
  await first.flush();
  await registry.flush();
  const stored = await registry.get(agentId);
  await registry.upsert({ ...stored, id: otherId, persistence: null, title: "No handle" });
  await registry.upsert({ ...stored, id: "00000000-0000-4000-8000-0000000000c3", provider: "ghost" });
  const second = new AgentManager(options());
  const feed = recordFeed(second);
  const deps = (broadcastTimeline) => ({ agentManager: second, agentStorage: registry, logger, broadcastTimeline });
  const load = (id, broadcastTimeline = false) => outcome(async () => toAgentPayload(await ensureAgentLoaded(id, deps(broadcastTimeline))));
  const results = await Promise.all([load(agentId), load(agentId, true)]);
  results.push(await load(agentId));
  results.push(await load(otherId));
  results.push(await load("00000000-0000-4000-8000-0000000000c3"));
  results.push(await load("00000000-0000-4000-8000-0000000000d4"));
  await sleep(50);
  await second.flush();
  await registry.flush();
  return { results, calls, feed };
};

const replaceScenario = async () => {
  const scripted = JSON.parse(scenarioTurnsJson);
  const collect = async (stream, events) => { for await (const event of stream) events.push(event); return events; };
  const build = async (name, turns, specExtra, managerExtra = {}) => {
    const calls = [];
    const registry = new AgentStorage(`${home}/replace-${name}`, logger);
    const manager = new AgentManager({ logger, registry, clients: { fake: fakeClient(calls, spec("fake", { turns, ...specExtra })) }, providerDefinitions: { fake: { enabled: true } }, ...managerExtra });
    const feed = recordFeed(manager);
    await manager.createAgent({ provider: "fake", cwd }, agentId, {});
    return { calls, registry, manager, feed };
  };
  const finish = async ({ calls, registry, manager, feed }, extra) => {
    await sleep(100);
    await manager.flush();
    await registry.flush();
    return { ...extra, calls, feed, agent: toAgentPayload(manager.getAgent(agentId)), rows: await manager.getTimelineRows(agentId), stored: await registry.get(agentId) };
  };
  const running = await build("running", [scripted.rpIdle, scripted.long, scripted.rpAfter], { interrupt: scripted.interrupt });
  const idleEvents = await collect(await running.manager.replaceAgentRun(agentId, "idle prompt"), []);
  await sleep(50);
  const old = running.manager.streamAgent(agentId, "long task");
  const oldEvents = [(await old.next()).value];
  await sleep(50);
  const replacement = await running.manager.replaceAgentRun(agentId, "replace it");
  await collect(old, oldEvents);
  const newEvents = await collect(replacement, []);
  const a = await finish(running, { idleEvents, oldEvents, newEvents });
  const finishing = await build("finishing", [scripted.rpFinishing, scripted.rpAfterFinish], {});
  const first = finishing.manager.streamAgent(agentId, "finishing");
  const firstEvents = [(await first.next()).value];
  await sleep(30);
  const second = await finishing.manager.replaceAgentRun(agentId, "while finishing");
  await collect(first, firstEvents);
  const secondEvents = await collect(second, []);
  // The replacement starts only after the finishing run has ended.
  const streamAt = (type, turnId) => finishing.feed.findIndex((entry) => entry[0] === "agent_stream" && entry[2].type === type && entry[2].turnId === turnId);
  const waited = streamAt("turn_completed", "turn-14") !== -1 && streamAt("turn_completed", "turn-14") < streamAt("turn_started", "turn-16");
  const b = await finish(finishing, { firstEvents, secondEvents, waited });
  const refused = await build("refused", [scripted.rpHeld], { interruptHang: true }, { rescueTimeouts: { interruptSessionMs: 80 } });
  const held = refused.manager.streamAgent(agentId, "long task");
  const heldEvents = [(await held.next()).value];
  await sleep(50);
  const failure = await outcome(async () => await refused.manager.replaceAgentRun(agentId, "never"));
  const again = await outcome(async () => { refused.manager.streamAgent(agentId, "again"); return null; });
  await collect(held, heldEvents);
  const c = await finish(refused, { heldEvents, failure, again });
  const starting = await build("starting", [scripted.slowStart, scripted.rpAfterFinish], {});
  const slow = starting.manager.streamAgent(agentId, "slow");
  const slowEvents = [];
  const slowDone = collect(slow, slowEvents);
  await sleep(100);
  const afterSlow = await starting.manager.replaceAgentRun(agentId, "while starting");
  await slowDone;
  const afterSlowEvents = await collect(afterSlow, []);
  const d = await finish(starting, { slowEvents, afterSlowEvents });
  return { a, b, c, d };
};

const rewindScenario = async () => {
  const scripted = JSON.parse(scenarioTurnsJson);
  const collect = async (stream, events) => { for await (const event of stream) events.push(event); return events; };
  const noFlagId = "00000000-0000-4000-8000-0000000000f2";
  const flags = { ...JSON.parse(capabilitiesJson), supportsRewindConversation: true, supportsRewindFiles: true, supportsRewindBoth: true };
  const warns = [];
  const infos = [];
  // Only the rewind messages: the manager logs other info lines this port does not.
  const warnLogger = { ...logger, child(bindings) { return withChild(this, bindings); }, warn(bindings, message) { warns.push([serializeLogErr(bindings), message]); }, info(bindings, message) { if (message.startsWith("agent.rewind.")) infos.push([bindings, message]); } };
  const calls = [];
  const registry = new AgentStorage(`${home}/rewind`, logger);
  const manager = new AgentManager({
    logger: warnLogger,
    registry,
    clients: {
      fake: fakeClient(calls, spec("fake", { turns: [scripted.rwEcho, scripted.rwNoEcho, scripted.long], history: scripted.history, interrupt: scripted.interrupt, capabilities: flags, revert: ["conversation", "files"] })),
      noflag: fakeClient(calls, spec("noflag", { history: scripted.history, revert: ["conversation", "files", "both"] })),
    },
    providerDefinitions: { fake: { enabled: true }, noflag: { enabled: true } },
  });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  await manager.createAgent({ provider: "noflag", cwd }, noFlagId, {});
  const rewind = (id, messageId, mode) => outcome(async () => { await manager.rewind(id, messageId, mode); return null; });
  const echoEvents = await collect(manager.streamAgent(agentId, "rewind me", { clientMessageId: "client-rw" }), []);
  const noEchoEvents = await collect(manager.streamAgent(agentId, "no ack", { clientMessageId: "client-noack" }), []);
  const results = {};
  results.unknownAgent = await rewind("00000000-0000-4000-8000-0000000000f3", "x", "files");
  results.unacknowledged = await rewind(agentId, "client-noack", "conversation");
  results.files = await rewind(agentId, "unknown-message", "files");
  results.both = await rewind(agentId, "client-rw", "both");
  results.noFlag = await rewind(noFlagId, "x", "conversation");
  const held = manager.streamAgent(agentId, "hold");
  const heldEvents = [(await held.next()).value];
  await sleep(50);
  results.running = await rewind(agentId, "client-rw", "files");
  // A rewind that did not cancel the run would leave this stream open.
  await Promise.race([collect(held, heldEvents), sleep(3000).then(() => { throw new Error("the running turn was not cancelled by rewind"); })]);
  results.conversation = await rewind(agentId, "client-rw", "conversation");
  await sleep(100);
  await manager.flush();
  await registry.flush();
  const refusedCalls = [];
  const refusedRegistry = new AgentStorage(`${home}/rewind-refused`, logger);
  const refused = new AgentManager({ logger, registry: refusedRegistry, clients: { fake: fakeClient(refusedCalls, spec("fake", { turns: [scripted.rpHeld], interruptHang: true, capabilities: flags, revert: ["files"] })) }, providerDefinitions: { fake: { enabled: true } }, rescueTimeouts: { interruptSessionMs: 80 } });
  await refused.createAgent({ provider: "fake", cwd }, agentId, {});
  const refusedHeld = refused.streamAgent(agentId, "long task");
  const refusedEvents = [(await refusedHeld.next()).value];
  await sleep(50);
  const refusedResult = await outcome(async () => { await refused.rewind(agentId, "x", "files"); return null; });
  await collect(refusedHeld, refusedEvents);
  await sleep(100);
  await refused.flush();
  await refusedRegistry.flush();
  return {
    results, echoEvents, noEchoEvents, heldEvents, calls, feed, warns, infos,
    agent: toAgentPayload(manager.getAgent(agentId)),
    rows: await manager.getTimelineRows(agentId),
    stored: await registry.get(agentId),
    refused: { result: refusedResult, events: refusedEvents, calls: refusedCalls, agent: toAgentPayload(refused.getAgent(agentId)) },
  };
};

const cancelLogsScenario = async () => {
  const scripted = JSON.parse(scenarioTurnsJson);
  const collect = async (stream, events) => { for await (const event of stream) events.push(event); return events; };
  const logs = [];
  const recorder = { ...logger, child(bindings) { return withChild(this, bindings); }, warn(bindings, message) { logs.push(["warn", serializeLogErr(bindings), message]); }, error(bindings, message) { logs.push(["error", serializeLogErr(bindings), message]); } };
  const cases = {};
  const runCase = async (name, turns, specExtra, managerExtra = {}, wait = 0) => {
    const calls = [];
    const registry = new AgentStorage(`${home}/cancel-${name}`, logger);
    const manager = new AgentManager({ logger: recorder, registry, clients: { fake: fakeClient(calls, spec("fake", { turns, ...specExtra })) }, providerDefinitions: { fake: { enabled: true } }, ...managerExtra });
    const feed = recordFeed(manager);
    await manager.createAgent({ provider: "fake", cwd }, agentId, {});
    const held = manager.streamAgent(agentId, "hold");
    const heldEvents = [(await held.next()).value];
    await sleep(50);
    const result = await outcome(async () => await manager.cancelAgentRun(agentId));
    if (wait) await sleep(wait);
    await collect(held, heldEvents);
    await sleep(100);
    await manager.flush();
    await registry.flush();
    cases[name] = { result, heldEvents, calls, feed, agent: toAgentPayload(manager.getAgent(agentId)) };
  };
  const rescue = { rescueTimeouts: { interruptSessionMs: 80 } };
  await runCase("hang", [scripted.rpHeld], { interruptHang: true }, rescue);
  await runCase("fails", [scripted.rpHeld], { interruptFails: true }, rescue);
  await runCase("late", [scripted.rpHeld], { interruptLateFailMs: 150 }, rescue, 200);
  await runCase("force", [scripted.long], {});
  return { cases, logs };
};

const reloadScenario = async () => {
  const scripted = JSON.parse(scenarioTurnsJson);
  const collect = async (stream, events) => { for await (const event of stream) events.push(event); return events; };
  const warns = [];
  const warnLogger = { ...logger, child(bindings) { return withChild(this, bindings); }, warn(bindings, message) { warns.push([serializeLogErr(bindings), message]); } };
  const build = async (name, { provider = "fake", turns = [], specExtra = {}, managerExtra = {} } = {}) => {
    const calls = [];
    const registry = new AgentStorage(`${home}/reload-${name}`, logger);
    const manager = new AgentManager({ logger: warnLogger, registry, clients: { [provider]: fakeClient(calls, spec(provider, { turns, interrupt: scripted.interrupt, ...specExtra })) }, providerDefinitions: { [provider]: { enabled: true } }, ...managerExtra });
    const feed = recordFeed(manager);
    await manager.createAgent({ provider, cwd }, agentId, { labels: { lane: "reload" }, workspaceId: "wks_1" });
    return { calls, registry, manager, feed };
  };
  const finish = async ({ calls, registry, manager, feed }, extra) => {
    await sleep(100);
    await manager.flush();
    await registry.flush();
    const agent = manager.getAgent(agentId);
    return { ...extra, calls, feed, agent: agent ? toAgentPayload(agent) : null, rows: agent ? await manager.getTimelineRows(agentId) : null, subagents: agent ? manager.listProviderSubagents(agentId) : null, stored: await registry.get(agentId) };
  };
  const reload = (c, overrides, options) => outcome(async () => toAgentPayload(await c.manager.reloadAgentSession(agentId, overrides, options)));
  const run = (c, prompt) => collect(c.manager.streamAgent(agentId, prompt), []);

  const idle = await build("idle", { turns: [scripted.rpIdle], specExtra: { history: scripted.history } });
  const idleEvents = await run(idle, "before reload");
  const idleResult = await reload(idle, { title: "Reloaded", modeId: "read-only" });
  // The reloaded agent's history stays primed, so this is a no-op.
  await idle.manager.hydrateTimelineFromProvider(agentId);
  const a = await finish(idle, { idleEvents, result: idleResult });

  const rehydrate = await build("rehydrate", { turns: [scripted.subagents], specExtra: { history: scripted.history } });
  const rehydrateEvents = await run(rehydrate, "delegate");
  // Hydrating once primes the history, which a rehydrating reload must drop.
  await rehydrate.manager.hydrateTimelineFromProvider(agentId);
  const rehydrateResult = await reload(rehydrate, undefined, { rehydrateFromDisk: true });
  // The history is no longer primed, so this hydrates again.
  await rehydrate.manager.hydrateTimelineFromProvider(agentId);
  const b = await finish(rehydrate, { rehydrateEvents, result: rehydrateResult });

  const running = await build("running", { turns: [scripted.long] });
  const held = running.manager.streamAgent(agentId, "long task");
  const heldEvents = [(await held.next()).value];
  await sleep(50);
  const runningResult = await reload(running);
  await collect(held, heldEvents);
  const c = await finish(running, { heldEvents, result: runningResult });

  const refused = await build("refused", { turns: [scripted.rpHeld], specExtra: { interruptHang: true, interrupt: undefined }, managerExtra: { rescueTimeouts: { interruptSessionMs: 80 } } });
  const refusedHeld = refused.manager.streamAgent(agentId, "long task");
  const refusedEvents = [(await refusedHeld.next()).value];
  await sleep(50);
  const refusedResult = await reload(refused);
  await collect(refusedHeld, refusedEvents);
  const d = await finish(refused, { refusedEvents, result: refusedResult });

  const noMcp = await build("nomcp", { provider: "nomcp", specExtra: { capabilities: { ...JSON.parse(capabilitiesJson), supportsMcpServers: false }, noPersistence: true } });
  const e = await finish(noMcp, { result: await reload(noMcp, { mcpServers: { a: { type: "stdio", command: "echo" } } }) });
  // The persistence handle names a provider with no client.
  const noClient = await build("noclient", { provider: "nomcp", specExtra: { capabilities: { ...JSON.parse(capabilitiesJson), supportsMcpServers: false } } });
  const j = await finish(noClient, { result: await reload(noClient, { mcpServers: { a: { type: "stdio", command: "echo" } } }) });

  // The last error and the last usage survive a reload.
  const failedRun = await build("lasterror", { turns: [scripted.failed] });
  const failedEvents = await run(failedRun, "fail me");
  const k = await finish(failedRun, { failedEvents, result: await reload(failedRun) });
  const usageRun = await build("lastusage");
  const usageEvents = await run(usageRun, "use tokens");
  const l = await finish(usageRun, { usageEvents, result: await reload(usageRun) });

  const slow = await build("slowclose", { specExtra: { closeHangs: true }, managerExtra: { rescueTimeouts: { reloadSessionCloseMs: 80 } } });
  const slowFirst = await reload(slow);
  const slowSecond = await reload(slow);
  const f = await finish(slow, { results: [slowFirst, slowSecond] });

  const failing = await build("resumefails", { specExtra: { resumeFails: true } });
  const g = await finish(failing, { result: await reload(failing) });

  const bare = await build("nopersistence", { specExtra: { noPersistence: true } });
  const h = await finish(bare, { result: await reload(bare, { title: "Fresh" }) });

  // An agent restored with recorded timestamps keeps them through a reload.
  const restoredCalls = [];
  const restoredRegistry = new AgentStorage(`${home}/reload-restored`, logger);
  const restoredManager = new AgentManager({ logger: warnLogger, registry: restoredRegistry, clients: { fake: fakeClient(restoredCalls, spec("fake")) }, providerDefinitions: { fake: { enabled: true } } });
  const restoredFeed = recordFeed(restoredManager);
  await restoredManager.resumeAgentFromPersistence(
    { provider: "fake", sessionId: "sess-r", nativeHandle: "thread-r", metadata: { cwd, model: "model-a", title: "Stored" } },
    { modeId: "auto" },
    agentId,
    {
      createdAt: new Date(1700000000000),
      updatedAt: new Date(1700000005000),
      lastUserMessageAt: new Date(1700000004000),
      labels: { surface: "workspace" },
      workspaceId: "wks_9",
      attention: { requiresAttention: true, attentionReason: "finished", attentionTimestamp: new Date(1700000006000) },
    },
    { purpose: "interactive" },
  );
  const restored = { calls: restoredCalls, registry: restoredRegistry, manager: restoredManager, feed: restoredFeed };
  const i = await finish(restored, { result: await reload(restored, { title: "Again" }) });

  const unknown = await outcome(async () => { await bare.manager.reloadAgentSession("00000000-0000-4000-8000-0000000000f3"); return null; });
  return { a, b, c, d, e, f, g, h, i, j, k, l, unknown, warns };
};

const failureLogsScenario = async () => {
  const scripted = JSON.parse(scenarioTurnsJson);
  const logs = [];
  const recorder = { ...logger, child(bindings) { return withChild(this, bindings); }, warn(bindings, message) { logs.push(["warn", serializeLogErr(bindings), message]); }, error(bindings, message) { logs.push(["error", serializeLogErr(bindings), message]); } };
  const build = async (name, specExtra, clientsOnly = false) => {
    const calls = [];
    const registry = new AgentStorage(`${home}/failure-${name}`, logger);
    const manager = new AgentManager({ logger: recorder, registry, clients: { fake: fakeClient(calls, spec("fake", specExtra)) }, providerDefinitions: { fake: { enabled: true } } });
    const feed = recordFeed(manager);
    if (!clientsOnly) await manager.createAgent({ provider: "fake", cwd }, agentId, {});
    return { calls, registry, manager, feed };
  };
  const finish = async ({ calls, manager, registry, feed }, extra) => {
    await sleep(100);
    await manager.flush();
    await registry.flush();
    return { ...extra, calls, feed };
  };

  const closing = await build("close", { import: scripted.badImport, closeFails: true }, true);
  const closeResult = await outcome(async () => toAgentPayload(await closing.manager.importProviderSession({ provider: "fake", providerHandleId: "h1", cwd, workspaceId: "wks_9" })));
  const closeCase = await finish(closing, { result: closeResult });

  const events = await build("event", { turns: [scripted.badItem] });
  const eventStream = [];
  for await (const event of events.manager.streamAgent(agentId, "bad item")) eventStream.push(event);
  const eventCase = await finish(events, { eventStream });

  const history = await build("history", { history: scripted.history, historyFails: true });
  await history.manager.reloadAgentSession(agentId, undefined, { rehydrateFromDisk: true });
  const historyResult = await outcome(async () => { await history.manager.hydrateTimelineFromProvider(agentId); return null; });
  const historyCase = await finish(history, { result: historyResult });
  return { closeCase, eventCase, historyCase, logs };
};

const settingsScenario = async () => {
  const cases = JSON.parse(scenarioTurnsJson).settingsCases;
  const out = {};
  for (const item of cases) {
    const calls = [];
    const registry = new AgentStorage(`${home}/settings-${item.name}`, logger);
    const manager = new AgentManager({ logger, registry, clients: { fake: fakeClient(calls, spec("fake", item.spec)) }, providerDefinitions: { fake: { enabled: true } } });
    const feed = recordFeed(manager);
    await manager.createAgent({ provider: "fake", cwd }, agentId, {});
    const steps = [];
    for (const [kind, ...args] of item.ops) {
      const result = await outcome(async () => {
        if (kind === "mode") return (await manager.setAgentMode(agentId, args[0])) ?? null;
        if (kind === "model") { await manager.setAgentModel(agentId, args[0]); return null; }
        if (kind === "thinking") return (await manager.setAgentThinkingOption(agentId, args[0])) ?? null;
        await manager.setAgentFeature(agentId, args[0], args[1]);
        return null;
      });
      steps.push({ result, agent: toAgentPayload(manager.getAgent(agentId)) });
    }
    const unknown = await outcome(async () => (await manager.setAgentMode("00000000-0000-4000-8000-0000000000f3", "x")) ?? null);
    await sleep(100);
    await manager.flush();
    await registry.flush();
    out[item.name] = { steps, unknown, calls, feed, stored: await registry.get(agentId) };
  }
  return out;
};

const metadataScenario = async () => {
  const scripted = JSON.parse(scenarioTurnsJson);
  const unknownId = "00000000-0000-4000-8000-0000000000f3";
  const calls = [];
  const registry = new AgentStorage(`${home}/metadata`, logger);
  const manager = new AgentManager({ logger, registry, clients: { fake: fakeClient(calls, spec("fake")) }, providerDefinitions: { fake: { enabled: true } } });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, { labels: { lane: "one" }, workspaceId: "wks_1" });
  await manager.createAgent({ provider: "fake", cwd }, otherId, { labels: { lane: "other" }, workspaceId: "wks_1" });
  await manager.closeAgent(otherId);
  await manager.flush();
  await registry.flush();
  const live = () => toAgentPayload(manager.getAgent(agentId));
  const results = [];
  const step = async (run) => results.push(await outcome(async () => { await run(); return live(); }));
  await step(() => manager.setLabels(agentId, { lane: "two", extra: "1" }));
  await step(() => manager.setLabels(agentId, { extra: null }));
  await step(() => manager.setLabels(unknownId, { a: "b" }));
  await step(() => manager.updateAgentMetadata(agentId, { title: "New title", labels: { x: "y" } }));
  await step(() => manager.updateAgentMetadata(agentId, { title: "", labels: { x: null } }));
  await step(() => manager.updateAgentMetadata(otherId, { title: "Stored title", labels: { s: "1", lane: null } }));
  await step(() => manager.updateAgentMetadata(otherId, {}));
  await step(() => manager.updateAgentMetadata(unknownId, { title: "x" }));
  await step(() => manager.markAgentUnread(agentId));
  await step(() => manager.markAgentUnread(agentId));
  await step(() => manager.markAgentUnread(otherId));
  await step(() => manager.markAgentUnread(otherId));
  await step(() => manager.markAgentUnread(unknownId));
  const storedAt = "2026-07-01T00:00:01.000Z";
  const storedRecord = (suffix, extra) => ({ id: `00000000-0000-4000-8000-0000000000${suffix}`, provider: "fake", cwd, createdAt: "2026-07-01T00:00:00.000Z", updatedAt: storedAt, lastStatus: "idle", ...extra });
  const storedIds = {};
  for (const [suffix, extra] of [["f4", { lastStatus: "running" }], ["f5", { archivedAt: "2026-07-01T00:00:00.000Z" }], ["f6", { internal: true }], ["f7", { requiresAttention: true }], ["f8", { updatedAt: "2099-01-01T00:00:00.000Z" }]]) {
    const record = storedRecord(suffix, extra);
    await registry.upsert(record);
    storedIds[suffix] = record.id;
    await step(() => manager.markAgentUnread(record.id));
  }
  const pendingCalls = [];
  const pendingRegistry = new AgentStorage(`${home}/metadata-pending`, logger);
  const pendingManager = new AgentManager({ logger, registry: pendingRegistry, clients: { fake: fakeClient(pendingCalls, spec("fake", { response: scripted.spontaneousPermission })) }, providerDefinitions: { fake: { enabled: true } } });
  const pendingFeed = recordFeed(pendingManager);
  await pendingManager.createAgent({ provider: "fake", cwd }, agentId, {});
  await pendingManager.respondToPermission(agentId, "none", { behavior: "allow" });
  await sleep(300);
  const pending = await outcome(async () => { await pendingManager.markAgentUnread(agentId); return toAgentPayload(pendingManager.getAgent(agentId)); });
  await sleep(100);
  await manager.flush();
  await registry.flush();
  const bareManager = new AgentManager({ logger, clients: { fake: fakeClient([], spec("fake")) }, providerDefinitions: { fake: { enabled: true } } });
  const noStorage = [
    await outcome(async () => { await bareManager.markAgentUnread(unknownId); return null; }),
    await outcome(async () => { await bareManager.updateAgentMetadata(unknownId, { title: "x" }); return null; }),
  ];
  const storedFixtures = {};
  for (const [suffix, id] of Object.entries(storedIds)) storedFixtures[suffix] = await registry.get(id);
  return { results, storedLive: await registry.get(agentId), storedOther: await registry.get(otherId), storedFixtures, pending, pendingFeed, feed, calls, noStorage };
};

const steerScenario = async () => {
  const scripted = JSON.parse(scenarioTurnsJson);
  const collect = async (stream, events) => { for await (const event of stream) events.push(event); return events; };
  const out = {};
  for (const item of scripted.steerCases) {
    const calls = [];
    const hookCalls = [];
    const raceEvents = [];
    let manager;
    const registry = new AgentStorage(`${home}/steer-${item.name}`, logger);
    let raceStream;
    manager = new AgentManager({
      logger,
      registry,
      clients: { fake: fakeClient(calls, spec("fake", { turns: item.turns.map((name) => scripted[name]), interrupt: scripted.interrupt, ...item.spec })) },
      providerDefinitions: { fake: { enabled: true } },
      beforeSteerUnavailableFallback: async (input) => {
        hookCalls.push(input);
        if (item.hookRace) {
          await manager.cancelAgentRun(input.agentId);
          raceStream = manager.streamAgent(input.agentId, "race");
          raceEvents.push((await raceStream.next()).value);
        }
      },
    });
    const feed = recordFeed(manager);
    await manager.createAgent({ provider: "fake", cwd }, agentId, {});
    let held;
    const heldEvents = [];
    if (item.running) {
      held = manager.streamAgent(agentId, "hold");
      heldEvents.push((await held.next()).value);
      // The held turn's reasoning item reaches the timeline after the coalescing window.
      for (let tick = 0; manager.getTimeline(agentId).length === 0; tick += 1) {
        if (tick === 2000) throw new Error("the held turn never reached the timeline");
        await sleep(5);
      }
    }
    const results = [];
    for (const [kind, prompt, options] of item.ops) {
      results.push(await outcome(async () => {
        if (kind === "steer") return await manager.steerAgentRun(agentId, prompt, options ?? undefined);
        const dispatch = await manager.steerOrReplaceActiveTurn(agentId, prompt, options ?? undefined);
        if (dispatch.status !== "replaced") return { status: dispatch.status };
        const events = [];
        await collect(dispatch.iterator, events);
        return { status: "replaced", events };
      }));
    }
    if (held) {
      if (item.cancel !== false) await outcome(async () => await manager.cancelAgentRun(agentId));
      await collect(held, heldEvents);
    }
    if (raceStream) await collect(raceStream, raceEvents);
    await sleep(100);
    await manager.flush();
    await registry.flush();
    out[item.name] = { results, heldEvents, raceEvents, hookCalls, calls, feed, agent: toAgentPayload(manager.getAgent(agentId)), rows: await manager.getTimelineRows(agentId) };
  }
  return out;
};

const timelineItemsScenario = async () => {
  const unknownId = "00000000-0000-4000-8000-0000000000f3";
  const otherId = "00000000-0000-4000-8000-0000000000f4";
  const calls = [];
  const registry = new AgentStorage(`${home}/timeline-items`, logger);
  const manager = new AgentManager({ logger, registry, clients: { fake: fakeClient(calls, spec("fake")) }, providerDefinitions: { fake: { enabled: true } } });
  const feed = recordFeed(manager);
  // An agent restored with recorded timestamps shows what each call touches.
  for (const id of [agentId, otherId]) {
    await manager.resumeAgentFromPersistence(
      { provider: "fake", sessionId: `sess-${id}`, nativeHandle: `thread-${id}`, metadata: { cwd, model: "model-a" } },
      undefined,
      id,
      { createdAt: new Date(1700000000000), updatedAt: new Date(1700000005000), lastUserMessageAt: new Date(1700000004000) },
      { purpose: "interactive" },
    );
  }
  const message = (text) => ({ type: "assistant_message", text });
  const results = [];
  const step = async (id, run) => {
    const result = await outcome(run);
    await sleep(50);
    await manager.flush();
    await registry.flush();
    results.push({ result, agent: toAgentPayload(manager.getAgent(id)), stored: await registry.get(id) });
  };
  await step(agentId, async () => { await manager.emitLiveTimelineItem(agentId, message("live")); return null; });
  await step(agentId, async () => { await manager.emitLiveTimelineItem(unknownId, message("x")); return null; });
  await step(otherId, async () => await manager.appendTimelineItem(otherId, message("appended")));
  await step(otherId, async () => await manager.appendTimelineItem(otherId, { type: "tool_call", callId: "x", name: "shell", status: "running", error: null }));
  await step(otherId, async () => await manager.appendTimelineItem(otherId, { type: "tool_call", callId: "big", name: "shell", status: "completed", error: null, detail: { type: "shell", command: "ls", output: "x".repeat(70000) } }));
  await step(otherId, async () => await manager.appendTimelineItem(unknownId, message("x")));
  return { results, rows: await manager.getTimelineRows(agentId), otherRows: await manager.getTimelineRows(otherId), feed };
};

const availabilityScenario = async () => {
  const calls = [];
  const warns = [];
  const warnLogger = { ...logger, child(bindings) { return withChild(this, bindings); }, warn(bindings, message) { warns.push([serializeLogErr(bindings), message]); } };
  const manager = new AgentManager({
    logger: warnLogger,
    registry: new AgentStorage(`${home}/availability`, logger),
    clients: {
      slowbad: fakeClient(calls, spec("slowbad", { available: "slow failure", availableDelayMs: 120 })),
      fake: fakeClient(calls, spec("fake")),
      gone: fakeClient(calls, spec("gone", { available: false })),
      fastbad: fakeClient(calls, spec("fastbad", { available: "fast failure" })),
    },
    providerDefinitions: { slowbad: { enabled: true }, fake: { enabled: true }, gone: { enabled: true }, fastbad: { enabled: true } },
  });
  const results = [];
  results.push(await outcome(async () => await manager.listProviderAvailability()));
  const afterList = warns.splice(0);
  for (const provider of ["fake", "gone", "fastbad", "nope"]) results.push(await outcome(async () => await manager.getProviderAvailability(provider)));
  return { results, afterList, warns };
};

const importableScenario = async () => {
  const scripted = JSON.parse(scenarioTurnsJson).importable;
  const bulk = { sessions: Array.from({ length: 22 }, (_, index) => ({ providerHandleId: `bulk-${index}`, cwd: "/bulk", title: `Bulk ${index}`, firstPromptPreview: null, lastPromptPreview: null, lastActivityAt: 1600000000000 + index * 1000 })) };
  const calls = [];
  const warns = [];
  const warnLogger = { ...logger, child(bindings) { return withChild(this, bindings); }, warn(bindings, message) { warns.push([serializeLogErr(bindings), message]); } };
  const listing = { ...JSON.parse(capabilitiesJson), supportsSessionListing: true };
  const make = (provider, importable) => fakeClient(calls, spec(provider, { capabilities: listing, ...(importable ? { importable } : {}) }));
  const clients = {
    alpha: make("alpha", scripted.alpha),
    slow: make("slow", scripted.slow),
    beta: make("beta", scripted.beta),
    nolist: make("nolist"),
    slowfail: make("slowfail", scripted.slowfail),
    failing: make("failing", scripted.failing),
    bulk: make("bulk", bulk),
    nocap: fakeClient(calls, spec("nocap", { importable: scripted.nocap })),
    off: make("off", scripted.off),
  };
  const manager = new AgentManager({
    logger: warnLogger,
    registry: new AgentStorage(`${home}/importable`, logger),
    clients,
    providerDefinitions: Object.fromEntries(Object.keys(clients).map((provider) => [provider, { enabled: provider !== "off" }])),
  });
  const cases = [
    ["all", undefined],
    ["limit2", { limit: 2 }],
    ["limitNegative", { limit: -1 }],
    ["limitFraction", { limit: 1.5 }],
    ["limitZero", { limit: 0 }],
    ["query", { query: "  LOGIN  " }],
    ["queryCwd", { query: "beta" }],
    ["queryBackslashCwd", { query: "C:" }],
    ["queryNone", { query: "zzz" }],
    ["queryBlank", { query: "   " }],
    ["filter", { providerFilter: ["alpha", "off", "nocap"] }],
    ["passthrough", { cwd: "/work", scanLimit: 7, limit: 3, providerFilter: ["beta"] }],
  ];
  const results = [];
  for (const [name, options] of cases) {
    const result = await outcome(async () => {
      const listed = await manager.listImportableSessions(options && { ...options, providerFilter: options.providerFilter && new Set(options.providerFilter) });
      return { ...listed, sessions: listed.sessions.map((session) => ({ ...session, lastActivityAt: session.lastActivityAt.getTime() })) };
    });
    const taken = calls.splice(0);
    results.push({ name, result, warns: warns.splice(0), calls: Object.fromEntries(Object.keys(clients).map((provider) => [provider, taken.filter((call) => call[1] === provider)])) });
  }
  return results;
};

const draftScenario = async () => {
  const calls = [];
  const warns = [];
  const warnLogger = { ...logger, child(bindings) { return withChild(this, bindings); }, warn(bindings, message) { warns.push([serializeLogErr(bindings), message]); } };
  const commands = [{ name: "review", description: "Review code", argumentHint: "" }];
  const features = [{ type: "toggle", id: "fast", label: "Fast", value: false }];
  const sessionCommands = [{ name: "session-review", description: "From a session", argumentHint: "" }];
  const sessionFeatures = [{ type: "toggle", id: "plan", label: "Plan", value: true }];
  const clients = {
    viaClient: fakeClient(calls, spec("viaClient", { clientCommands: commands, clientFeatures: features })),
    viaSession: fakeClient(calls, spec("viaSession", { sessionCommands, sessionFeatures })),
    bare: fakeClient(calls, spec("bare")),
    nullFeatures: fakeClient(calls, spec("nullFeatures", { sessionFeatures: null })),
    closeFails: fakeClient(calls, spec("closeFails", { sessionCommands, sessionFeatures, closeFails: true })),
    featuresOnly: fakeClient(calls, spec("featuresOnly", { clientFeatures: features })),
    gone: fakeClient(calls, spec("gone", { available: false, clientFeatures: features })),
    broken: fakeClient(calls, spec("broken", { available: "missing binary", clientCommands: commands })),
  };
  const manager = new AgentManager({
    logger: warnLogger,
    registry: new AgentStorage(`${home}/draft`, logger),
    clients,
    providerDefinitions: Object.fromEntries(Object.keys(clients).map((provider) => [provider, { enabled: true }])),
  });
  const cases = [
    ["viaClient", { provider: "viaClient", cwd, model: "m1" }],
    ["viaClientTrimmed", { provider: "viaClient", cwd, model: "  m2  " }],
    ["viaSession", { provider: "viaSession", cwd, model: "m1" }],
    ["bare", { provider: "bare", cwd, model: "m1" }],
    ["nullFeatures", { provider: "nullFeatures", cwd, model: "m1" }],
    ["closeFails", { provider: "closeFails", cwd, model: "m1" }],
    ["noModel", { provider: "viaSession", cwd }],
    ["defaultModel", { provider: "viaClient", cwd, model: " default " }],
    ["blankModel", { provider: "featuresOnly", cwd, model: "" }],
    ["noModelNoClientFeatures", { provider: "bare", cwd, model: "default" }],
    ["gone", { provider: "gone", cwd, model: "m1" }],
    ["broken", { provider: "broken", cwd, model: "m1" }],
    ["unknown", { provider: "nope", cwd, model: "m1" }],
    ["missingCwd", { provider: "viaClient", cwd: "/nonexistent/spocky-draft", model: "m1" }],
    ["noCwd", { provider: "viaSession", model: "m1" }],
    ["providerOptions", { provider: "viaClient", cwd, model: "m1", providerOptions: {} }],
  ];
  const results = [];
  for (const [name, config] of cases) {
    const commandsResult = await outcome(async () => await manager.listDraftCommands(config));
    const featuresResult = await outcome(async () => await manager.listDraftFeatures(config));
    results.push({ name, commands: commandsResult, features: featuresResult, calls: calls.splice(0), warns: warns.splice(0) });
  }
  return results;
};

const registryScenario = async () => {
  const scripted = JSON.parse(scenarioTurnsJson);
  const gammaId = "00000000-0000-4000-8000-0000000000f5";
  const calls = [];
  const warns = [];
  const warnLogger = { ...logger, child(bindings) { return withChild(this, bindings); }, warn(bindings, message) { warns.push([serializeLogErr(bindings), message]); } };
  const client = (provider, extra = {}) => fakeClient(calls, spec(provider, extra));
  const definition = { enabled: true };
  const manager = new AgentManager({
    logger: warnLogger,
    registry: new AgentStorage(`${home}/provider-registry`, logger),
    clients: { alpha: client("alpha", { turns: [scripted.held] }), beta: client("beta", { closeFails: true }), gamma: client("gamma") },
    providerDefinitions: { alpha: definition, beta: definition, gamma: definition },
    mcpAuthToken: "secret-token",
    resolvePaseoToolPolicy: (provider) => ({ enabled: true, name: provider }),
  });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "alpha", cwd }, agentId, {});
  await manager.createAgent({ provider: "beta", cwd }, otherId, {});
  await manager.createAgent({ provider: "gamma", cwd }, gammaId, {});
  for (const [id, body] of [[agentId, "one"], [agentId, "two"], [gammaId, "three"]]) await manager.appendTimelineItem(id, { type: "assistant_message", text: body });
  const held = manager.streamAgent(agentId, "hold");
  const heldFirst = (await held.next()).value;
  await sleep(50);
  const view = async () => ({
    ids: manager.getRegisteredProviderIds(),
    metrics: manager.getMetricsSnapshot(),
    availability: await manager.listProviderAvailability(),
    policies: [manager.getPaseoToolPolicy(agentId), manager.getPaseoToolPolicy(otherId), manager.getPaseoToolPolicy(unknownId)],
    token: manager.getMcpAuthToken(),
  });
  const create = async (provider) => await outcome(async () => (await manager.createAgent({ provider, cwd }, undefined, {})).provider);
  const steps = [];
  steps.push({ name: "initial", view: await view() });
  manager.registerClient("alpha", client("alpha"));
  manager.registerClient("delta", client("delta"));
  steps.push({ name: "registered", view: await view(), zeta: await create("zeta"), delta: await create("delta") });
  manager.updateProviderRegistry({
    providerDefinitions: { gamma: definition, alpha: definition, off: { enabled: false } },
    clients: { gamma: client("gamma"), alpha: client("alpha"), off: client("off") },
    retiredProviders: ["beta", "gamma", "nobody"],
  });
  await sleep(150);
  steps.push({ name: "updated", view: await view(), beta: await create("beta"), off: await create("off"), alpha: await create("alpha"), agents: manager.listAgents().map((agent) => [agent.id, agent.provider, agent.lifecycle]) });
  manager.updateProviderRegistry({ providerDefinitions: {}, clients: {} });
  steps.push({ name: "emptied", view: await view(), alpha: await create("alpha") });
  await manager.cancelAgentRun(agentId);
  await sleep(100);
  await manager.flush();
  return { steps, heldFirst, feed, warns, calls };
};

const callbacksScenario = async () => {
  const scripted = JSON.parse(scenarioTurnsJson);
  const gammaId = "00000000-0000-4000-8000-0000000000f5";
  const childId = "00000000-0000-4000-8000-0000000000f6";
  const calls = [];
  const log = [];
  const warns = [];
  const warnLogger = { ...logger, child(bindings) { return withChild(this, bindings); }, warn(bindings, message) { warns.push([serializeLogErr(bindings), message]); } };
  const client = (provider, turns) => fakeClient(calls, spec(provider, { turns: turns.map((name) => scripted[name]) }));
  const clients = { p1: client("p1", ["coalesce"]), p2: client("p2", ["failed"]), p3: client("p3", ["permission"]), p4: client("p4", ["coalesce"]) };
  const manager = new AgentManager({
    logger: warnLogger,
    registry: new AgentStorage(`${home}/callbacks`, logger),
    clients,
    providerDefinitions: Object.fromEntries(Object.keys(clients).map((provider) => [provider, { enabled: true }])),
    onAgentAttention: (notice) => { log.push(["first attention", notice]); },
  });
  manager.setAgentArchivedCallback(async (id) => { log.push(["first archived", id]); });
  const feed = recordFeed(manager);
  const create = async (provider, id, labels) => await manager.createAgent({ provider, cwd }, id, labels ? { labels } : {});
  await create("p1", agentId);
  await create("p2", otherId);
  await create("p3", gammaId);
  await create("p4", childId, { "paseo.parent-agent-id": agentId });
  const run = (id) => outcome(async () => { await manager.runAgent(id, "go"); return null; });
  const archive = (id) => outcome(async () => { await manager.archiveAgent(id); return null; });
  const results = [];
  results.push(await run(agentId));
  await sleep(50);
  manager.setAgentAttentionCallback((notice) => { log.push(["second attention", notice]); });
  results.push(await run(otherId));
  await sleep(50);
  manager.runAgent(gammaId, "ask").catch(() => {});
  const started = (entry) => entry[0] === "agent_stream" && entry[2].type === "turn_started" && entry[2].turnId === "turn-6";
  for (let tick = 0; !feed.some(started); tick += 1) {
    if (tick === 2000) throw new Error("turn-6 never started");
    await sleep(5);
  }
  await sleep(100);
  results.push(await run(childId));
  await sleep(50);
  results.push(await archive(agentId));
  manager.setAgentArchivedCallback(async (id) => { log.push(["second archived", id]); throw new Error("callback broke"); });
  results.push(await archive(otherId));
  await sleep(100);
  await manager.flush();
  return { results, log, warns, feed };
};

const persistFailureScenario = async () => {
  const scripted = JSON.parse(scenarioTurnsJson);
  const calls = [];
  const errors = [];
  const errorLogger = { ...logger, child(bindings) { return withChild(this, bindings); }, error(bindings, message) { errors.push([serializeLogErr(bindings), message]); } };
  const base = `${home}/persist-failure`;
  const manager = new AgentManager({ logger: errorLogger, registry: new AgentStorage(base, logger), clients: { fake: fakeClient(calls, spec("fake", { turns: [scripted.coalesce] })) }, providerDefinitions: { fake: { enabled: true } } });
  const feed = recordFeed(manager);
  await manager.createAgent({ provider: "fake", cwd }, agentId, {});
  await sleep(50);
  await manager.flush();
  // A read-only record directory makes every later write fail.
  const dirs = [base, ...fs.readdirSync(base, { withFileTypes: true }).filter((entry) => entry.isDirectory()).map((entry) => `${base}/${entry.name}`)];
  for (const dir of dirs) fs.chmodSync(dir, 0o500);
  try {
    const result = await outcome(async () => { await manager.runAgent(agentId, "go"); return null; });
    await sleep(100);
    await manager.flush();
    // The record directory, process id, clock and uuid in a temporary file name.
    const maskTemp = (path) => path.replace(/^.*\/persist-failure\//, "").replace(/\.\d+\.\d+\.[0-9a-f-]{36}\.tmp$/, ".<pid>.<ms>.<uuid>.tmp");
    const masked = errors.map(([bindings, message]) => {
      const path = bindings.err.path;
      return [{ ...bindings, err: { ...bindings.err, message: bindings.err.message.replace(path, maskTemp(path)), path: maskTemp(path) } }, message];
    });
    return { result, errors: masked, feed };
  } finally {
    for (const dir of dirs) fs.chmodSync(dir, 0o700);
  }
};

const catalogScenario = async () => {
  const gammaId = "00000000-0000-4000-8000-0000000000f5";
  const calls = [];
  const factoryLog = [];
  const native = { ...JSON.parse(capabilitiesJson), supportsNativePaseoTools: true };
  const factory = async (context) => {
    factoryLog.push(context);
    return { toJSON() { return { caller: context.callerAgentId }; }, tools: new Map(), getTool() {}, async executeTool() {} };
  };
  const clients = {
    native: fakeClient(calls, spec("native", { capabilities: native })),
    plain: fakeClient(calls, spec("plain")),
    quiet: fakeClient(calls, spec("quiet", { capabilities: native })),
  };
  const manager = new AgentManager({
    logger,
    registry: new AgentStorage(`${home}/catalog`, logger),
    clients,
    providerDefinitions: Object.fromEntries(Object.keys(clients).map((provider) => [provider, { enabled: true }])),
    paseoToolCatalogFactory: factory,
    mcpAuthToken: "secret-token",
    resolvePaseoToolPolicy: (provider) => (provider === "quiet" ? { enabled: false } : { enabled: true, name: provider }),
  });
  // With an MCP URL on the internal path (/mcp/agents) the launch config carries the internal
  // Paseo server, which a provider that gets the tools natively launches without.
  manager.setMcpBaseUrl("http://127.0.0.1:1/mcp/agents");
  const steps = [];
  const step = async (name, run) => {
    const result = await outcome(run);
    steps.push({ name, result, factory: factoryLog.splice(0), calls: calls.splice(0) });
  };
  const create = (provider, id) => async () => (await manager.createAgent({ provider, cwd }, id, {})).provider;
  await step("native", create("native", agentId));
  await step("plain", create("plain", otherId));
  await step("quiet", create("quiet", gammaId));
  manager.setPaseoToolsEnabled(false);
  await step("toolsOff", create("native"));
  manager.setPaseoToolsEnabled(true);
  manager.setPaseoToolCatalogFactory(null);
  await step("noFactory", create("native"));
  manager.setPaseoToolCatalogFactory(async () => { throw new Error("catalog broke"); });
  await step("failing", create("native"));
  manager.setPaseoToolCatalogFactory(factory);
  await step("resume", async () => (await manager.resumeAgentFromPersistence({ provider: "native", sessionId: "sess-c", nativeHandle: "thread-c", metadata: { cwd, model: "m" } })).provider);
  await step("reload", async () => (await manager.reloadAgentSession(agentId)).provider);
  return { steps };
};

const pluginLifecycleScenario = async () => {
  const scripted = JSON.parse(scenarioTurnsJson);
  const internalId = "00000000-0000-4000-8000-0000000000f7";
  const importedId = "00000000-0000-4000-8000-0000000000f8";
  const collect = async (stream, events) => { for await (const event of stream) events.push(event); return events; };
  const build = (name, hooks, specExtra = {}) => {
    const calls = [];
    const manager = new AgentManager({
      logger,
      registry: new AgentStorage(`${home}/plugin-${name}`, logger),
      clients: { fake: fakeClient(calls, spec("fake", specExtra)) },
      providerDefinitions: { fake: { enabled: true } },
      pluginLifecycle: hooks,
    });
    return { calls, manager, feed: recordFeed(manager) };
  };
  // A plugin that adds to the env of each request.
  const log = [];
  // The awaited calls and the emits are compared as two sequences: an emit runs on the dispatcher, so Rust may log it after the next awaited call.
  const splitLog = (entries) => ({ befores: entries.filter((entry) => entry[0] === "before"), emits: entries.filter((entry) => entry[0] === "emit") });
  // What subscribers hear and what plugins are told, in one sequence. The calls the manager awaits are left out: a subscriber hears an event on the dispatcher, after the manager has moved on.
  const order = [];
  const recording = {
    async before(name, request) {
      log.push(["before", name, request]);
      const checked = validateBeforeRequest(name, request);
      const added = name === "agent.session_open" ? { PLUGIN_OPEN: "1" } : { PLUGIN_CREATE: "1" };
      return { ...checked, env: { ...checked.env, ...added } };
    },
    emit(name, event) { log.push(["emit", name, event]); order.push(["emit", name]); },
  };
  const one = build("recording", recording, { turns: [scripted.ask, scripted.long, scripted.failed], response: scripted.response, interrupt: scripted.interrupt, import: scripted.import });
  const { manager } = one;
  manager.subscribe((event) => order.push(["event", event.type, event.type === "agent_state" ? event.agent.lifecycle : event.type === "agent_stream" ? event.event.type : null]));
  await manager.createAgent({ provider: "fake", cwd, title: "Plugin agent" }, agentId, { labels: { "paseo.parent-agent-id": otherId }, workspaceId: "wks_1" });
  await manager.createAgent({ provider: "fake", cwd, internal: true }, internalId, {});
  const second = manager.streamAgent(agentId, "remove x");
  const secondEvents = [(await second.next()).value];
  await manager.waitForAgentEvent(agentId);
  await manager.respondToPermission(agentId, "perm-1", { behavior: "allow" });
  await collect(second, secondEvents);
  await sleep(100);
  const third = manager.streamAgent(agentId, "long task");
  const thirdEvents = [(await third.next()).value];
  await sleep(50);
  await manager.cancelAgentRun(agentId);
  await collect(third, thirdEvents);
  await sleep(100);
  const fourthEvents = await outcome(async () => await collect(manager.streamAgent(agentId, "fail me"), []));
  await sleep(100);
  const archived = await outcome(async () => { await manager.archiveAgent(agentId); return null; });
  const archivedInternal = await outcome(async () => { await manager.archiveAgent(internalId); return null; });
  const archivedAgain = await outcome(async () => { await manager.archiveSnapshot(agentId, "2026-07-12T10:00:00.000Z"); return null; });
  const resumed = await outcome(async () => (await manager.resumeAgentFromPersistence({ provider: "fake", sessionId: "sess-p", nativeHandle: "thread-p", metadata: { cwd, model: "m" } }, undefined, otherId, { workspaceId: "wks_2" })).id);
  const reloaded = await outcome(async () => (await manager.reloadAgentSession(otherId)).id);
  const imported = await outcome(async () => (await manager.importProviderSession({ provider: "fake", providerHandleId: "h1", cwd, workspaceId: "wks_3" })).provider);
  // The archived agent resumes for its history, not for interaction.
  const resumedArchived = await outcome(async () => (await manager.resumeAgentFromPersistence({ provider: "fake", sessionId: "sess-p", nativeHandle: "thread-p", metadata: { cwd, model: "m" } }, undefined, agentId, { workspaceId: "wks_1" })).id);
  // A failing upsert archives nothing, so the plugin hears nothing.
  const base = `${home}/plugin-recording`;
  const dirs = [base, ...fs.readdirSync(base, { withFileTypes: true }).filter((entry) => entry.isDirectory()).map((entry) => `${base}/${entry.name}`)];
  for (const dir of dirs) fs.chmodSync(dir, 0o500);
  let archiveFailed;
  try {
    archiveFailed = await manager.archiveAgent(otherId).then(() => "ok", () => "threw");
  } finally {
    for (const dir of dirs) fs.chmodSync(dir, 0o700);
  }
  await sleep(100);
  await manager.flush();
  const recordingCase = { secondEvents, thirdEvents, fourthEvents, archived, archivedInternal, archivedAgain, resumed, reloaded, imported, resumedArchived, archiveFailed, order, ...splitLog(log.splice(0)), calls: one.calls, feed: one.feed };

  // The runtime with no plugin loaded: the requests are only validated.
  const validating = { async before(name, request) { log.push(["before", name, request]); return validateBeforeRequest(name, request); }, emit() {} };
  const plain = build("validating", validating);
  const created = await outcome(async () => (await plain.manager.createAgent({ provider: "fake", cwd }, undefined, { env: { KEEP: "me" } })).provider);
  const badEnv = await outcome(async () => (await plain.manager.createAgent({ provider: "fake", cwd }, undefined, { env: { BAD: 1 } })).provider);
  const validatingCase = { created, badEnv, log: log.splice(0), calls: plain.calls };
  // A plugin that refuses to open the session.
  const refusing = build("refusing", { async before(name, request) { if (name === "agent.session_open") throw new Error("blocked by plugin"); return validateBeforeRequest(name, request); }, emit() {} });
  const refused = await outcome(async () => (await refusing.manager.createAgent({ provider: "fake", cwd }, undefined, {})).provider);
  const refusingCase = { refused, calls: refusing.calls, feed: refusing.feed };
  return { recordingCase, validatingCase, refusingCase };
};

const traceScenario = async () => {
  const scripted = JSON.parse(scenarioTurnsJson);
  const traces = [];
  const traceLogger = { ...logger, child(bindings) { return withChild(this, bindings); }, trace(bindings, message) { traces.push([JSON.parse(JSON.stringify(bindings)), message]); } };
  const calls = [];
  const manager = new AgentManager({
    logger: traceLogger,
    registry: new AgentStorage(`${home}/trace`, logger),
    clients: { fake: fakeClient(calls, spec("fake", { turns: [scripted.ask, scripted.long, scripted.failed, scripted.coalesce, scripted.startSlowDone], response: scripted.response, interrupt: scripted.interrupt })) },
    providerDefinitions: { fake: { enabled: true } },
  });
  const collect = async (stream, events) => { for await (const event of stream) events.push(event); return events; };
  await manager.createAgent({ provider: "fake", cwd, title: "Traced" }, agentId, { workspaceId: "wks_1" });
  const second = manager.streamAgent(agentId, "remove x");
  const secondEvents = [(await second.next()).value];
  await manager.waitForAgentEvent(agentId);
  await manager.respondToPermission(agentId, "perm-1", { behavior: "allow" });
  await collect(second, secondEvents);
  await sleep(100);
  const third = manager.streamAgent(agentId, "long task");
  const thirdEvents = [(await third.next()).value];
  await sleep(50);
  const duplicate = await outcome(async () => { await collect(manager.streamAgent(agentId, "dup"), []); return null; });
  await manager.cancelAgentRun(agentId);
  await collect(third, thirdEvents);
  await sleep(100);
  const failed = await outcome(async () => await collect(manager.streamAgent(agentId, "fail me"), []));
  await sleep(100);
  const coalesced = await outcome(async () => await collect(manager.streamAgent(agentId, "coalesce"), []));
  await sleep(100);
  const staged = await outcome(async () => await collect(manager.streamAgent(agentId, "start slowly"), []));
  await sleep(100);
  const closed = await outcome(async () => { await manager.closeAgent(agentId); return null; });
  await sleep(100);
  await manager.flush();
  return { secondEvents, thirdEvents, duplicate, failed, coalesced, staged, closed, traces, calls };
};

const importScenario = async () => {
  const scripted = JSON.parse(scenarioTurnsJson);
  const calls = [];
  const registry = new AgentStorage(`${home}/import`, logger);
  const manager = new AgentManager({
    logger,
    registry,
    clients: {
      fake: fakeClient(calls, spec("fake", { import: scripted.import, turns: [scripted.impTurn], history: scripted.history })),
      plain: fakeClient(calls, spec("plain")),
      badimport: fakeClient(calls, spec("badimport", { import: scripted.badImport })),
      badtimeline: fakeClient(calls, spec("badtimeline", { import: scripted.badTimeline })),
      badsubagent: fakeClient(calls, spec("badsubagent", { import: scripted.badSubagent })),
    },
    providerDefinitions: { fake: { enabled: true }, plain: { enabled: true }, badimport: { enabled: true }, badtimeline: { enabled: true }, badsubagent: { enabled: true } },
  });
  const feed = recordFeed(manager);
  const run = (provider) => outcome(async () => toAgentPayload(await manager.importProviderSession({ provider, providerHandleId: "h1", cwd, workspaceId: "wks_9", labels: { a: "b" } })));
  const results = [await run("fake"), await run("plain"), await run("nope"), await run("badimport"), await run("badtimeline"), await run("badsubagent")];
  await sleep(50);
  await manager.flush();
  await registry.flush();
  const ids = [...new Set(feed.filter((entry) => entry[0] === "agent_state").map((entry) => entry[1].id))];
  // An imported agent's history is already primed, so this is a no-op.
  await manager.hydrateTimelineFromProvider(ids[0]);
  const turnEvents = [];
  for await (const event of manager.streamAgent(ids[0], "after import")) turnEvents.push(event);
  await sleep(50);
  await manager.flush();
  await registry.flush();
  const rows = {};
  for (const id of ids) rows[id] = await manager.getTimelineRows(id);
  const subagents = [];
  for (const id of ids) subagents.push(manager.listProviderSubagents(id));
  const stored = {};
  for (const id of ids) stored[id] = await registry.get(id);
  return { results, calls, feed, turnEvents, rows, subagents, stored };
};

const archive = async () => {
  const PARENT_LABEL = "paseo.parent-agent-id";
  const ids = { same: "00000000-0000-4000-8000-0000000000e1", other: "00000000-0000-4000-8000-0000000000e2", tab: "00000000-0000-4000-8000-0000000000e3", stored: "00000000-0000-4000-8000-0000000000e4" };
  const calls = [];
  const registry = new AgentStorage(`${home}/archive`, logger);
  const manager = new AgentManager({ logger, registry, clients: { fake: fakeClient(calls, spec("fake")) }, providerDefinitions: { fake: { enabled: true } } });
  const feed = recordFeed(manager);
  const create = (id, labels, workspaceId) => manager.createAgent({ provider: "fake", cwd }, id, { labels, workspaceId });
  await create(agentId, {}, "wks_1");
  await create(ids.same, { [PARENT_LABEL]: agentId }, "wks_1");
  await create(ids.other, { [PARENT_LABEL]: agentId }, "wks_2");
  await create(ids.tab, { [PARENT_LABEL]: agentId, "paseo.open-agent-tab.c1": "true" }, "wks_1");
  await create(ids.stored, { [PARENT_LABEL]: agentId }, "wks_1");
  await manager.closeAgent(ids.stored);
  await manager.flush();
  await registry.flush();
  const results = [];
  results.push(await outcome(async () => (await manager.archiveAgent(agentId)).archivedAt));
  await manager.flush();
  await registry.flush();
  const stored = {};
  for (const [name, id] of Object.entries({ parent: agentId, ...ids })) stored[name] = await registry.get(id);
  results.push(await outcome(async () => await manager.unarchiveSnapshot(agentId, { workspaceId: "wks_3", labels: { a: "b", gone: null } })));
  results.push(await outcome(async () => await manager.unarchiveSnapshot(agentId)));
  results.push(await outcome(async () => await manager.unarchiveSnapshot(unknownId)));
  results.push(await outcome(async () => { const { record, live, previousParentAgentId } = await manager.detachAgent(ids.same); return { id: record.id, labels: record.labels, live, previousParentAgentId }; }));
  results.push(await outcome(async () => { await manager.clearAgentAttention(ids.same); return null; }));
  results.push(await outcome(async () => { await manager.clearAgentAttention(unknownId); return null; }));
  await manager.flush();
  await registry.flush();
  const afterStored = {};
  for (const [name, id] of Object.entries({ parent: agentId, ...ids })) afterStored[name] = await registry.get(id);
  const byHandleCalls = [];
  const warns = [];
  const warnLogger = { ...logger, child(bindings) { return withChild(this, bindings); }, warn(bindings, message) { warns.push([serializeLogErr(bindings), message]); } };
  const byHandleRegistry = new AgentStorage(`${home}/archive-handle`, logger);
  const byHandle = new AgentManager({ logger: warnLogger, registry: byHandleRegistry, clients: { fake: fakeClient(byHandleCalls, spec("fake", { archiveFails: true })) }, providerDefinitions: { fake: { enabled: true } } });
  await byHandle.createAgent({ provider: "fake", cwd }, agentId, { labels: {}, workspaceId: "wks_1" });
  await byHandle.closeAgent(agentId);
  await byHandleRegistry.flush();
  const archivedRecord = await outcome(async () => await byHandle.archiveSnapshot(agentId, "2026-07-12T10:00:00.000Z"));
  const handle = (await byHandleRegistry.get(agentId)).persistence;
  const unarchived = await outcome(async () => { await byHandle.unarchiveSnapshotByHandle(handle); return null; });
  await byHandleRegistry.flush();
  return { results, stored, afterStored, calls, feed, byHandle: { archivedRecord, unarchived, record: await byHandleRegistry.get(agentId), calls: byHandleCalls, warns } };
};

const storedDates = async () => {
  const calls = [];
  const registry = new AgentStorage(`${home}/stored-dates`, logger);
  const options = () => ({ logger, registry, clients: { fake: fakeClient(calls, spec("fake")) }, providerDefinitions: { fake: { enabled: true } } });
  const first = new AgentManager(options());
  await first.createAgent({ provider: "fake", cwd }, agentId, { labels: {}, workspaceId: "wks_1" });
  await first.closeAgent(agentId);
  await first.flush();
  await registry.flush();
  const stored = await registry.get(agentId);
  await registry.upsert({ ...stored, id: otherId, createdAt: "Jan 3 2020 00:00:00 GMT", updatedAt: "Wed, 01 Jan 2020 12:00:00 GMT", lastUserMessageAt: "1/3/2020 00:00:00 GMT" });
  const second = new AgentManager(options());
  const feed = recordFeed(second);
  const results = [];
  results.push(await outcome(async () => await second.archiveSnapshot(otherId, "Jan 4 2020 00:00:00 GMT")));
  const times = feed.filter((entry) => entry[0] === "agent_state").map((entry) => [entry[1].createdAt, entry[1].updatedAt, entry[1].lastUserMessageAt].map((at) => (at === null ? null : Date.parse(at))));
  results.push(await outcome(async () => await second.unarchiveSnapshot(otherId)));
  await second.flush();
  await registry.flush();
  return { results, times, feed, stored: await registry.get(otherId) };
};

process.stdout.write(JSON.stringify({ main: await main(), errors: await errors(), turns: await turns(), permission: await permission(), lifecycle: await lifecycle(), subagents: await subagents(), hydration: await hydration(), resume: await resume(), titles: await titles(), runstart: await runstart(), outofband: await outofband(), shutdown: await shutdown(), loading: await loading(), replace: await replaceScenario(), rewind: await rewindScenario(), timelineItems: await timelineItemsScenario(), availability: await availabilityScenario(), importable: await importableScenario(), draft: await draftScenario(), registry: await registryScenario(), callbacks: await callbacksScenario(), persistFailure: await persistFailureScenario(), catalog: await catalogScenario(), pluginLifecycle: await pluginLifecycleScenario(), trace: await traceScenario(), steer: await steerScenario(), settings: await settingsScenario(), metadata: await metadataScenario(), cancelLogs: await cancelLogsScenario(), failureLogs: await failureLogsScenario(), reload: await reloadScenario(), import: await importScenario(), archive: await archive(), storedDates: await storedDates() }));
"#;

fn json(text: &str) -> JsValue {
    parse(text).expect("fixture JSON")
}

fn text(value: &str) -> JsValue {
    JsValue::String(value.to_owned())
}

#[allow(clippy::cast_precision_loss, reason = "sequence numbers are small")]
fn number(value: i64) -> JsValue {
    JsValue::Number(value as f64)
}

/// `isAvailable()`: resolves `Ok`, or rejects with `Err`'s message.
type Availability = Result<bool, String>;

/// One fake provider: what it reports and the turns its sessions play.
#[derive(Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag switches one fake behavior"
)]
struct Spec {
    provider: String,
    capabilities: JsValue,
    available: Availability,
    turns: Arc<Mutex<VecDeque<JsValue>>>,
    /// Emitted 200 ms after `respondToPermission`.
    response: Option<JsValue>,
    /// Emitted 10 ms after `interrupt`.
    interrupt: Option<JsValue>,
    /// What `streamHistory()` yields.
    history: Option<JsValue>,
    /// `session.initialTimeline`: `[{ item, timestamp }]`.
    initial_timeline: Option<JsValue>,
    /// `createSession` resolves after this long.
    create_delay: Option<Duration>,
    /// `isAvailable()` settles after this long.
    available_delay: Option<Duration>,
    /// `archiveNativeSession` rejects.
    archive_fails: bool,
    /// `interrupt` never resolves.
    interrupt_hang: bool,
    /// What `importSession` resolves, with `$CWD` for the working directory.
    import: Option<JsValue>,
    /// What `listImportableSessions` does: `sessions`, `delayMs` and `fail`.
    importable: Option<JsValue>,
    /// What the client's `listCommands` and `listFeatures` resolve, when it has them.
    client_commands: Option<JsValue>,
    client_features: Option<JsValue>,
    /// What the session's `listCommands` resolves, when it has it, and its `features`.
    session_commands: Option<JsValue>,
    session_features: Option<JsValue>,
    /// The `revert*` methods the session has: `conversation`, `files`, `both`.
    revert: Vec<&'static str>,
    /// The session has `steerActiveTurn`: `{ result, emit }`.
    steer: Option<JsValue>,
    /// The session has `setModel`, `setThinkingOption` and `setFeature`.
    settable: bool,
    /// What `setMode` resolves.
    mode_notice: Option<JsValue>,
    /// What `setThinkingOption` resolves.
    thinking_notice: Option<JsValue>,
    /// `getCurrentMode` resolves `null`.
    current_mode_null: bool,
    /// Merged over what `getRuntimeInfo` resolves.
    runtime_info_extra: Option<JsValue>,
    /// `interrupt` rejects at once.
    interrupt_fails: bool,
    /// `interrupt` rejects after this many milliseconds.
    interrupt_late_fail_ms: Option<u64>,
    /// `close` rejects.
    close_fails: bool,
    /// `streamHistory` throws after its events.
    history_fails: bool,
    /// `close` never resolves.
    close_hangs: bool,
    /// `resumeSession` rejects.
    resume_fails: bool,
    /// `describePersistence` returns `null`.
    no_persistence: bool,
}

fn spec(provider: &str) -> Spec {
    Spec {
        provider: provider.to_owned(),
        capabilities: json(CAPABILITIES),
        available: Ok(true),
        turns: Arc::new(Mutex::new(VecDeque::new())),
        response: None,
        interrupt: None,
        history: None,
        initial_timeline: None,
        create_delay: None,
        available_delay: None,
        archive_fails: false,
        interrupt_hang: false,
        import: None,
        importable: None,
        client_commands: None,
        client_features: None,
        session_commands: None,
        session_features: None,
        revert: Vec::new(),
        steer: None,
        settable: false,
        mode_notice: None,
        thinking_notice: None,
        current_mode_null: false,
        runtime_info_extra: None,
        interrupt_fails: false,
        interrupt_late_fail_ms: None,
        close_fails: false,
        history_fails: false,
        close_hangs: false,
        resume_fails: false,
        no_persistence: false,
    }
}

struct FakeSession {
    spec: Spec,
    listeners: Arc<Mutex<Vec<StreamCallback>>>,
    calls: Arc<Mutex<Vec<JsValue>>>,
    /// What `setModel` last received, as `describePersistence` shows it.
    model: Mutex<Option<String>>,
}

/// `async *streamHistory()` over the scripted history.
struct History(std::vec::IntoIter<JsValue>, bool);

impl AgentEventStream for History {
    fn next(&mut self) -> BoxFuture<'_, Option<AgentResult<AgentStreamEvent>>> {
        let next = self.0.next();
        // After its events, a failing history throws once.
        let fails = next.is_none() && std::mem::take(&mut self.1);
        Box::pin(async move {
            if fails {
                Some(Err(AgentError::new("history broke")))
            } else {
                next.map(Ok)
            }
        })
    }
}

fn prompt_value(prompt: &AgentPromptInput) -> JsValue {
    match prompt {
        AgentPromptInput::Text(text) => JsValue::String(text.clone()),
        AgentPromptInput::Blocks(blocks) => JsValue::Array(blocks.clone()),
    }
}

fn turn_id_of(events: &JsValue) -> String {
    events
        .as_array()
        .expect("events")
        .iter()
        .find(|event| event.get("type").and_then(JsValue::as_str) == Some("turn_started"))
        .and_then(|event| event.get("turnId").and_then(JsValue::as_str))
        .unwrap_or("turn-1")
        .to_owned()
}

fn event_type(event: &JsValue) -> Option<&str> {
    event.get("type").and_then(JsValue::as_str)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "scripted delays are small whole milliseconds"
)]
fn delay(event: &JsValue) -> Duration {
    Duration::from_millis(event.get("ms").and_then(JsValue::as_f64).expect("ms") as u64)
}

impl FakeSession {
    /// Emits the scripted `events` to every listener after `millis`, as
    /// `setTimeout`, pausing at `__delay` entries.
    fn emit_later(&self, events: JsValue, millis: u64) {
        let listeners = Arc::clone(&self.listeners);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(millis)).await;
            let callbacks = listeners.lock().expect("listeners").clone();
            for event in events.as_array().expect("events") {
                match event_type(event) {
                    Some("__delay") => tokio::time::sleep(delay(event)).await,
                    Some("__startDelay") => {}
                    _ => {
                        for callback in &callbacks {
                            callback(event.clone());
                        }
                    }
                }
            }
        });
    }

    fn record(&self, call: Vec<JsValue>) {
        self.calls.lock().expect("calls").push(JsValue::Array(call));
    }

    /// `revertConversation`, `revertFiles` or `revertBoth`, when the spec
    /// gives the session that method.
    fn revert(
        &self,
        kind: &str,
        method: &str,
        message_id: &str,
    ) -> Option<BoxFuture<'_, AgentResult<()>>> {
        if !self.spec.revert.contains(&kind) {
            return None;
        }
        self.record(vec![text(method), text(message_id)]);
        Some(Box::pin(async { Ok(()) }))
    }
}

/// The `/goal` handler of the fake session's `tryHandleOutOfBand`.
struct GoalHandler {
    fail: bool,
}

impl OutOfBandHandler for GoalHandler {
    fn run(self: Box<Self>, emit: StreamCallback) -> BoxFuture<'static, AgentResult<()>> {
        Box::pin(async move {
            if self.fail {
                return Err(AgentError::new("goal broke"));
            }
            emit(json(
                r#"{"type":"timeline","provider":"fake","item":{"type":"assistant_message","text":"Goal paused."}}"#,
            ));
            emit(json(
                r#"{"type":"usage_updated","provider":"fake","usage":{"inputTokens":1}}"#,
            ));
            Ok(())
        })
    }
}

impl AgentSession for FakeSession {
    fn try_handle_out_of_band(
        &self,
        prompt: &AgentPromptInput,
    ) -> Option<Option<Box<dyn OutOfBandHandler>>> {
        self.record(vec![text("tryHandleOutOfBand"), prompt_value(prompt)]);
        Some(match prompt {
            AgentPromptInput::Text(text) if text.starts_with("/goal") => {
                Some(Box::new(GoalHandler {
                    fail: text == "/goal fail",
                }) as Box<dyn OutOfBandHandler>)
            }
            _ => None,
        })
    }
    fn provider(&self) -> String {
        self.spec.provider.clone()
    }
    fn features(&self) -> Option<JsValue> {
        self.spec.session_features.clone()
    }
    fn list_commands(&self) -> Option<BoxFuture<'_, AgentResult<JsValue>>> {
        let commands = self.spec.session_commands.clone()?;
        self.record(vec![text("session.listCommands")]);
        Some(Box::pin(async move { Ok(commands) }))
    }
    fn id(&self) -> Option<String> {
        Some("sess-1".to_owned())
    }
    fn capabilities(&self) -> JsValue {
        self.spec.capabilities.clone()
    }
    fn run(
        &self,
        _prompt: AgentPromptInput,
        _options: Option<AgentRunOptions>,
    ) -> BoxFuture<'_, AgentResult<JsValue>> {
        Box::pin(async { Err(AgentError::new("unused")) })
    }
    fn start_turn(
        &self,
        prompt: AgentPromptInput,
        options: Option<AgentRunOptions>,
    ) -> BoxFuture<'_, AgentResult<String>> {
        let mut call = vec![text("startTurn"), prompt_value(&prompt)];
        call.push(options.map_or(JsValue::Null, |options| {
            let mut value = JsObject::new();
            if let Some(id) = options.client_message_id {
                value.insert("clientMessageId", JsValue::String(id));
            }
            JsValue::Object(value)
        }));
        self.calls.lock().expect("calls").push(JsValue::Array(call));
        let events = self
            .spec
            .turns
            .lock()
            .expect("turns")
            .pop_front()
            .unwrap_or_else(|| json(TURN_EVENTS));
        if let Some(failure) = events
            .as_array()
            .and_then(|events| events.first())
            .filter(|event| event_type(event) == Some("__startFail"))
        {
            let message = failure
                .get("message")
                .and_then(JsValue::as_str)
                .expect("message")
                .to_owned();
            let wait = delay(failure);
            return Box::pin(async move {
                tokio::time::sleep(wait).await;
                Err(AgentError::new(message))
            });
        }
        let turn_id = turn_id_of(&events);
        let start_delay = events
            .as_array()
            .and_then(|events| events.first())
            .filter(|event| event_type(event) == Some("__startDelay"))
            .map(delay);
        self.emit_later(events, 20);
        Box::pin(async move {
            if let Some(start_delay) = start_delay {
                tokio::time::sleep(start_delay).await;
            }
            Ok(turn_id)
        })
    }
    fn subscribe(&self, callback: StreamCallback) -> Unsubscribe {
        self.listeners.lock().expect("listeners").push(callback);
        Box::new(|| {})
    }
    fn stream_history(&self) -> Box<dyn AgentEventStream> {
        let events = self
            .spec
            .history
            .as_ref()
            .and_then(JsValue::as_array)
            .map(<[JsValue]>::to_vec)
            .unwrap_or_default();
        Box::new(History(events.into_iter(), self.spec.history_fails))
    }
    fn get_runtime_info(&self) -> BoxFuture<'_, AgentResult<JsValue>> {
        let extra = self.spec.runtime_info_extra.clone();
        Box::pin(async move {
            let mut info = spread(Some(&json(RUNTIME_INFO)));
            spread_into(&mut info, extra.as_ref());
            Ok(JsValue::Object(info))
        })
    }
    fn get_available_modes(&self) -> BoxFuture<'_, AgentResult<JsValue>> {
        Box::pin(async { Ok(json(MODES)) })
    }
    fn get_current_mode(&self) -> BoxFuture<'_, AgentResult<Option<String>>> {
        let null = self.spec.current_mode_null;
        Box::pin(async move { Ok((!null).then(|| "auto".to_owned())) })
    }
    fn set_mode(&self, mode_id: &str) -> BoxFuture<'_, AgentResult<Option<JsValue>>> {
        self.record(vec![text("setMode"), text(mode_id)]);
        let notice = self.spec.mode_notice.clone();
        Box::pin(async move { Ok(notice) })
    }
    fn supports_steer_active_turn(&self) -> bool {
        self.spec.steer.is_some()
    }
    fn steer_active_turn(
        &self,
        prompt: &AgentPromptInput,
        options: &spocky_session::agent_sdk::SteerActiveTurnOptions,
    ) -> Option<BoxFuture<'_, AgentResult<SteerResult>>> {
        let steer = self.spec.steer.clone()?;
        let mut seen = JsObject::new();
        if let Some(id) = &options.run.client_message_id {
            seen.insert("clientMessageId", text(id));
        }
        if let Some(clear) = options.clear_pending_permissions {
            seen.insert("clearPendingPermissions", JsValue::Bool(clear));
        }
        seen.insert("expectedTurnId", text(&options.expected_turn_id));
        self.record(vec![
            text("steerActiveTurn"),
            prompt_value(prompt),
            JsValue::Object(seen),
        ]);
        let callbacks = self.listeners.lock().expect("listeners").clone();
        for event in steer
            .get("emit")
            .and_then(JsValue::as_array)
            .unwrap_or_default()
        {
            for callback in &callbacks {
                callback(event.clone());
            }
        }
        let accepted = steer.get("result").and_then(JsValue::as_str) == Some("accepted");
        Some(Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(if accepted {
                SteerResult::Accepted
            } else {
                SteerResult::Unavailable
            })
        }))
    }
    fn set_model(&self, model_id: Option<&str>) -> Option<BoxFuture<'_, AgentResult<()>>> {
        if !self.spec.settable {
            return None;
        }
        self.record(vec![text("setModel"), model_id.map_or(JsValue::Null, text)]);
        *self.model.lock().expect("model") = Some(model_id.unwrap_or("null").to_owned());
        Some(Box::pin(async { Ok(()) }))
    }
    fn set_thinking_option(
        &self,
        thinking_option_id: Option<&str>,
    ) -> Option<BoxFuture<'_, AgentResult<Option<JsValue>>>> {
        if !self.spec.settable {
            return None;
        }
        self.record(vec![
            text("setThinkingOption"),
            thinking_option_id.map_or(JsValue::Null, text),
        ]);
        let notice = self.spec.thinking_notice.clone();
        Some(Box::pin(async move { Ok(notice) }))
    }
    fn set_feature(
        &self,
        feature_id: &str,
        value: JsValue,
    ) -> Option<BoxFuture<'_, AgentResult<()>>> {
        if !self.spec.settable {
            return None;
        }
        self.record(vec![text("setFeature"), text(feature_id), value]);
        Some(Box::pin(async { Ok(()) }))
    }
    fn get_pending_permissions(&self) -> AgentResult<Vec<JsValue>> {
        Ok(Vec::new())
    }
    fn respond_to_permission(
        &self,
        request_id: &str,
        response: JsValue,
    ) -> BoxFuture<'_, AgentResult<Option<JsValue>>> {
        self.record(vec![
            text("respondToPermission"),
            text(request_id),
            response,
        ]);
        if let Some(events) = self.spec.response.clone() {
            self.emit_later(events, 200);
        }
        Box::pin(async { Ok(None) })
    }
    fn initial_timeline(&self) -> Option<Vec<ImportedTimelineEntry>> {
        self.spec.initial_timeline.as_ref().map(|entries| {
            entries
                .as_array()
                .expect("entries")
                .iter()
                .map(|entry| ImportedTimelineEntry {
                    item: entry.get("item").cloned().expect("item"),
                    timestamp: entry
                        .get("timestamp")
                        .and_then(JsValue::as_str)
                        .map(str::to_owned),
                })
                .collect()
        })
    }
    fn describe_persistence(&self) -> Option<JsValue> {
        if self.spec.no_persistence {
            return None;
        }
        let mut handle = spread(Some(&json(PERSISTENCE)));
        if let Some(model) = self.model.lock().expect("model").as_deref() {
            handle.insert("nativeHandle", text(&format!("model-{model}")));
        }
        Some(JsValue::Object(handle))
    }
    fn interrupt(&self) -> BoxFuture<'_, AgentResult<()>> {
        self.record(vec![text("interrupt")]);
        if let Some(events) = self.spec.interrupt.clone() {
            self.emit_later(events, 10);
        }
        let hang = self.spec.interrupt_hang;
        let fails = self.spec.interrupt_fails;
        let late = self.spec.interrupt_late_fail_ms;
        Box::pin(async move {
            if fails {
                return Err(AgentError::new("interrupt failed"));
            }
            if let Some(late) = late {
                tokio::time::sleep(Duration::from_millis(late)).await;
                return Err(AgentError::new("interrupt failed late"));
            }
            if hang {
                std::future::pending::<()>().await;
            }
            Ok(())
        })
    }
    fn close(&self) -> BoxFuture<'_, AgentResult<()>> {
        self.calls
            .lock()
            .expect("calls")
            .push(JsValue::Array(vec![text("close")]));
        let hang = self.spec.close_hangs;
        let fails = self.spec.close_fails;
        Box::pin(async move {
            if fails {
                return Err(AgentError::new("close failed"));
            }
            if hang {
                std::future::pending::<()>().await;
            }
            Ok(())
        })
    }
    fn revert_conversation(&self, message_id: &str) -> Option<BoxFuture<'_, AgentResult<()>>> {
        self.revert("conversation", "revertConversation", message_id)
    }
    fn revert_files(&self, message_id: &str) -> Option<BoxFuture<'_, AgentResult<()>>> {
        self.revert("files", "revertFiles", message_id)
    }
    fn revert_both(&self, message_id: &str) -> Option<BoxFuture<'_, AgentResult<()>>> {
        self.revert("both", "revertBoth", message_id)
    }
}

fn launch_context_value(launch_context: Option<AgentLaunchContext>) -> JsValue {
    launch_context.map_or(JsValue::Null, |context| {
        let mut value = JsObject::new();
        if let Some(agent_id) = context.agent_id {
            value.insert("agentId", JsValue::String(agent_id));
        }
        if let Some(env) = context.env {
            value.insert("env", JsValue::Object(env));
        }
        if let Some(tools) = context.paseo_tools {
            value.insert(
                "paseoTools",
                object(vec![(
                    "caller",
                    text(&tools.tools().first().expect("a tool").name),
                )]),
            );
        }
        JsValue::Object(value)
    })
}

struct FakeClient {
    spec: Spec,
    calls: Arc<Mutex<Vec<JsValue>>>,
}

impl AgentClient for FakeClient {
    fn provider(&self) -> String {
        self.spec.provider.clone()
    }
    fn capabilities(&self) -> JsValue {
        self.spec.capabilities.clone()
    }
    fn create_session(
        &self,
        config: JsValue,
        launch_context: Option<AgentLaunchContext>,
        options: Option<AgentCreateSessionOptions>,
    ) -> BoxFuture<'_, AgentResult<Arc<dyn AgentSession>>> {
        let context = launch_context_value(launch_context);
        let options = options.map_or(JsValue::Null, |options| {
            let mut value = JsObject::new();
            if let Some(persist) = options.persist_session {
                value.insert("persistSession", JsValue::Bool(persist));
            }
            JsValue::Object(value)
        });
        self.calls.lock().expect("calls").push(JsValue::Array(vec![
            text("createSession"),
            config,
            context,
            options,
        ]));
        let session = FakeSession {
            spec: self.spec.clone(),
            listeners: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::clone(&self.calls),
            model: Mutex::new(None),
        };
        let delay = self.spec.create_delay;
        Box::pin(async move {
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            Ok(Arc::new(session) as Arc<dyn AgentSession>)
        })
    }
    fn resume_session(
        &self,
        handle: JsValue,
        overrides: Option<JsValue>,
        launch_context: Option<AgentLaunchContext>,
        options: Option<spocky_session::agent_sdk::AgentResumeSessionOptions>,
    ) -> BoxFuture<'_, AgentResult<Arc<dyn AgentSession>>> {
        let options = options.map_or(JsValue::Null, |options| {
            let mut value = JsObject::new();
            if let Some(purpose) = options.purpose {
                value.insert(
                    "purpose",
                    text(match purpose {
                        AgentResumePurpose::Interactive => "interactive",
                        AgentResumePurpose::History => "history",
                    }),
                );
            }
            JsValue::Object(value)
        });
        self.calls.lock().expect("calls").push(JsValue::Array(vec![
            text("resumeSession"),
            handle,
            overrides.unwrap_or(JsValue::Null),
            launch_context_value(launch_context),
            options,
        ]));
        let session = FakeSession {
            spec: self.spec.clone(),
            listeners: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::clone(&self.calls),
            model: Mutex::new(None),
        };
        let fails = self.spec.resume_fails;
        Box::pin(async move {
            if fails {
                return Err(AgentError::new("resume failed"));
            }
            Ok(Arc::new(session) as Arc<dyn AgentSession>)
        })
    }
    fn fetch_catalog(
        &self,
        options: FetchCatalogOptions,
        context: Option<Arc<dyn ProviderRefreshContext>>,
    ) -> BoxFuture<'_, AgentResult<JsValue>> {
        let mut value = JsObject::new();
        match options {
            FetchCatalogOptions::Workspace { cwd, force } => {
                value.insert("scope", text("workspace"));
                value.insert("cwd", JsValue::String(cwd));
                value.insert("force", JsValue::Bool(force));
            }
            FetchCatalogOptions::Global { force } => {
                value.insert("scope", text("global"));
                value.insert("force", JsValue::Bool(force));
            }
        }
        self.calls.lock().expect("calls").push(JsValue::Array(vec![
            text("fetchCatalog"),
            JsValue::Object(value),
            text(if context.is_none() {
                "no context"
            } else {
                "context"
            }),
        ]));
        Box::pin(async { Ok(json(CATALOG)) })
    }
    fn supports_import_session(&self) -> bool {
        self.spec.import.is_some()
    }
    fn list_commands(&self, config: JsValue) -> Option<BoxFuture<'_, AgentResult<JsValue>>> {
        let commands = self.spec.client_commands.clone()?;
        self.calls
            .lock()
            .expect("calls")
            .push(JsValue::Array(vec![text("listCommands"), config]));
        Some(Box::pin(async move { Ok(commands) }))
    }
    fn list_features(&self, config: JsValue) -> Option<BoxFuture<'_, AgentResult<JsValue>>> {
        let features = self.spec.client_features.clone()?;
        let calls = Arc::clone(&self.calls);
        // Recorded when polled, as the node client records when it is called.
        Some(Box::pin(async move {
            calls
                .lock()
                .expect("calls")
                .push(JsValue::Array(vec![text("listFeatures"), config]));
            Ok(features)
        }))
    }
    fn list_importable_sessions(
        &self,
        options: Option<ListImportableSessionsOptions>,
    ) -> Option<BoxFuture<'_, AgentResult<Vec<ImportableProviderSession>>>> {
        let importable = self.spec.importable.clone()?;
        let mut seen = JsObject::new();
        if let Some(options) = options {
            if let Some(limit) = options.limit {
                seen.insert("limit", JsValue::Number(limit));
            }
            if let Some(query) = &options.query {
                seen.insert("query", text(query));
            }
            if let Some(scan_limit) = options.scan_limit {
                seen.insert("scanLimit", JsValue::Number(scan_limit));
            }
            if let Some(cwd) = &options.cwd {
                seen.insert("cwd", text(cwd));
            }
        }
        self.calls.lock().expect("calls").push(JsValue::Array(vec![
            text("listImportableSessions"),
            text(&self.spec.provider),
            JsValue::Object(seen),
        ]));
        Some(Box::pin(async move {
            if let Some(delay) = importable.get("delayMs").and_then(JsValue::as_f64) {
                tokio::time::sleep(Duration::from_secs_f64(delay / 1000.0)).await;
            }
            if let Some(message) = importable.get("fail").and_then(JsValue::as_str) {
                return Err(AgentError::new(message));
            }
            let field = |session: &JsValue, key: &str| {
                session
                    .get(key)
                    .and_then(JsValue::as_str)
                    .map(str::to_owned)
            };
            Ok(importable
                .get("sessions")
                .and_then(JsValue::as_array)
                .unwrap_or_default()
                .iter()
                .map(|session| ImportableProviderSession {
                    provider_handle_id: field(session, "providerHandleId").expect("handle"),
                    cwd: field(session, "cwd").expect("cwd"),
                    title: field(session, "title"),
                    first_prompt_preview: field(session, "firstPromptPreview"),
                    last_prompt_preview: field(session, "lastPromptPreview"),
                    last_activity_at_millis: session
                        .get("lastActivityAt")
                        .and_then(JsValue::as_f64)
                        .expect("lastActivityAt"),
                })
                .collect())
        }))
    }
    fn import_session(
        &self,
        input: ImportProviderSessionInput,
        context: ImportProviderSessionContext,
    ) -> Option<BoxFuture<'_, AgentResult<ImportedProviderSession>>> {
        let import = self.spec.import.clone()?;
        let mut requested = JsObject::new();
        requested.insert("providerHandleId", text(&input.provider_handle_id));
        requested.insert("cwd", text(&input.cwd));
        let mut seen = JsObject::new();
        seen.insert("config", context.config.clone());
        seen.insert("storedConfig", context.stored_config.clone());
        seen.insert(
            "launchContext",
            launch_context_value(context.launch_context),
        );
        self.calls.lock().expect("calls").push(JsValue::Array(vec![
            text("importSession"),
            JsValue::Object(requested),
            JsValue::Object(seen),
        ]));
        let session = FakeSession {
            spec: self.spec.clone(),
            listeners: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::clone(&self.calls),
            model: Mutex::new(None),
        };
        let cwd = input.cwd;
        Some(Box::pin(async move {
            let imported = parse(&stringify(&import).replace("$CWD", &cwd)).expect("import");
            let timeline = imported
                .get("timeline")
                .and_then(JsValue::as_array)
                .expect("timeline")
                .iter()
                .map(|entry| ImportedTimelineEntry {
                    item: entry.get("item").cloned().expect("item"),
                    timestamp: entry
                        .get("timestamp")
                        .and_then(JsValue::as_str)
                        .map(str::to_owned),
                })
                .collect();
            Ok(ImportedProviderSession {
                session: Arc::new(session) as Arc<dyn AgentSession>,
                config: imported.get("config").cloned().expect("config"),
                persistence: imported.get("persistence").cloned().expect("persistence"),
                timeline,
                provider_subagent_events: imported
                    .get("providerSubagentEvents")
                    .and_then(JsValue::as_array)
                    .map(<[JsValue]>::to_vec),
            })
        }))
    }
    fn archive_native_session(&self, handle: JsValue) -> Option<BoxFuture<'_, AgentResult<()>>> {
        self.calls
            .lock()
            .expect("calls")
            .push(JsValue::Array(vec![text("archiveNativeSession"), handle]));
        let fails = self.spec.archive_fails;
        Some(Box::pin(async move {
            if fails {
                Err(AgentError::new("native archive failed"))
            } else {
                Ok(())
            }
        }))
    }
    fn unarchive_native_session(&self, handle: JsValue) -> Option<BoxFuture<'_, AgentResult<()>>> {
        self.calls
            .lock()
            .expect("calls")
            .push(JsValue::Array(vec![text("unarchiveNativeSession"), handle]));
        Some(Box::pin(async { Ok(()) }))
    }
    fn is_available(
        &self,
        _signal: Option<AbortSignal>,
        _options: Option<FetchCatalogOptions>,
    ) -> BoxFuture<'_, AgentResult<bool>> {
        let available = self.spec.available.clone();
        let delay = self.spec.available_delay;
        Box::pin(async move {
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            available.map_err(AgentError::new)
        })
    }
}

fn feed_entry(event: &AgentManagerEvent) -> JsValue {
    match event {
        AgentManagerEvent::AgentState(agent) => JsValue::Array(vec![
            text("agent_state"),
            to_agent_payload(&agent.payload_view(), None).expect("payload"),
        ]),
        AgentManagerEvent::AgentStream {
            agent_id,
            event,
            seq,
            epoch,
            timestamp,
        } => JsValue::Array(vec![
            text("agent_stream"),
            text(agent_id),
            event.clone(),
            seq.map_or(JsValue::Null, number),
            epoch.as_deref().map_or(JsValue::Null, text),
            timestamp.as_deref().map_or(JsValue::Null, text),
        ]),
        AgentManagerEvent::TimelineReplacement { .. } => {
            JsValue::Array(vec![text("timeline_replacement")])
        }
        AgentManagerEvent::ProviderSubagent(event) => {
            JsValue::Array(vec![text("provider_subagent"), event.clone()])
        }
    }
}

type Calls = Arc<Mutex<Vec<JsValue>>>;
type Feed = Arc<Mutex<Vec<JsValue>>>;

fn manager_with(
    calls: &Calls,
    registry: &AgentStorage,
    providers: Vec<(Spec, ProviderDefinition)>,
) -> AgentManager {
    let (clients, provider_definitions) = providers
        .into_iter()
        .map(|(spec, definition)| {
            let id = spec.provider.clone();
            let client = Arc::new(FakeClient {
                spec,
                calls: Arc::clone(calls),
            }) as Arc<dyn AgentClient>;
            ((id.clone(), client), (id, definition))
        })
        .unzip();
    AgentManager::new(AgentManagerOptions {
        clients,
        provider_definitions,
        registry: Some(registry.clone()),
        ..AgentManagerOptions::default()
    })
}

fn enabled() -> ProviderDefinition {
    ProviderDefinition {
        enabled: true,
        ..ProviderDefinition::default()
    }
}

/// Subscribes a recorder of every manager event for the manager's life,
/// as the JS side never unsubscribes it.
fn record_feed(manager: &AgentManager) -> Feed {
    let feed = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&feed);
    let unsubscribe = manager
        .subscribe(
            Arc::new(move |event| sink.lock().expect("feed").push(feed_entry(event))),
            SubscribeOptions::default(),
        )
        .expect("subscribe");
    std::mem::forget(unsubscribe);
    feed
}

fn outcome(result: Result<JsValue, AgentError>) -> JsValue {
    let mut value = JsObject::new();
    match result {
        Ok(ok) => value.insert("ok", ok),
        Err(error) => {
            value.insert("name", JsValue::String(error.name));
            value.insert("message", JsValue::String(error.message));
        }
    }
    JsValue::Object(value)
}

fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in entries {
        out.insert(key, value);
    }
    JsValue::Object(out)
}

fn read_record(directory: &Path) -> JsValue {
    let project = std::fs::read_dir(directory)
        .expect("records")
        .next()
        .expect("record directory")
        .expect("entry")
        .path();
    parse(&std::fs::read_to_string(project.join(format!("{AGENT_ID}.json"))).expect("record"))
        .expect("record JSON")
}

async fn main_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("main"));
    let manager = manager_with(&calls, &registry, vec![(spec("fake"), enabled())]);
    let feed = record_feed(&manager);
    let mut env = JsObject::new();
    env.insert("EXTRA", text("1"));
    let mut labels = JsObject::new();
    labels.insert("surface", text("workspace"));
    let created = manager
        .create_agent(
            object(vec![
                ("provider", text("fake")),
                ("cwd", text(cwd)),
                ("title", text("  Fake title  ")),
                ("model", text(" default ")),
            ]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions {
                workspace_id: Some("wks_1".to_owned()),
                labels: Some(JsValue::Object(labels)),
                env: Some(env),
                ..CreateAgentOptions::default()
            },
        )
        .await
        .expect("create");
    let created = to_agent_payload(&created.payload_view(), None).expect("payload");
    let run = manager
        .run_agent(
            AGENT_ID,
            AgentPromptInput::Text("hello".to_owned()),
            Some(AgentRunOptions {
                client_message_id: Some("client-1".to_owned()),
                ..AgentRunOptions::default()
            }),
        )
        .await
        .expect("run");
    let wait = manager
        .wait_for_agent_event(AGENT_ID, WaitForAgentOptions::default())
        .await
        .expect("wait");
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("calls", JsValue::Array(calls)),
        ("created", created),
        ("run", run.to_js()),
        ("wait", wait.to_js()),
        (
            "rows",
            JsValue::Array(manager.get_timeline_rows(AGENT_ID).expect("rows")),
        ),
        (
            "fetch",
            manager
                .fetch_timeline(AGENT_ID, FetchDirection::Tail, None, Some(3))
                .expect("fetch")
                .to_js(),
        ),
        ("feed", JsValue::Array(feed)),
        ("record", read_record(&home.join("main"))),
    ])
}

async fn errors_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("errors"));
    let mut gone = spec("gone");
    gone.available = Ok(false);
    let mut broken = spec("broken");
    broken.available = Err("missing binary".to_owned());
    let mut nomcp = spec("nomcp");
    if let JsValue::Object(capabilities) = &mut nomcp.capabilities {
        capabilities.insert("supportsMcpServers", JsValue::Bool(false));
    }
    let tools = ProviderDefinition {
        enabled: true,
        apply_tool_policy: Some(Arc::new(
            |config: &JsValue, tool_policy: Option<&JsValue>| {
                let mut config = match config {
                    JsValue::Object(config) => config.clone(),
                    _ => JsObject::new(),
                };
                config.insert(
                    "toolPolicy",
                    tool_policy.cloned().unwrap_or(JsValue::Undefined),
                );
                JsValue::Object(config)
            },
        )),
        ..ProviderDefinition::default()
    };
    let manager = manager_with(
        &calls,
        &registry,
        vec![
            (spec("fake"), enabled()),
            (gone, enabled()),
            (broken, enabled()),
            (spec("off"), ProviderDefinition::default()),
            (spec("tools"), tools),
            (nomcp, enabled()),
        ],
    );
    let mut results = Vec::new();
    for case in json(&ERROR_CASES.replace("$CWD", cwd))
        .as_array()
        .expect("cases")
    {
        let case = case.as_array().expect("case");
        let agent_id = case[1].as_str().map(str::to_owned);
        let created = manager
            .create_agent(case[0].clone(), agent_id, CreateAgentOptions::default())
            .await
            .map(|snapshot| text(&snapshot.id));
        results.push(outcome(created));
    }
    results.push(outcome(
        manager
            .run_agent(UNKNOWN_ID, AgentPromptInput::Text("hi".to_owned()), None)
            .await
            .map(|run| run.to_js()),
    ));
    results.push(outcome(
        manager
            .wait_for_agent_event(UNKNOWN_ID, WaitForAgentOptions::default())
            .await
            .map(|wait| wait.to_js()),
    ));
    results.push(outcome(
        manager
            .subscribe(
                Arc::new(|_: &AgentManagerEvent| {}),
                SubscribeOptions {
                    agent_id: Some("bad".to_owned()),
                    replay_state: None,
                },
            )
            .map(|_| JsValue::Null),
    ));
    manager.flush().await;
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("calls", JsValue::Array(calls)),
    ])
}

fn turn_started(feed: &Feed, turn_id: &str) -> bool {
    feed.lock().expect("feed").iter().any(|entry| {
        let entry = entry.as_array().expect("entry");
        entry[0].as_str() == Some("agent_stream")
            && entry[2].get("type").and_then(JsValue::as_str) == Some("turn_started")
            && entry[2].get("turnId").and_then(JsValue::as_str) == Some(turn_id)
    })
}

async fn wait_for_turn_started(feed: &Feed, turn_id: &str) {
    for _ in 0..2000 {
        if turn_started(feed, turn_id) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("{turn_id} never started");
}

fn scripted(spec: &Spec, names: &[&str]) {
    let turns = json(SCENARIO_TURNS);
    let mut queue = spec.turns.lock().expect("turns");
    for name in names {
        queue.push_back(turns.get(name).expect("turn").clone());
    }
}

/// Waits on the held turn whose signal is aborted before the wait, with a
/// string after it, with an `Error` after it, and with no reason.
async fn aborted_waits(manager: &AgentManager) -> Vec<JsValue> {
    let wait = |options: WaitForAgentOptions| {
        let manager = manager.clone();
        async move {
            outcome(
                manager
                    .wait_for_agent_event(AGENT_ID, options)
                    .await
                    .map(|wait| wait.to_js()),
            )
        }
    };
    let mut results = Vec::new();
    let pre = AbortController::default();
    pre.abort(AbortReason::Value(text("pre")));
    results.push(
        wait(WaitForAgentOptions {
            signal: Some(pre.signal()),
            ..WaitForAgentOptions::default()
        })
        .await,
    );
    let stop = AbortController::default();
    let signal = stop.signal();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        stop.abort(AbortReason::Value(text("stop")));
    });
    results.push(
        wait(WaitForAgentOptions {
            signal: Some(signal),
            ..WaitForAgentOptions::default()
        })
        .await,
    );
    let timeout = AbortController::default();
    let signal = timeout.signal();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        timeout.abort(AbortReason::Error(AgentError::new("wait timeout")));
    });
    results.push(
        wait(WaitForAgentOptions {
            signal: Some(signal),
            ..WaitForAgentOptions::default()
        })
        .await,
    );
    let plain = AbortController::default();
    plain.abort(AbortReason::Value(JsValue::Undefined));
    results.push(
        wait(WaitForAgentOptions {
            signal: Some(plain.signal()),
            ..WaitForAgentOptions::default()
        })
        .await,
    );
    results
}

async fn turns_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("turns"));
    let fake = spec("fake");
    scripted(&fake, &["failed", "canceled", "coalesce", "held"]);
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let run = |prompt: &'static str| {
        let manager = manager.clone();
        async move {
            outcome(
                manager
                    .run_agent(AGENT_ID, AgentPromptInput::Text(prompt.to_owned()), None)
                    .await
                    .map(|run| run.to_js()),
            )
        }
    };
    let wait = |options: WaitForAgentOptions| {
        let manager = manager.clone();
        async move {
            outcome(
                manager
                    .wait_for_agent_event(AGENT_ID, options)
                    .await
                    .map(|wait| wait.to_js()),
            )
        }
    };
    let mut results = vec![
        run("fail").await,
        wait(WaitForAgentOptions::default()).await,
        run("cancel").await,
        wait(WaitForAgentOptions::default()).await,
        run("coalesce").await,
        wait(WaitForAgentOptions {
            wait_for_active: true,
            ..WaitForAgentOptions::default()
        })
        .await,
    ];
    let held = tokio::spawn(run("hold"));
    wait_for_turn_started(&feed, "turn-5").await;
    results.extend(aborted_waits(&manager).await);
    results.push(run("again").await);
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    held.abort();
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
        (
            "rows",
            JsValue::Array(manager.get_timeline_rows(AGENT_ID).expect("rows")),
        ),
        (
            "stored",
            registry.get(AGENT_ID).await.unwrap_or(JsValue::Null),
        ),
    ])
}

async fn permission_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("permission"));
    let fake = spec("fake");
    scripted(&fake, &["permission"]);
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let asking = tokio::spawn({
        let manager = manager.clone();
        async move {
            manager
                .run_agent(AGENT_ID, AgentPromptInput::Text("ask".to_owned()), None)
                .await
        }
    });
    wait_for_turn_started(&feed, "turn-6").await;
    let mut results = Vec::new();
    for _ in 0..2 {
        results.push(outcome(
            manager
                .wait_for_agent_event(AGENT_ID, WaitForAgentOptions::default())
                .await
                .map(|wait| wait.to_js()),
        ));
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    asking.abort();
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
        (
            "rows",
            JsValue::Array(manager.get_timeline_rows(AGENT_ID).expect("rows")),
        ),
    ])
}

async fn collect(stream: &mut TurnEventStream, events: &mut Vec<JsValue>) {
    while let Some(event) = stream.next().await {
        events.push(event.expect("stream event"));
    }
}

async fn lifecycle_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("lifecycle"));
    let turns = json(SCENARIO_TURNS);
    let mut fake = spec("fake");
    scripted(&fake, &["ask", "long", "slowStart"]);
    fake.response = turns.get("response").cloned();
    fake.interrupt = turns.get("interrupt").cloned();
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let prompt = |text: &str| AgentPromptInput::Text(text.to_owned());
    let mut second = manager
        .stream_agent(AGENT_ID, prompt("remove x"), None)
        .expect("second stream");
    let mut second_events = vec![second.next().await.expect("first").expect("event")];
    let permission_wait = manager
        .wait_for_agent_event(AGENT_ID, WaitForAgentOptions::default())
        .await
        .expect("permission wait");
    let pending = manager.get_pending_permissions(AGENT_ID).expect("pending");
    let respond = manager
        .respond_to_permission(
            AGENT_ID,
            "perm-1",
            object(vec![("behavior", text("allow"))]),
        )
        .await
        .expect("respond");
    collect(&mut second, &mut second_events).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut third = manager
        .stream_agent(AGENT_ID, prompt("long task"), None)
        .expect("third stream");
    let mut third_events = vec![third.next().await.expect("first").expect("event")];
    tokio::time::sleep(Duration::from_millis(50)).await;
    let cancel = manager.cancel_agent_run(AGENT_ID).await.expect("cancel");
    collect(&mut third, &mut third_events).await;
    let cancel_again = manager
        .cancel_agent_run(AGENT_ID)
        .await
        .expect("cancel again");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let starting = tokio::spawn({
        let manager = manager.clone();
        async move {
            outcome(
                manager
                    .run_agent(AGENT_ID, AgentPromptInput::Text("slow".to_owned()), None)
                    .await
                    .map(|run| run.to_js()),
            )
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let cancel_starting = manager
        .cancel_agent_run(AGENT_ID)
        .await
        .expect("cancel while starting");
    let starting_result = starting.await.expect("join");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let rows = manager.get_timeline_rows(AGENT_ID).expect("rows");
    let fetch = manager
        .fetch_timeline(AGENT_ID, FetchDirection::Tail, None, Some(3))
        .expect("fetch")
        .to_js();
    manager.close_agent(AGENT_ID).await.expect("close");
    manager.close_agent(AGENT_ID).await.expect("close again");
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("secondEvents", JsValue::Array(second_events)),
        ("permissionWait", permission_wait.to_js()),
        ("pending", JsValue::Array(pending)),
        ("respond", respond.unwrap_or(JsValue::Null)),
        ("thirdEvents", JsValue::Array(third_events)),
        ("cancel", text(cancel.as_str())),
        ("cancelAgain", text(cancel_again.as_str())),
        ("cancelStarting", text(cancel_starting.as_str())),
        ("startingResult", starting_result),
        ("rows", JsValue::Array(rows)),
        ("fetch", fetch),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
        ("record", read_record(&home.join("lifecycle"))),
    ])
}

async fn subagents_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("subagents"));
    let fake = spec("fake");
    scripted(&fake, &["subagents"]);
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let run = outcome(
        manager
            .run_agent(
                AGENT_ID,
                AgentPromptInput::Text("delegate".to_owned()),
                None,
            )
            .await
            .map(|run| run.to_js()),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let queries = vec![
        outcome(
            manager
                .list_provider_subagents(AGENT_ID)
                .map(JsValue::Array),
        ),
        outcome(Ok(JsValue::Array(
            manager.list_provider_subagent_activity(),
        ))),
        outcome(
            manager
                .get_provider_subagent(AGENT_ID, "child-1")
                .map(|subagent| subagent.unwrap_or(JsValue::Null)),
        ),
        outcome(
            manager
                .get_provider_subagent(AGENT_ID, "nope")
                .map(|subagent| subagent.unwrap_or(JsValue::Null)),
        ),
        outcome(
            manager
                .fetch_provider_subagent_timeline(
                    AGENT_ID,
                    "child-1",
                    FetchDirection::Tail,
                    None,
                    Some(5),
                )
                .map(|page| page.to_js()),
        ),
        outcome(
            manager
                .list_provider_subagents(UNKNOWN_ID)
                .map(JsValue::Array),
        ),
    ];
    manager.close_agent(AGENT_ID).await.expect("close");
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("run", run),
        ("queries", JsValue::Array(queries)),
        (
            "after",
            JsValue::Array(manager.list_provider_subagent_activity()),
        ),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
    ])
}

async fn hydration_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("hydration"));
    let mut fake = spec("fake");
    fake.history = json(SCENARIO_TURNS).get("history").cloned();
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let mut steps = Vec::new();
    let options = [
        HydrateTimelineOptions::default(),
        HydrateTimelineOptions {
            force: true,
            broadcast: Some(HydrateBroadcast::Now(true)),
            broadcast_timeline: None,
        },
        HydrateTimelineOptions {
            force: true,
            broadcast: Some(HydrateBroadcast::Now(true)),
            broadcast_timeline: Some(false),
        },
        HydrateTimelineOptions {
            force: true,
            ..HydrateTimelineOptions::default()
        },
    ];
    for options in options {
        steps.push(outcome(
            manager
                .hydrate_timeline_from_provider(AGENT_ID, options)
                .await
                .map(|()| JsValue::Null),
        ));
        steps.push(JsValue::Array(
            manager.get_timeline_rows(AGENT_ID).expect("rows"),
        ));
    }
    steps.push(outcome(
        manager
            .hydrate_timeline_from_provider(UNKNOWN_ID, HydrateTimelineOptions::default())
            .await
            .map(|()| JsValue::Null),
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("steps", JsValue::Array(steps)),
        (
            "subagents",
            JsValue::Array(manager.list_provider_subagents(AGENT_ID).expect("list")),
        ),
        ("feed", JsValue::Array(feed)),
    ])
}

/// The resume errors: an unregistered client, an unavailable provider,
/// and a malformed agent id.
/// An archived agent resumed for history: its working directory is gone.
async fn resume_archived(manager: &AgentManager, registry: &AgentStorage) -> JsValue {
    registry
        .upsert(object(vec![
            ("id", text(OTHER_ID)),
            ("provider", text("fake")),
            ("cwd", text("/nonexistent/spocky-archived")),
            ("archivedAt", text("2026-07-01T00:00:00.000Z")),
        ]))
        .await
        .expect("archived record");
    outcome(
        manager
            .resume_agent_from_persistence(
                object(vec![
                    ("provider", text("fake")),
                    ("sessionId", text("sess-a")),
                    (
                        "metadata",
                        object(vec![("cwd", text("/nonexistent/spocky-archived"))]),
                    ),
                ]),
                None,
                Some(OTHER_ID.to_owned()),
                ResumeAgentOptions::default(),
                None,
            )
            .await
            .map(|agent| to_agent_payload(&agent.payload_view(), None).expect("payload")),
    )
}

async fn resume_errors(manager: &AgentManager, handle: &JsValue, cwd: &str) -> Vec<JsValue> {
    let mut results = Vec::new();
    for (provider, agent_id) in [("nope", None), ("gone", None), ("fake", Some("not-a-uuid"))] {
        let handle = if provider == "fake" {
            handle.clone()
        } else {
            object(vec![
                ("provider", text(provider)),
                ("sessionId", text("x")),
                ("metadata", object(vec![("cwd", text(cwd))])),
            ])
        };
        results.push(outcome(
            manager
                .resume_agent_from_persistence(
                    handle,
                    None,
                    agent_id.map(str::to_owned),
                    ResumeAgentOptions::default(),
                    None,
                )
                .await
                .map(|agent| text(&agent.id)),
        ));
    }
    results
}

async fn resume_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("resume"));
    let mut fake = spec("fake");
    fake.history = json(SCENARIO_TURNS).get("history").cloned();
    fake.initial_timeline = Some(json(
        r#"[{"item":{"type":"assistant_message","text":"startup row"},"timestamp":"2026-07-12T07:00:00.000Z"}]"#,
    ));
    let mut gone = spec("gone");
    gone.available = Ok(false);
    let manager = manager_with(
        &calls,
        &registry,
        vec![(fake, enabled()), (gone, enabled())],
    );
    let feed = record_feed(&manager);
    let handle = object(vec![
        ("provider", text("fake")),
        ("sessionId", text("sess-r")),
        ("nativeHandle", text("thread-r")),
        (
            "metadata",
            object(vec![
                ("cwd", text(cwd)),
                ("model", text("model-a")),
                ("title", text("Stored")),
            ]),
        ),
    ]);
    let mut results = vec![outcome(
        manager
            .resume_agent_from_persistence(
                handle.clone(),
                Some(object(vec![("modeId", text("auto"))])),
                Some(AGENT_ID.to_owned()),
                ResumeAgentOptions {
                    created_at_millis: Some(1_700_000_000_000),
                    updated_at_millis: Some(1_700_000_005_000),
                    last_user_message_at_millis: Some(1_700_000_004_000),
                    labels: Some(object(vec![("surface", text("workspace"))])),
                    workspace_id: Some("wks_9".to_owned()),
                    owner: None,
                    attention: Some(AgentAttention::Required {
                        reason: "finished".to_owned(),
                        timestamp_millis: 1_700_000_006_000,
                    }),
                },
                Some(AgentResumeSessionOptions {
                    purpose: Some(AgentResumePurpose::Interactive),
                }),
            )
            .await
            .map(|agent| to_agent_payload(&agent.payload_view(), None).expect("payload")),
    )];
    results.push(JsValue::Array(
        manager.get_timeline_rows(AGENT_ID).expect("rows"),
    ));
    results.push(outcome(
        manager
            .hydrate_timeline_from_provider(AGENT_ID, HydrateTimelineOptions::default())
            .await
            .map(|()| JsValue::Null),
    ));
    results.push(resume_archived(&manager, &registry).await);
    results.extend(resume_errors(&manager, &handle, cwd).await);
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
        (
            "stored",
            registry.get(AGENT_ID).await.unwrap_or(JsValue::Null),
        ),
    ])
}

async fn titles_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("titles"));
    let fake = spec("fake");
    scripted(&fake, &["held"]);
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let mut results = vec![
        JsValue::Bool(manager.has_in_flight_run(AGENT_ID)),
        JsValue::Bool(manager.has_in_flight_run(UNKNOWN_ID)),
    ];
    for (agent_id, title) in [
        (AGENT_ID, "  New title  "),
        (AGENT_ID, "   "),
        (UNKNOWN_ID, "x"),
    ] {
        results.push(outcome(
            manager
                .set_title(agent_id, title)
                .await
                .map(|()| JsValue::Null),
        ));
    }
    let held = tokio::spawn({
        let manager = manager.clone();
        async move {
            manager
                .run_agent(AGENT_ID, AgentPromptInput::Text("hold".to_owned()), None)
                .await
        }
    });
    wait_for_turn_started(&feed, "turn-5").await;
    results.push(JsValue::Bool(manager.has_in_flight_run(AGENT_ID)));
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    held.abort();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("feed", JsValue::Array(feed)),
        (
            "stored",
            registry.get(AGENT_ID).await.unwrap_or(JsValue::Null),
        ),
    ])
}

/// Replaces ISO timestamps with `<ISO>` and UUIDs other than
/// [`FIXED_IDS`] with `<UUID:n>`, `n` counting the distinct ones from 1.
/// The wall-clock window of one test run. `Date.now()` and `new Date()` on
/// either side give values inside it; a timestamp a fixture fixes is outside
/// it and is compared exactly.
struct WallClock {
    from_millis: i64,
}

impl WallClock {
    /// Slack either side of the run, for clock reads and process start.
    const SLACK_MILLIS: i64 = 2_000;

    fn start() -> Self {
        Self {
            from_millis: spocky_session::clock::now_millis() - Self::SLACK_MILLIS,
        }
    }

    /// Whether `iso` is a wall-clock value of this run.
    fn contains(&self, iso: &str) -> bool {
        spocky_store::time::parse_iso_millis(iso).is_some_and(|millis| {
            millis >= self.from_millis
                && millis <= spocky_session::clock::now_millis() + Self::SLACK_MILLIS
        })
    }
}

/// Masks the generated ids and the wall-clock timestamps of one run, and
/// nothing else: fixed ids and fixed fixture timestamps stay as they are.
fn normalize(text: &str, clock: &WallClock) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut generated: Vec<&str> = Vec::new();
    let mut index = 0;
    let digit = |offset: usize| bytes.get(offset).is_some_and(u8::is_ascii_digit);
    let hex = |offset: usize| bytes.get(offset).is_some_and(u8::is_ascii_hexdigit);
    while index < bytes.len() {
        let iso = (0..24).all(|offset| match offset {
            4 | 7 => bytes.get(index + offset) == Some(&b'-'),
            10 => bytes.get(index + offset) == Some(&b'T'),
            13 | 16 => bytes.get(index + offset) == Some(&b':'),
            19 => bytes.get(index + offset) == Some(&b'.'),
            23 => bytes.get(index + offset) == Some(&b'Z'),
            _ => digit(index + offset),
        });
        if iso && clock.contains(&text[index..index + 24]) {
            out.push_str("<ISO>");
            index += 24;
            continue;
        }
        let uuid = (0..36).all(|offset| match offset {
            8 | 13 | 18 | 23 => bytes.get(index + offset) == Some(&b'-'),
            _ => hex(index + offset),
        });
        if uuid {
            let id = &text[index..index + 36];
            if FIXED_IDS.contains(&id) {
                out.push_str(id);
            } else {
                let number = generated
                    .iter()
                    .position(|seen| *seen == id)
                    .unwrap_or_else(|| {
                        generated.push(id);
                        generated.len() - 1
                    })
                    + 1;
                out.push_str("<UUID:");
                out.push_str(&number.to_string());
                out.push('>');
            }
            index += 36;
            continue;
        }
        let character = text[index..].chars().next().expect("character");
        out.push(character);
        index += character.len_utf8();
    }
    out
}

#[test]
fn normalize_masks_only_wall_clock_values() {
    let clock = WallClock::start();
    let now = spocky_session::clock::now_iso();
    let text = format!(
        r#"["{now}","2026-07-12T08:00:00.000Z","2031-01-02T03:04:05.678Z","3f2b8c1e-9a4d-4e6f-8b7a-1c2d3e4f5a6b","{AGENT_ID}","{UNKNOWN_ID}x","7a1b8c1e-9a4d-4e6f-8b7a-1c2d3e4f5a6b","3f2b8c1e-9a4d-4e6f-8b7a-1c2d3e4f5a6b"]"#
    );
    assert_eq!(
        normalize(&text, &clock),
        format!(
            r#"["<ISO>","2026-07-12T08:00:00.000Z","2031-01-02T03:04:05.678Z","<UUID:1>","{AGENT_ID}","{UNKNOWN_ID}x","<UUID:2>","<UUID:1>"]"#
        )
    );
}

/// The pinned dist modules this test runs, relative to `SPOCKY_PASEO_DIST`,
/// with their SHA-256.
const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/agent/agent-manager.js",
        "09e1a170a75fc6b1f4eca29779feb7ada0c6d607bd33588f545e619174d7fa65",
    ),
    (
        "server/agent/agent-storage.js",
        "f1e3ccb1cf1e4caf75084627304450ddab8f93098291049819b312174f1e17ea",
    ),
    (
        "server/agent/agent-projections.js",
        "725258d3cf93e0de535d27fc245d776983303bc4c7c8874141ed9277516bb690",
    ),
    (
        "server/agent/agent-stream-coalescer.js",
        "391e756b43b6ebafda726effc38c033ab916395947a62e27297e983d94b25b99",
    ),
    (
        "server/agent/agent-timeline-store.js",
        "5473e829162d3256ab76b9b39d965158efbf5b4a29bb01e444626ac3a4bf52e3",
    ),
    (
        "server/plugins/lifecycle/index.js",
        "fff7a2629df50bfff0e7837e34adc13f4876e0760d233509da1f313c98039546",
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

struct Home(std::path::PathBuf);

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn home(name: &str) -> Home {
    let path = std::env::temp_dir().join(format!("spocky-manager-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("disposable home");
    Home(path)
}

#[allow(
    clippy::too_many_lines,
    reason = "one line per scenario the node twin runs"
)]
#[tokio::test]
async fn scenarios_match_pinned_manager() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: manager differential not run");
            return;
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    };
    let clock = WallClock::start();
    assert_pinned_modules(&dist);
    let workspace = home("cwd");
    let cwd = std::fs::canonicalize(&workspace.0)
        .expect("cwd")
        .to_string_lossy()
        .into_owned();
    let node_home = home("node-records");
    let rust_home = home("rust-records");
    let timeout = if Command::new("gtimeout").arg("--version").output().is_ok() {
        "gtimeout"
    } else {
        "timeout"
    };
    let output = Command::new(timeout)
        .args(["--kill-after=5", "120"])
        .arg(&node)
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .arg(&dist)
        .args([
            AGENT_ID,
            OTHER_ID,
            UNKNOWN_ID,
            TURN_EVENTS,
            SCENARIO_TURNS,
            ERROR_CASES,
            RUNTIME_INFO,
            PERSISTENCE,
            CAPABILITIES,
            MODES,
            CATALOG,
            &cwd,
        ])
        .arg(&node_home.0)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = normalize(&String::from_utf8_lossy(&output.stdout), &clock);
    let rust = object(vec![
        ("main", main_scenario(&cwd, &rust_home.0).await),
        ("errors", errors_scenario(&cwd, &rust_home.0).await),
        ("turns", turns_scenario(&cwd, &rust_home.0).await),
        ("permission", permission_scenario(&cwd, &rust_home.0).await),
        ("lifecycle", lifecycle_scenario(&cwd, &rust_home.0).await),
        ("subagents", subagents_scenario(&cwd, &rust_home.0).await),
        ("hydration", hydration_scenario(&cwd, &rust_home.0).await),
        ("resume", resume_scenario(&cwd, &rust_home.0).await),
        ("titles", titles_scenario(&cwd, &rust_home.0).await),
        ("runstart", runstart_scenario(&cwd, &rust_home.0).await),
        ("outofband", outofband_scenario(&cwd, &rust_home.0).await),
        ("shutdown", shutdown_scenario(&cwd, &rust_home.0).await),
        ("loading", loading_scenario(&cwd, &rust_home.0).await),
        ("replace", replace_scenario(&cwd, &rust_home.0).await),
        ("rewind", rewind_scenario(&cwd, &rust_home.0).await),
        (
            "timelineItems",
            timeline_items_scenario(&cwd, &rust_home.0).await,
        ),
        ("availability", availability_scenario(&rust_home.0).await),
        ("importable", importable_scenario(&rust_home.0).await),
        ("draft", draft_scenario(&cwd, &rust_home.0).await),
        ("registry", registry_scenario(&cwd, &rust_home.0).await),
        ("callbacks", callbacks_scenario(&cwd, &rust_home.0).await),
        (
            "persistFailure",
            persist_failure_scenario(&cwd, &rust_home.0).await,
        ),
        ("catalog", catalog_scenario(&cwd, &rust_home.0).await),
        (
            "pluginLifecycle",
            plugin_lifecycle_scenario(&cwd, &rust_home.0).await,
        ),
        ("trace", trace_scenario(&cwd, &rust_home.0).await),
        ("steer", steer_scenario(&cwd, &rust_home.0).await),
        ("settings", settings_scenario(&cwd, &rust_home.0).await),
        ("metadata", metadata_scenario(&cwd, &rust_home.0).await),
        ("cancelLogs", cancel_logs_scenario(&cwd, &rust_home.0).await),
        (
            "failureLogs",
            failure_logs_scenario(&cwd, &rust_home.0).await,
        ),
        ("reload", reload_scenario(&cwd, &rust_home.0).await),
        ("import", import_scenario(&cwd, &rust_home.0).await),
        ("archive", archive_scenario(&cwd, &rust_home.0).await),
        (
            "storedDates",
            stored_dates_scenario(&cwd, &rust_home.0).await,
        ),
    ]);
    assert_eq!(normalize(&stringify(&rust), &clock), expected);
}

async fn collect_stream(mut stream: TurnEventStream, events: &mut Vec<JsValue>) {
    while let Some(event) = stream.next().await {
        events.push(event.expect("event"));
    }
}

async fn replace_manager(
    name: &str,
    turns: &[&str],
    home: &Path,
    cwd: &str,
    configure: impl FnOnce(&mut Spec, &mut AgentManagerOptions),
) -> (AgentManager, AgentStorage, Calls, Feed) {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join(format!("replace-{name}")));
    let mut fake = spec("fake");
    scripted(&fake, turns);
    let mut options = AgentManagerOptions::default();
    configure(&mut fake, &mut options);
    let client = Arc::new(FakeClient {
        spec: fake,
        calls: Arc::clone(&calls),
    }) as Arc<dyn AgentClient>;
    options.clients = vec![("fake".to_owned(), client)];
    options.provider_definitions = vec![("fake".to_owned(), enabled())];
    options.registry = Some(registry.clone());
    let manager = AgentManager::new(options);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    (manager, registry, calls, feed)
}

async fn replace_finish(
    manager: &AgentManager,
    registry: &AgentStorage,
    calls: &Calls,
    feed: &Feed,
    mut extra: Vec<(&'static str, JsValue)>,
) -> JsValue {
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let agent = manager.get_agent(AGENT_ID).expect("agent");
    extra.push((
        "calls",
        JsValue::Array(calls.lock().expect("calls").clone()),
    ));
    extra.push(("feed", JsValue::Array(feed.lock().expect("feed").clone())));
    extra.push((
        "agent",
        to_agent_payload(&agent.payload_view(), None).expect("payload"),
    ));
    extra.push((
        "rows",
        JsValue::Array(manager.get_timeline_rows(AGENT_ID).expect("rows")),
    ));
    extra.push((
        "stored",
        registry.get(AGENT_ID).await.unwrap_or(JsValue::Null),
    ));
    object(extra)
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn replace_scenario(cwd: &str, home: &Path) -> JsValue {
    let scripted_turns = json(SCENARIO_TURNS);
    let interrupt = scripted_turns.get("interrupt").cloned();
    let text_prompt = |prompt: &str| AgentPromptInput::Text(prompt.to_owned());
    let to_array = |events: Vec<JsValue>| JsValue::Array(events);

    let (manager, registry, calls, feed) = replace_manager(
        "running",
        &["rpIdle", "long", "rpAfter"],
        home,
        cwd,
        |fake, _| fake.interrupt = interrupt,
    )
    .await;
    let mut idle_events = Vec::new();
    collect_stream(
        manager
            .replace_agent_run(AGENT_ID, text_prompt("idle prompt"), None)
            .await
            .expect("idle replace"),
        &mut idle_events,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut old = manager
        .stream_agent(AGENT_ID, text_prompt("long task"), None)
        .expect("old stream");
    let mut old_events = vec![old.next().await.expect("first").expect("event")];
    tokio::time::sleep(Duration::from_millis(50)).await;
    let replacement = manager
        .replace_agent_run(AGENT_ID, text_prompt("replace it"), None)
        .await
        .expect("replace");
    collect_stream(old, &mut old_events).await;
    let mut new_events = Vec::new();
    collect_stream(replacement, &mut new_events).await;
    let a = replace_finish(
        &manager,
        &registry,
        &calls,
        &feed,
        vec![
            ("idleEvents", to_array(idle_events)),
            ("oldEvents", to_array(old_events)),
            ("newEvents", to_array(new_events)),
        ],
    )
    .await;

    let (manager, registry, calls, feed) = replace_manager(
        "finishing",
        &["rpFinishing", "rpAfterFinish"],
        home,
        cwd,
        |_, _| {},
    )
    .await;
    let mut first = manager
        .stream_agent(AGENT_ID, text_prompt("finishing"), None)
        .expect("first stream");
    let mut first_events = vec![first.next().await.expect("first").expect("event")];
    tokio::time::sleep(Duration::from_millis(30)).await;
    let second = manager
        .replace_agent_run(AGENT_ID, text_prompt("while finishing"), None)
        .await
        .expect("replace");
    collect_stream(first, &mut first_events).await;
    let mut second_events = Vec::new();
    collect_stream(second, &mut second_events).await;
    // The replacement starts only after the finishing run has ended.
    let stream_at = |kind: &str, turn_id: &str| {
        feed.lock().expect("feed").iter().position(|entry| {
            let entry = entry.as_array().expect("entry");
            entry[0].as_str() == Some("agent_stream")
                && entry[2].get("type").and_then(JsValue::as_str) == Some(kind)
                && entry[2].get("turnId").and_then(JsValue::as_str) == Some(turn_id)
        })
    };
    let waited = stream_at("turn_completed", "turn-14")
        .is_some_and(|ended| Some(ended) < stream_at("turn_started", "turn-16"));
    let b = replace_finish(
        &manager,
        &registry,
        &calls,
        &feed,
        vec![
            ("firstEvents", to_array(first_events)),
            ("secondEvents", to_array(second_events)),
            ("waited", JsValue::Bool(waited)),
        ],
    )
    .await;

    let (manager, registry, calls, feed) =
        replace_manager("refused", &["rpHeld"], home, cwd, |fake, options| {
            fake.interrupt_hang = true;
            options.rescue_interrupt_session_ms = Some(80);
        })
        .await;
    let mut held = manager
        .stream_agent(AGENT_ID, text_prompt("long task"), None)
        .expect("held stream");
    let held_events = vec![held.next().await.expect("first").expect("event")];
    tokio::time::sleep(Duration::from_millis(50)).await;
    let failure = match manager
        .replace_agent_run(AGENT_ID, text_prompt("never"), None)
        .await
    {
        Ok(_) => outcome(Ok(JsValue::Null)),
        Err(error) => outcome(Err(error)),
    };
    let again = outcome(
        manager
            .stream_agent(AGENT_ID, text_prompt("again"), None)
            .map(|_| JsValue::Null),
    );
    let mut held_events = held_events;
    collect_stream(held, &mut held_events).await;
    let c = replace_finish(
        &manager,
        &registry,
        &calls,
        &feed,
        vec![
            ("heldEvents", to_array(held_events)),
            ("failure", failure),
            ("again", again),
        ],
    )
    .await;

    let (manager, registry, calls, feed) = replace_manager(
        "starting",
        &["slowStart", "rpAfterFinish"],
        home,
        cwd,
        |_, _| {},
    )
    .await;
    let slow = manager
        .stream_agent(AGENT_ID, text_prompt("slow"), None)
        .expect("slow stream");
    let slow_done = tokio::spawn(async move {
        let mut events = Vec::new();
        collect_stream(slow, &mut events).await;
        events
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let after_slow = manager
        .replace_agent_run(AGENT_ID, text_prompt("while starting"), None)
        .await
        .expect("replace");
    let slow_events = slow_done.await.expect("slow events");
    let mut after_slow_events = Vec::new();
    collect_stream(after_slow, &mut after_slow_events).await;
    let d = replace_finish(
        &manager,
        &registry,
        &calls,
        &feed,
        vec![
            ("slowEvents", to_array(slow_events)),
            ("afterSlowEvents", to_array(after_slow_events)),
        ],
    )
    .await;
    object(vec![("a", a), ("b", b), ("c", c), ("d", d)])
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn import_scenario(cwd: &str, home: &Path) -> JsValue {
    let fixture = json(SCENARIO_TURNS);
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("import"));
    let mut importing = spec("fake");
    importing.import = fixture.get("import").cloned();
    importing.history = fixture.get("history").cloned();
    scripted(&importing, &["impTurn"]);
    let mut bad = spec("badimport");
    bad.import = fixture.get("badImport").cloned();
    let mut bad_timeline = spec("badtimeline");
    bad_timeline.import = fixture.get("badTimeline").cloned();
    let mut bad_subagent = spec("badsubagent");
    bad_subagent.import = fixture.get("badSubagent").cloned();
    let manager = manager_with(
        &calls,
        &registry,
        vec![
            (importing, enabled()),
            (spec("plain"), enabled()),
            (bad, enabled()),
            (bad_timeline, enabled()),
            (bad_subagent, enabled()),
        ],
    );
    let feed = record_feed(&manager);
    let run = |provider: &'static str| {
        let manager = manager.clone();
        let cwd = cwd.to_owned();
        async move {
            outcome(
                manager
                    .import_provider_session(ImportProviderSessionRequest {
                        provider: provider.to_owned(),
                        provider_handle_id: "h1".to_owned(),
                        cwd,
                        workspace_id: "wks_9".to_owned(),
                        labels: Some(object(vec![("a", text("b"))])),
                    })
                    .await
                    .map(|agent| to_agent_payload(&agent.payload_view(), None).expect("payload")),
            )
        }
    };
    let results = vec![
        run("fake").await,
        run("plain").await,
        run("nope").await,
        run("badimport").await,
        run("badtimeline").await,
        run("badsubagent").await,
    ];
    tokio::time::sleep(Duration::from_millis(50)).await;
    manager.flush().await;
    registry.flush().await;
    let registered = feed.lock().expect("feed").clone();
    let mut ids: Vec<String> = Vec::new();
    for entry in &registered {
        let entry = entry.as_array().expect("entry");
        if entry[0].as_str() == Some("agent_state")
            && let Some(id) = entry[1].get("id").and_then(JsValue::as_str)
            && !ids.iter().any(|known| known == id)
        {
            ids.push(id.to_owned());
        }
    }
    // An imported agent's history is already primed, so this is a no-op.
    manager
        .hydrate_timeline_from_provider(&ids[0], HydrateTimelineOptions::default())
        .await
        .expect("hydrate");
    let mut turn_events = Vec::new();
    collect_stream(
        manager
            .stream_agent(
                &ids[0],
                AgentPromptInput::Text("after import".to_owned()),
                None,
            )
            .expect("stream"),
        &mut turn_events,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    manager.flush().await;
    registry.flush().await;
    let feed = feed.lock().expect("feed").clone();
    let mut rows = JsObject::new();
    let mut stored = JsObject::new();
    let mut subagents = Vec::new();
    for id in &ids {
        rows.insert(
            id.as_str(),
            JsValue::Array(manager.get_timeline_rows(id).expect("rows")),
        );
        subagents.push(JsValue::Array(
            manager.list_provider_subagents(id).expect("subagents"),
        ));
        stored.insert(id.as_str(), registry.get(id).await.unwrap_or(JsValue::Null));
    }
    let calls = calls.lock().expect("calls").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
        ("turnEvents", JsValue::Array(turn_events)),
        ("rows", JsValue::Object(rows)),
        ("subagents", JsValue::Array(subagents)),
        ("stored", JsValue::Object(stored)),
    ])
}

async fn rewind_outcome(
    manager: &AgentManager,
    id: &str,
    message_id: &str,
    mode: RewindMode,
) -> JsValue {
    outcome(
        manager
            .rewind(id, message_id, mode)
            .await
            .map(|()| JsValue::Null),
    )
}

fn rewind_client(spec: Spec, calls: &Calls) -> Arc<dyn AgentClient> {
    Arc::new(FakeClient {
        spec,
        calls: Arc::clone(calls),
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn rewind_scenario(cwd: &str, home: &Path) -> JsValue {
    const NO_FLAG_ID: &str = "00000000-0000-4000-8000-0000000000f2";
    let fixture = json(SCENARIO_TURNS);
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("rewind"));
    let warns: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let warn_sink = Arc::clone(&warns);
    let infos: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let info_sink = Arc::clone(&infos);
    let mut fake = spec("fake");
    scripted(&fake, &["rwEcho", "rwNoEcho", "long"]);
    fake.history = fixture.get("history").cloned();
    fake.interrupt = fixture.get("interrupt").cloned();
    fake.capabilities = json(REWIND_CAPABILITIES);
    fake.revert = vec!["conversation", "files"];
    let mut no_flag = spec("noflag");
    no_flag.history = fixture.get("history").cloned();
    no_flag.revert = vec!["conversation", "files", "both"];
    let manager = AgentManager::new(AgentManagerOptions {
        clients: vec![
            ("fake".to_owned(), rewind_client(fake, &calls)),
            ("noflag".to_owned(), rewind_client(no_flag, &calls)),
        ],
        provider_definitions: vec![
            ("fake".to_owned(), enabled()),
            ("noflag".to_owned(), enabled()),
        ],
        registry: Some(registry.clone()),
        log_warn: Some(Arc::new(move |bindings, message| {
            warn_sink
                .lock()
                .expect("warns")
                .push(JsValue::Array(vec![bindings, text(message)]));
        })),
        log_info: Some(Arc::new(move |bindings, message| {
            info_sink
                .lock()
                .expect("infos")
                .push(JsValue::Array(vec![bindings, text(message)]));
        })),
        ..AgentManagerOptions::default()
    });
    let feed = record_feed(&manager);
    for (provider, id) in [("fake", AGENT_ID), ("noflag", NO_FLAG_ID)] {
        manager
            .create_agent(
                object(vec![("provider", text(provider)), ("cwd", text(cwd))]),
                Some(id.to_owned()),
                CreateAgentOptions::default(),
            )
            .await
            .expect("create");
    }
    let acknowledged = |id: &str| AgentRunOptions {
        client_message_id: Some(id.to_owned()),
        ..AgentRunOptions::default()
    };
    let mut echo_events = Vec::new();
    collect_stream(
        manager
            .stream_agent(
                AGENT_ID,
                AgentPromptInput::Text("rewind me".to_owned()),
                Some(acknowledged("client-rw")),
            )
            .expect("echo stream"),
        &mut echo_events,
    )
    .await;
    let mut no_echo_events = Vec::new();
    collect_stream(
        manager
            .stream_agent(
                AGENT_ID,
                AgentPromptInput::Text("no ack".to_owned()),
                Some(acknowledged("client-noack")),
            )
            .expect("no echo stream"),
        &mut no_echo_events,
    )
    .await;
    let rewind = |id: &'static str, message_id: &'static str, mode: RewindMode| {
        rewind_outcome(&manager, id, message_id, mode)
    };
    let mut results = JsObject::new();
    results.insert(
        "unknownAgent",
        rewind(
            "00000000-0000-4000-8000-0000000000f3",
            "x",
            RewindMode::Files,
        )
        .await,
    );
    results.insert(
        "unacknowledged",
        rewind(AGENT_ID, "client-noack", RewindMode::Conversation).await,
    );
    results.insert(
        "files",
        rewind(AGENT_ID, "unknown-message", RewindMode::Files).await,
    );
    results.insert(
        "both",
        rewind(AGENT_ID, "client-rw", RewindMode::Both).await,
    );
    results.insert(
        "noFlag",
        rewind(NO_FLAG_ID, "x", RewindMode::Conversation).await,
    );
    let mut held = manager
        .stream_agent(AGENT_ID, AgentPromptInput::Text("hold".to_owned()), None)
        .expect("held stream");
    let mut held_events = vec![held.next().await.expect("first").expect("event")];
    tokio::time::sleep(Duration::from_millis(50)).await;
    results.insert(
        "running",
        rewind(AGENT_ID, "client-rw", RewindMode::Files).await,
    );
    // A rewind that did not cancel the run would leave this stream open.
    tokio::time::timeout(
        Duration::from_secs(3),
        collect_stream(held, &mut held_events),
    )
    .await
    .expect("the running turn was not cancelled by rewind");
    results.insert(
        "conversation",
        rewind(AGENT_ID, "client-rw", RewindMode::Conversation).await,
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;

    let refused_calls = Calls::default();
    let refused_registry = AgentStorage::new(home.join("rewind-refused"));
    let mut hanging = spec("fake");
    scripted(&hanging, &["rpHeld"]);
    hanging.interrupt_hang = true;
    hanging.capabilities = json(REWIND_CAPABILITIES);
    hanging.revert = vec!["files"];
    let refused = AgentManager::new(AgentManagerOptions {
        clients: vec![("fake".to_owned(), rewind_client(hanging, &refused_calls))],
        provider_definitions: vec![("fake".to_owned(), enabled())],
        registry: Some(refused_registry.clone()),
        rescue_interrupt_session_ms: Some(80),
        ..AgentManagerOptions::default()
    });
    refused
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let mut refused_held = refused
        .stream_agent(
            AGENT_ID,
            AgentPromptInput::Text("long task".to_owned()),
            None,
        )
        .expect("refused stream");
    let mut refused_events = vec![refused_held.next().await.expect("first").expect("event")];
    tokio::time::sleep(Duration::from_millis(50)).await;
    let refused_result = rewind_outcome(&refused, AGENT_ID, "x", RewindMode::Files).await;
    collect_stream(refused_held, &mut refused_events).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    refused.flush().await;
    refused_registry.flush().await;
    let payload = |manager: &AgentManager| {
        to_agent_payload(
            &manager.get_agent(AGENT_ID).expect("agent").payload_view(),
            None,
        )
        .expect("payload")
    };
    let stored = registry.get(AGENT_ID).await.unwrap_or(JsValue::Null);
    object(vec![
        ("results", JsValue::Object(results)),
        ("echoEvents", JsValue::Array(echo_events)),
        ("noEchoEvents", JsValue::Array(no_echo_events)),
        ("heldEvents", JsValue::Array(held_events)),
        (
            "calls",
            JsValue::Array(calls.lock().expect("calls").clone()),
        ),
        ("feed", JsValue::Array(feed.lock().expect("feed").clone())),
        (
            "warns",
            JsValue::Array(warns.lock().expect("warns").clone()),
        ),
        (
            "infos",
            JsValue::Array(infos.lock().expect("infos").clone()),
        ),
        ("agent", payload(&manager)),
        (
            "rows",
            JsValue::Array(manager.get_timeline_rows(AGENT_ID).expect("rows")),
        ),
        ("stored", stored),
        (
            "refused",
            object(vec![
                ("result", refused_result),
                ("events", JsValue::Array(refused_events)),
                (
                    "calls",
                    JsValue::Array(refused_calls.lock().expect("calls").clone()),
                ),
                ("agent", payload(&refused)),
            ]),
        ),
    ])
}

/// One cancel case: a held turn on a fake provider, cancelled once.
async fn cancel_logs_case(
    name: &str,
    turns: &[&str],
    home: &Path,
    cwd: &str,
    logs: &Arc<Mutex<Vec<JsValue>>>,
    wait_ms: u64,
    configure: impl FnOnce(&mut Spec, &mut AgentManagerOptions),
) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join(format!("cancel-{name}")));
    let mut fake = spec("fake");
    scripted(&fake, turns);
    let warn_sink = Arc::clone(logs);
    let error_sink = Arc::clone(logs);
    let mut options = AgentManagerOptions {
        log_warn: Some(Arc::new(move |bindings, message| {
            warn_sink.lock().expect("logs").push(JsValue::Array(vec![
                text("warn"),
                bindings,
                text(message),
            ]));
        })),
        log_error: Some(Arc::new(move |bindings, message| {
            error_sink.lock().expect("logs").push(JsValue::Array(vec![
                text("error"),
                bindings,
                text(message),
            ]));
        })),
        ..AgentManagerOptions::default()
    };
    configure(&mut fake, &mut options);
    options.clients = vec![("fake".to_owned(), rewind_client(fake, &calls))];
    options.provider_definitions = vec![("fake".to_owned(), enabled())];
    options.registry = Some(registry.clone());
    let manager = AgentManager::new(options);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let mut held = manager
        .stream_agent(AGENT_ID, AgentPromptInput::Text("hold".to_owned()), None)
        .expect("held stream");
    let mut held_events = vec![held.next().await.expect("first").expect("event")];
    tokio::time::sleep(Duration::from_millis(50)).await;
    let result = outcome(
        manager
            .cancel_agent_run(AGENT_ID)
            .await
            .map(|status| object(vec![("status", text(status.as_str()))])),
    );
    if wait_ms > 0 {
        tokio::time::sleep(Duration::from_millis(wait_ms)).await;
    }
    collect_stream(held, &mut held_events).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let agent = to_agent_payload(
        &manager.get_agent(AGENT_ID).expect("agent").payload_view(),
        None,
    )
    .expect("payload");
    object(vec![
        ("result", result),
        ("heldEvents", JsValue::Array(held_events)),
        (
            "calls",
            JsValue::Array(calls.lock().expect("calls").clone()),
        ),
        ("feed", JsValue::Array(feed.lock().expect("feed").clone())),
        ("agent", agent),
    ])
}

async fn cancel_logs_scenario(cwd: &str, home: &Path) -> JsValue {
    let logs: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let quick = |options: &mut AgentManagerOptions| options.rescue_interrupt_session_ms = Some(80);
    let hang = cancel_logs_case("hang", &["rpHeld"], home, cwd, &logs, 0, |fake, options| {
        fake.interrupt_hang = true;
        quick(options);
    })
    .await;
    let fails = cancel_logs_case(
        "fails",
        &["rpHeld"],
        home,
        cwd,
        &logs,
        0,
        |fake, options| {
            fake.interrupt_fails = true;
            quick(options);
        },
    )
    .await;
    let late = cancel_logs_case(
        "late",
        &["rpHeld"],
        home,
        cwd,
        &logs,
        200,
        |fake, options| {
            fake.interrupt_late_fail_ms = Some(150);
            quick(options);
        },
    )
    .await;
    let force = cancel_logs_case("force", &["long"], home, cwd, &logs, 0, |_, _| {}).await;
    let logs = JsValue::Array(logs.lock().expect("logs").clone());
    object(vec![
        (
            "cases",
            object(vec![
                ("hang", hang),
                ("fails", fails),
                ("late", late),
                ("force", force),
            ]),
        ),
        ("logs", logs),
    ])
}

/// A manager over one provider whose agent `AGENT_ID` exists, for the
/// reload cases.
async fn reload_manager(
    name: &str,
    provider: &str,
    turns: &[&str],
    home: &Path,
    cwd: &str,
    configure: impl FnOnce(&mut Spec, &mut AgentManagerOptions),
) -> (
    AgentManager,
    AgentStorage,
    Calls,
    Feed,
    Arc<Mutex<Vec<JsValue>>>,
) {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join(format!("reload-{name}")));
    let warns: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let warn_sink = Arc::clone(&warns);
    let mut fake = spec(provider);
    scripted(&fake, turns);
    fake.interrupt = json(SCENARIO_TURNS).get("interrupt").cloned();
    let mut options = AgentManagerOptions {
        log_warn: Some(Arc::new(move |bindings, message| {
            warn_sink
                .lock()
                .expect("warns")
                .push(JsValue::Array(vec![bindings, text(message)]));
        })),
        ..AgentManagerOptions::default()
    };
    configure(&mut fake, &mut options);
    options.clients = vec![(provider.to_owned(), rewind_client(fake, &calls))];
    options.provider_definitions = vec![(provider.to_owned(), enabled())];
    options.registry = Some(registry.clone());
    let manager = AgentManager::new(options);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text(provider)), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions {
                labels: Some(object(vec![("lane", text("reload"))])),
                workspace_id: Some("wks_1".to_owned()),
                ..CreateAgentOptions::default()
            },
        )
        .await
        .expect("create");
    (manager, registry, calls, feed, warns)
}

async fn reload_outcome(
    manager: &AgentManager,
    id: &str,
    overrides: Option<JsValue>,
    options: ReloadAgentOptions,
) -> JsValue {
    outcome(
        manager
            .reload_agent_session(id, overrides, options)
            .await
            .map(|agent| to_agent_payload(&agent.payload_view(), None).expect("payload")),
    )
}

async fn reload_finish(
    parts: &(
        AgentManager,
        AgentStorage,
        Calls,
        Feed,
        Arc<Mutex<Vec<JsValue>>>,
    ),
    mut extra: Vec<(&'static str, JsValue)>,
) -> JsValue {
    let (manager, registry, calls, feed, _) = parts;
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let agent = manager.get_agent(AGENT_ID);
    extra.push((
        "calls",
        JsValue::Array(calls.lock().expect("calls").clone()),
    ));
    extra.push(("feed", JsValue::Array(feed.lock().expect("feed").clone())));
    extra.push((
        "agent",
        agent.as_ref().map_or(JsValue::Null, |agent| {
            to_agent_payload(&agent.payload_view(), None).expect("payload")
        }),
    ));
    extra.push((
        "rows",
        agent.as_ref().map_or(JsValue::Null, |_| {
            JsValue::Array(manager.get_timeline_rows(AGENT_ID).expect("rows"))
        }),
    ));
    extra.push((
        "subagents",
        agent.as_ref().map_or(JsValue::Null, |_| {
            JsValue::Array(
                manager
                    .list_provider_subagents(AGENT_ID)
                    .expect("subagents"),
            )
        }),
    ));
    extra.push((
        "stored",
        registry.get(AGENT_ID).await.unwrap_or(JsValue::Null),
    ));
    object(extra)
}

async fn reload_run(manager: &AgentManager, prompt: &str) -> Vec<JsValue> {
    let mut events = Vec::new();
    collect_stream(
        manager
            .stream_agent(AGENT_ID, AgentPromptInput::Text(prompt.to_owned()), None)
            .expect("stream"),
        &mut events,
    )
    .await;
    events
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn reload_scenario(cwd: &str, home: &Path) -> JsValue {
    let none = ReloadAgentOptions::default();
    let to_array = JsValue::Array;

    let idle = reload_manager("idle", "fake", &["rpIdle"], home, cwd, |fake, _| {
        fake.history = json(SCENARIO_TURNS).get("history").cloned();
    })
    .await;
    let idle_events = reload_run(&idle.0, "before reload").await;
    let result = reload_outcome(
        &idle.0,
        AGENT_ID,
        Some(object(vec![
            ("title", text("Reloaded")),
            ("modeId", text("read-only")),
        ])),
        none,
    )
    .await;
    // The reloaded agent's history stays primed, so this is a no-op.
    idle.0
        .hydrate_timeline_from_provider(AGENT_ID, HydrateTimelineOptions::default())
        .await
        .expect("hydrate");
    let idle_case = reload_finish(
        &idle,
        vec![("idleEvents", to_array(idle_events)), ("result", result)],
    )
    .await;

    let rehydrate = reload_manager("rehydrate", "fake", &["subagents"], home, cwd, |fake, _| {
        fake.history = json(SCENARIO_TURNS).get("history").cloned();
    })
    .await;
    let rehydrate_events = reload_run(&rehydrate.0, "delegate").await;
    // Hydrating once primes the history, which a rehydrating reload must drop.
    rehydrate
        .0
        .hydrate_timeline_from_provider(AGENT_ID, HydrateTimelineOptions::default())
        .await
        .expect("hydrate");
    let result = reload_outcome(
        &rehydrate.0,
        AGENT_ID,
        None,
        ReloadAgentOptions {
            rehydrate_from_disk: true,
        },
    )
    .await;
    // The history is no longer primed, so this hydrates again.
    rehydrate
        .0
        .hydrate_timeline_from_provider(AGENT_ID, HydrateTimelineOptions::default())
        .await
        .expect("hydrate");
    let rehydrate_case = reload_finish(
        &rehydrate,
        vec![
            ("rehydrateEvents", to_array(rehydrate_events)),
            ("result", result),
        ],
    )
    .await;

    let running = reload_manager("running", "fake", &["long"], home, cwd, |_, _| {}).await;
    let mut held = running
        .0
        .stream_agent(
            AGENT_ID,
            AgentPromptInput::Text("long task".to_owned()),
            None,
        )
        .expect("held stream");
    let mut held_events = vec![held.next().await.expect("first").expect("event")];
    tokio::time::sleep(Duration::from_millis(50)).await;
    let running_result = reload_outcome(&running.0, AGENT_ID, None, none).await;
    collect_stream(held, &mut held_events).await;
    let running_case = reload_finish(
        &running,
        vec![
            ("heldEvents", to_array(held_events)),
            ("result", running_result),
        ],
    )
    .await;

    let refused = reload_manager(
        "refused",
        "fake",
        &["rpHeld"],
        home,
        cwd,
        |fake, options| {
            fake.interrupt = None;
            fake.interrupt_hang = true;
            options.rescue_interrupt_session_ms = Some(80);
        },
    )
    .await;
    let mut refused_held = refused
        .0
        .stream_agent(
            AGENT_ID,
            AgentPromptInput::Text("long task".to_owned()),
            None,
        )
        .expect("refused stream");
    let mut refused_events = vec![refused_held.next().await.expect("first").expect("event")];
    tokio::time::sleep(Duration::from_millis(50)).await;
    let refused_result = reload_outcome(&refused.0, AGENT_ID, None, none).await;
    collect_stream(refused_held, &mut refused_events).await;
    let refused_case = reload_finish(
        &refused,
        vec![
            ("refusedEvents", to_array(refused_events)),
            ("result", refused_result),
        ],
    )
    .await;

    let no_mcp = reload_manager("nomcp", "nomcp", &[], home, cwd, |fake, _| {
        fake.capabilities = json(NO_MCP_CAPABILITIES);
        fake.no_persistence = true;
    })
    .await;
    let mcp_override = object(vec![(
        "mcpServers",
        object(vec![(
            "a",
            object(vec![("type", text("stdio")), ("command", text("echo"))]),
        )]),
    )]);
    let result = reload_outcome(&no_mcp.0, AGENT_ID, Some(mcp_override.clone()), none).await;
    let no_mcp_case = reload_finish(&no_mcp, vec![("result", result)]).await;

    // The persistence handle names a provider with no client.
    let no_client = reload_manager("noclient", "nomcp", &[], home, cwd, |fake, _| {
        fake.capabilities = json(NO_MCP_CAPABILITIES);
    })
    .await;
    let result = reload_outcome(&no_client.0, AGENT_ID, Some(mcp_override), none).await;
    let no_client_case = reload_finish(&no_client, vec![("result", result)]).await;

    // The last error and the last usage survive a reload.
    let failed_run = reload_manager("lasterror", "fake", &["failed"], home, cwd, |_, _| {}).await;
    let failed_events = reload_run(&failed_run.0, "fail me").await;
    let result = reload_outcome(&failed_run.0, AGENT_ID, None, none).await;
    let failed_case = reload_finish(
        &failed_run,
        vec![
            ("failedEvents", to_array(failed_events)),
            ("result", result),
        ],
    )
    .await;
    let usage_run = reload_manager("lastusage", "fake", &[], home, cwd, |_, _| {}).await;
    let usage_events = reload_run(&usage_run.0, "use tokens").await;
    let result = reload_outcome(&usage_run.0, AGENT_ID, None, none).await;
    let usage_case = reload_finish(
        &usage_run,
        vec![("usageEvents", to_array(usage_events)), ("result", result)],
    )
    .await;

    let slow = reload_manager("slowclose", "fake", &[], home, cwd, |fake, options| {
        fake.close_hangs = true;
        options.rescue_reload_session_close_ms = Some(80);
    })
    .await;
    let slow_first = reload_outcome(&slow.0, AGENT_ID, None, none).await;
    let slow_second = reload_outcome(&slow.0, AGENT_ID, None, none).await;
    let slow_case = reload_finish(
        &slow,
        vec![("results", to_array(vec![slow_first, slow_second]))],
    )
    .await;

    let failing = reload_manager("resumefails", "fake", &[], home, cwd, |fake, _| {
        fake.resume_fails = true;
    })
    .await;
    let result = reload_outcome(&failing.0, AGENT_ID, None, none).await;
    let failing_case = reload_finish(&failing, vec![("result", result)]).await;

    let bare = reload_manager("nopersistence", "fake", &[], home, cwd, |fake, _| {
        fake.no_persistence = true;
    })
    .await;
    let result = reload_outcome(
        &bare.0,
        AGENT_ID,
        Some(object(vec![("title", text("Fresh"))])),
        none,
    )
    .await;
    let bare_case = reload_finish(&bare, vec![("result", result)]).await;

    // An agent restored with recorded timestamps keeps them through a reload.
    let restored_calls = Calls::default();
    let restored_registry = AgentStorage::new(home.join("reload-restored"));
    let restored_manager = manager_with(
        &restored_calls,
        &restored_registry,
        vec![(spec("fake"), enabled())],
    );
    let restored_feed = record_feed(&restored_manager);
    restored_manager
        .resume_agent_from_persistence(
            object(vec![
                ("provider", text("fake")),
                ("sessionId", text("sess-r")),
                ("nativeHandle", text("thread-r")),
                (
                    "metadata",
                    object(vec![
                        ("cwd", text(cwd)),
                        ("model", text("model-a")),
                        ("title", text("Stored")),
                    ]),
                ),
            ]),
            Some(object(vec![("modeId", text("auto"))])),
            Some(AGENT_ID.to_owned()),
            ResumeAgentOptions {
                created_at_millis: Some(1_700_000_000_000),
                updated_at_millis: Some(1_700_000_005_000),
                last_user_message_at_millis: Some(1_700_000_004_000),
                labels: Some(object(vec![("surface", text("workspace"))])),
                workspace_id: Some("wks_9".to_owned()),
                owner: None,
                attention: Some(AgentAttention::Required {
                    reason: "finished".to_owned(),
                    timestamp_millis: 1_700_000_006_000,
                }),
            },
            Some(AgentResumeSessionOptions {
                purpose: Some(AgentResumePurpose::Interactive),
            }),
        )
        .await
        .expect("resume");
    let restored = (
        restored_manager,
        restored_registry,
        restored_calls,
        restored_feed,
        Arc::default(),
    );
    let result = reload_outcome(
        &restored.0,
        AGENT_ID,
        Some(object(vec![("title", text("Again"))])),
        none,
    )
    .await;
    let restored_case = reload_finish(&restored, vec![("result", result)]).await;

    let unknown = outcome(
        bare.0
            .reload_agent_session("00000000-0000-4000-8000-0000000000f3", None, none)
            .await
            .map(|_| JsValue::Null),
    );
    // Node shares one logger across the cases; their warnings, in case order.
    let warns = to_array(
        [
            &idle,
            &rehydrate,
            &running,
            &refused,
            &no_mcp,
            &no_client,
            &failed_run,
            &usage_run,
            &slow,
            &failing,
            &bare,
        ]
        .iter()
        .flat_map(|case| case.4.lock().expect("warns").clone())
        .collect(),
    );
    object(vec![
        ("a", idle_case),
        ("b", rehydrate_case),
        ("c", running_case),
        ("d", refused_case),
        ("e", no_mcp_case),
        ("f", slow_case),
        ("g", failing_case),
        ("h", bare_case),
        ("i", restored_case),
        ("j", no_client_case),
        ("k", failed_case),
        ("l", usage_case),
        ("unknown", unknown),
        ("warns", warns),
    ])
}

/// A manager over one fake provider for the failure-log cases, with its
/// warnings and errors recorded in `logs`.
fn failure_manager(
    name: &str,
    home: &Path,
    logs: &Arc<Mutex<Vec<JsValue>>>,
    configure: impl FnOnce(&mut Spec),
    turns: &[&str],
) -> (AgentManager, AgentStorage, Calls, Feed) {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join(format!("failure-{name}")));
    let mut fake = spec("fake");
    scripted(&fake, turns);
    configure(&mut fake);
    let warn_sink = Arc::clone(logs);
    let error_sink = Arc::clone(logs);
    let manager = AgentManager::new(AgentManagerOptions {
        clients: vec![("fake".to_owned(), rewind_client(fake, &calls))],
        provider_definitions: vec![("fake".to_owned(), enabled())],
        registry: Some(registry.clone()),
        log_warn: Some(Arc::new(move |bindings, message| {
            warn_sink.lock().expect("logs").push(JsValue::Array(vec![
                text("warn"),
                bindings,
                text(message),
            ]));
        })),
        log_error: Some(Arc::new(move |bindings, message| {
            error_sink.lock().expect("logs").push(JsValue::Array(vec![
                text("error"),
                bindings,
                text(message),
            ]));
        })),
        ..AgentManagerOptions::default()
    });
    let feed = record_feed(&manager);
    (manager, registry, calls, feed)
}

async fn failure_create(manager: &AgentManager, cwd: &str) {
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
}

async fn failure_finish(
    parts: &(AgentManager, AgentStorage, Calls, Feed),
    mut extra: Vec<(&'static str, JsValue)>,
) -> JsValue {
    let (manager, registry, calls, feed) = parts;
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    extra.push((
        "calls",
        JsValue::Array(calls.lock().expect("calls").clone()),
    ));
    extra.push(("feed", JsValue::Array(feed.lock().expect("feed").clone())));
    object(extra)
}

async fn failure_logs_scenario(cwd: &str, home: &Path) -> JsValue {
    let logs: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let fixture = json(SCENARIO_TURNS);

    let closing = failure_manager(
        "close",
        home,
        &logs,
        |fake| {
            fake.import = fixture.get("badImport").cloned();
            fake.close_fails = true;
        },
        &[],
    );
    let result = outcome(
        closing
            .0
            .import_provider_session(ImportProviderSessionRequest {
                provider: "fake".to_owned(),
                provider_handle_id: "h1".to_owned(),
                cwd: cwd.to_owned(),
                workspace_id: "wks_9".to_owned(),
                labels: None,
            })
            .await
            .map(|agent| to_agent_payload(&agent.payload_view(), None).expect("payload")),
    );
    let close_case = failure_finish(&closing, vec![("result", result)]).await;

    let events = failure_manager("event", home, &logs, |_| {}, &["badItem"]);
    failure_create(&events.0, cwd).await;
    let mut event_stream = Vec::new();
    collect_stream(
        events
            .0
            .stream_agent(
                AGENT_ID,
                AgentPromptInput::Text("bad item".to_owned()),
                None,
            )
            .expect("stream"),
        &mut event_stream,
    )
    .await;
    let event_case =
        failure_finish(&events, vec![("eventStream", JsValue::Array(event_stream))]).await;

    let history = failure_manager(
        "history",
        home,
        &logs,
        |fake| {
            fake.history = fixture.get("history").cloned();
            fake.history_fails = true;
        },
        &[],
    );
    failure_create(&history.0, cwd).await;
    history
        .0
        .reload_agent_session(
            AGENT_ID,
            None,
            ReloadAgentOptions {
                rehydrate_from_disk: true,
            },
        )
        .await
        .expect("reload");
    let result = outcome(
        history
            .0
            .hydrate_timeline_from_provider(AGENT_ID, HydrateTimelineOptions::default())
            .await
            .map(|()| JsValue::Null),
    );
    let history_case = failure_finish(&history, vec![("result", result)]).await;
    let logs = JsValue::Array(logs.lock().expect("logs").clone());
    object(vec![
        ("closeCase", close_case),
        ("eventCase", event_case),
        ("historyCase", history_case),
        ("logs", logs),
    ])
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn settings_scenario(cwd: &str, home: &Path) -> JsValue {
    let table = json(SCENARIO_TURNS);
    let mut out = JsObject::new();
    for item in table
        .get("settingsCases")
        .and_then(JsValue::as_array)
        .expect("cases")
    {
        let name = js_string(item.get("name"));
        let knobs = item.get("spec").cloned().unwrap_or(JsValue::Undefined);
        let calls = Calls::default();
        let registry = AgentStorage::new(home.join(format!("settings-{name}")));
        let mut fake = spec("fake");
        fake.settable = knobs.get("settable").and_then(JsValue::as_bool) == Some(true);
        fake.mode_notice = knobs.get("modeNotice").cloned();
        fake.thinking_notice = knobs.get("thinkingNotice").cloned();
        fake.current_mode_null =
            knobs.get("currentModeNull").and_then(JsValue::as_bool) == Some(true);
        fake.runtime_info_extra = knobs.get("runtimeInfoExtra").cloned();
        let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
        let feed = record_feed(&manager);
        manager
            .create_agent(
                object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
                Some(AGENT_ID.to_owned()),
                CreateAgentOptions::default(),
            )
            .await
            .expect("create");
        let mut steps = Vec::new();
        for op in item.get("ops").and_then(JsValue::as_array).expect("ops") {
            let op = op.as_array().expect("op");
            let kind = js_string(op.first());
            let arg = op.get(1);
            let result = match kind.as_str() {
                "mode" => outcome(
                    manager
                        .set_agent_mode(AGENT_ID, &js_string(arg))
                        .await
                        .map(|notice| notice.unwrap_or(JsValue::Null)),
                ),
                "model" => outcome(
                    manager
                        .set_agent_model(AGENT_ID, arg.and_then(JsValue::as_str))
                        .await
                        .map(|()| JsValue::Null),
                ),
                "thinking" => outcome(
                    manager
                        .set_agent_thinking_option(AGENT_ID, arg.and_then(JsValue::as_str))
                        .await
                        .map(|notice| notice.unwrap_or(JsValue::Null)),
                ),
                _ => outcome(
                    manager
                        .set_agent_feature(
                            AGENT_ID,
                            &js_string(arg),
                            op.get(2).cloned().unwrap_or(JsValue::Undefined),
                        )
                        .await
                        .map(|()| JsValue::Null),
                ),
            };
            steps.push(object(vec![
                ("result", result),
                (
                    "agent",
                    to_agent_payload(
                        &manager.get_agent(AGENT_ID).expect("agent").payload_view(),
                        None,
                    )
                    .expect("payload"),
                ),
            ]));
        }
        let unknown = outcome(
            manager
                .set_agent_mode("00000000-0000-4000-8000-0000000000f3", "x")
                .await
                .map(|notice| notice.unwrap_or(JsValue::Null)),
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        manager.flush().await;
        registry.flush().await;
        let stored = registry.get(AGENT_ID).await.unwrap_or(JsValue::Null);
        out.insert(
            name.as_str(),
            object(vec![
                ("steps", JsValue::Array(steps)),
                ("unknown", unknown),
                (
                    "calls",
                    JsValue::Array(calls.lock().expect("calls").clone()),
                ),
                ("feed", JsValue::Array(feed.lock().expect("feed").clone())),
                ("stored", stored),
            ]),
        );
    }
    JsValue::Object(out)
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn metadata_scenario(cwd: &str, home: &Path) -> JsValue {
    const UNKNOWN: &str = "00000000-0000-4000-8000-0000000000f3";
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("metadata"));
    let manager = manager_with(&calls, &registry, vec![(spec("fake"), enabled())]);
    let feed = record_feed(&manager);
    for (id, lane) in [(AGENT_ID, "one"), (OTHER_ID, "other")] {
        manager
            .create_agent(
                object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
                Some(id.to_owned()),
                CreateAgentOptions {
                    labels: Some(object(vec![("lane", text(lane))])),
                    workspace_id: Some("wks_1".to_owned()),
                    ..CreateAgentOptions::default()
                },
            )
            .await
            .expect("create");
    }
    manager.close_agent(OTHER_ID).await.expect("close");
    manager.flush().await;
    registry.flush().await;
    let live = || {
        to_agent_payload(
            &manager.get_agent(AGENT_ID).expect("agent").payload_view(),
            None,
        )
        .expect("payload")
    };
    let updates = |title: Option<&str>, labels: Option<JsValue>| AgentMetadataUpdates {
        title: title.map(str::to_owned),
        labels,
    };
    let patch = |entries: Vec<(&str, JsValue)>| object(entries);
    let mut results = Vec::new();
    results.push(outcome(
        manager
            .set_labels(
                AGENT_ID,
                &patch(vec![("lane", text("two")), ("extra", text("1"))]),
            )
            .await
            .map(|()| live()),
    ));
    results.push(outcome(
        manager
            .set_labels(AGENT_ID, &patch(vec![("extra", JsValue::Null)]))
            .await
            .map(|()| live()),
    ));
    results.push(outcome(
        manager
            .set_labels(UNKNOWN, &patch(vec![("a", text("b"))]))
            .await
            .map(|()| live()),
    ));
    results.push(outcome(
        manager
            .update_agent_metadata(
                AGENT_ID,
                updates(Some("New title"), Some(patch(vec![("x", text("y"))]))),
            )
            .await
            .map(|()| live()),
    ));
    results.push(outcome(
        manager
            .update_agent_metadata(
                AGENT_ID,
                updates(Some(""), Some(patch(vec![("x", JsValue::Null)]))),
            )
            .await
            .map(|()| live()),
    ));
    results.push(outcome(
        manager
            .update_agent_metadata(
                OTHER_ID,
                updates(
                    Some("Stored title"),
                    Some(patch(vec![("s", text("1")), ("lane", JsValue::Null)])),
                ),
            )
            .await
            .map(|()| live()),
    ));
    results.push(outcome(
        manager
            .update_agent_metadata(OTHER_ID, updates(None, None))
            .await
            .map(|()| live()),
    ));
    results.push(outcome(
        manager
            .update_agent_metadata(UNKNOWN, updates(Some("x"), None))
            .await
            .map(|()| live()),
    ));
    for id in [AGENT_ID, AGENT_ID, OTHER_ID, OTHER_ID, UNKNOWN] {
        results.push(outcome(
            manager.mark_agent_unread(id).await.map(|()| live()),
        ));
    }
    let stored_at = "2026-07-01T00:00:01.000Z";
    let mut stored_ids = JsObject::new();
    for (suffix, extra) in [
        ("f4", vec![("lastStatus", text("running"))]),
        ("f5", vec![("archivedAt", text("2026-07-01T00:00:00.000Z"))]),
        ("f6", vec![("internal", JsValue::Bool(true))]),
        ("f7", vec![("requiresAttention", JsValue::Bool(true))]),
        ("f8", vec![("updatedAt", text("2099-01-01T00:00:00.000Z"))]),
    ] {
        let id = format!("00000000-0000-4000-8000-0000000000{suffix}");
        let mut record = JsObject::new();
        record.insert("id", text(&id));
        record.insert("provider", text("fake"));
        record.insert("cwd", text(cwd));
        record.insert("createdAt", text("2026-07-01T00:00:00.000Z"));
        record.insert("updatedAt", text(stored_at));
        record.insert("lastStatus", text("idle"));
        for (key, value) in extra {
            record.insert(key, value);
        }
        registry
            .upsert(JsValue::Object(record))
            .await
            .expect("upsert");
        results.push(outcome(
            manager.mark_agent_unread(&id).await.map(|()| live()),
        ));
        stored_ids.insert(suffix, text(&id));
    }
    let pending_calls = Calls::default();
    let pending_registry = AgentStorage::new(home.join("metadata-pending"));
    let mut pending_spec = spec("fake");
    pending_spec.response = json(SCENARIO_TURNS).get("spontaneousPermission").cloned();
    let pending_manager = manager_with(
        &pending_calls,
        &pending_registry,
        vec![(pending_spec, enabled())],
    );
    let pending_feed = record_feed(&pending_manager);
    pending_manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    pending_manager
        .respond_to_permission(AGENT_ID, "none", object(vec![("behavior", text("allow"))]))
        .await
        .expect("respond");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let pending = outcome(pending_manager.mark_agent_unread(AGENT_ID).await.map(|()| {
        to_agent_payload(
            &pending_manager
                .get_agent(AGENT_ID)
                .expect("agent")
                .payload_view(),
            None,
        )
        .expect("payload")
    }));
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let bare = AgentManager::new(AgentManagerOptions {
        clients: vec![(
            "fake".to_owned(),
            rewind_client(spec("fake"), &Calls::default()),
        )],
        provider_definitions: vec![("fake".to_owned(), enabled())],
        ..AgentManagerOptions::default()
    });
    let no_storage = vec![
        outcome(
            bare.mark_agent_unread(UNKNOWN)
                .await
                .map(|()| JsValue::Null),
        ),
        outcome(
            bare.update_agent_metadata(UNKNOWN, updates(Some("x"), None))
                .await
                .map(|()| JsValue::Null),
        ),
    ];
    let stored_live = registry.get(AGENT_ID).await.unwrap_or(JsValue::Null);
    let stored_other = registry.get(OTHER_ID).await.unwrap_or(JsValue::Null);
    let mut stored_fixtures = JsObject::new();
    for (suffix, id) in stored_ids.iter() {
        stored_fixtures.insert(
            suffix,
            registry
                .get(id.as_str().expect("id"))
                .await
                .unwrap_or(JsValue::Null),
        );
    }
    let pending_feed = JsValue::Array(pending_feed.lock().expect("feed").clone());
    object(vec![
        ("results", JsValue::Array(results)),
        ("storedLive", stored_live),
        ("storedOther", stored_other),
        ("storedFixtures", JsValue::Object(stored_fixtures)),
        ("pending", pending),
        ("pendingFeed", pending_feed),
        ("feed", JsValue::Array(feed.lock().expect("feed").clone())),
        (
            "calls",
            JsValue::Array(calls.lock().expect("calls").clone()),
        ),
        ("noStorage", JsValue::Array(no_storage)),
    ])
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn steer_scenario(cwd: &str, home: &Path) -> JsValue {
    let table = json(SCENARIO_TURNS);
    let mut out = JsObject::new();
    for item in table
        .get("steerCases")
        .and_then(JsValue::as_array)
        .expect("cases")
    {
        let name = js_string(item.get("name"));
        let calls = Calls::default();
        let registry = AgentStorage::new(home.join(format!("steer-{name}")));
        let mut fake = spec("fake");
        let turns: Vec<String> = item
            .get("turns")
            .and_then(JsValue::as_array)
            .expect("turns")
            .iter()
            .map(|turn| js_string(Some(turn)))
            .collect();
        scripted(&fake, &turns.iter().map(String::as_str).collect::<Vec<_>>());
        fake.interrupt = table.get("interrupt").cloned();
        fake.steer = item
            .get("spec")
            .and_then(|knobs| knobs.get("steer"))
            .cloned();
        let hook_calls: Arc<Mutex<Vec<JsValue>>> = Arc::default();
        let race_events: Arc<Mutex<Vec<JsValue>>> = Arc::default();
        let race_stream: Arc<tokio::sync::Mutex<Option<TurnEventStream>>> = Arc::default();
        let cell: Arc<Mutex<Option<AgentManager>>> = Arc::default();
        let race = item.get("hookRace").and_then(JsValue::as_bool) == Some(true);
        let hook: spocky_session::agent_manager::SteerFallbackHook = {
            let (hook_calls, race_events, race_stream, cell) = (
                Arc::clone(&hook_calls),
                Arc::clone(&race_events),
                Arc::clone(&race_stream),
                Arc::clone(&cell),
            );
            Arc::new(move |agent_id, expected| {
                let (hook_calls, race_events, race_stream, cell) = (
                    Arc::clone(&hook_calls),
                    Arc::clone(&race_events),
                    Arc::clone(&race_stream),
                    Arc::clone(&cell),
                );
                Box::pin(async move {
                    hook_calls.lock().expect("hook").push(object(vec![
                        ("agentId", text(&agent_id)),
                        ("expectedTurnId", text(&expected)),
                    ]));
                    if race {
                        let manager = cell.lock().expect("cell").clone().expect("manager");
                        manager.cancel_agent_run(&agent_id).await.expect("cancel");
                        let mut stream = manager
                            .stream_agent(
                                &agent_id,
                                AgentPromptInput::Text("race".to_owned()),
                                None,
                            )
                            .expect("race stream");
                        let first = stream.next().await.expect("first").expect("event");
                        race_events.lock().expect("race").push(first);
                        *race_stream.lock().await = Some(stream);
                    }
                })
            })
        };
        let manager = AgentManager::new(AgentManagerOptions {
            clients: vec![("fake".to_owned(), rewind_client(fake, &calls))],
            provider_definitions: vec![("fake".to_owned(), enabled())],
            registry: Some(registry.clone()),
            before_steer_unavailable_fallback: Some(hook),
            ..AgentManagerOptions::default()
        });
        *cell.lock().expect("cell") = Some(manager.clone());
        let feed = record_feed(&manager);
        manager
            .create_agent(
                object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
                Some(AGENT_ID.to_owned()),
                CreateAgentOptions::default(),
            )
            .await
            .expect("create");
        let mut held = None;
        let mut held_events = Vec::new();
        if item.get("running").and_then(JsValue::as_bool) == Some(true) {
            let mut stream = manager
                .stream_agent(AGENT_ID, AgentPromptInput::Text("hold".to_owned()), None)
                .expect("held stream");
            held_events.push(stream.next().await.expect("first").expect("event"));
            held = Some(stream);
            // The held turn's reasoning item reaches the timeline after the
            // coalescing window.
            for tick in 0..=2000 {
                assert!(tick < 2000, "the held turn never reached the timeline");
                if !manager.get_timeline(AGENT_ID).expect("timeline").is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        let mut results = Vec::new();
        for op in item.get("ops").and_then(JsValue::as_array).expect("ops") {
            let op = op.as_array().expect("op");
            let kind = js_string(op.first());
            let prompt = AgentPromptInput::Text(js_string(op.get(1)));
            let options = op
                .get(2)
                .filter(|options| !matches!(options, JsValue::Null))
                .map(|options| AgentSteerOptions {
                    run: AgentRunOptions {
                        client_message_id: options
                            .get("clientMessageId")
                            .and_then(JsValue::as_str)
                            .map(str::to_owned),
                        ..AgentRunOptions::default()
                    },
                    clear_pending_permissions: options
                        .get("clearPendingPermissions")
                        .and_then(JsValue::as_bool),
                });
            let status = |status: &str| object(vec![("status", text(status))]);
            if kind == "steer" {
                results.push(outcome(
                    manager
                        .steer_agent_run(AGENT_ID, prompt, options)
                        .await
                        .map(|result| {
                            status(match result {
                                SteerResult::Accepted => "accepted",
                                SteerResult::Unavailable => "unavailable",
                            })
                        }),
                ));
                continue;
            }
            let dispatch = manager
                .steer_or_replace_active_turn(AGENT_ID, prompt, options)
                .await;
            results.push(match dispatch {
                Err(error) => outcome(Err(error)),
                Ok(SteerDispatch::Inactive) => outcome(Ok(status("inactive"))),
                Ok(SteerDispatch::Steered) => outcome(Ok(status("steered"))),
                Ok(SteerDispatch::Replaced(stream)) => {
                    let mut events = Vec::new();
                    collect_stream(*stream, &mut events).await;
                    outcome(Ok(object(vec![
                        ("status", text("replaced")),
                        ("events", JsValue::Array(events)),
                    ])))
                }
            });
        }
        if let Some(held) = held {
            if item.get("cancel").and_then(JsValue::as_bool) != Some(false) {
                let _ = manager.cancel_agent_run(AGENT_ID).await;
            }
            collect_stream(held, &mut held_events).await;
        }
        let raced = race_stream.lock().await.take();
        if let Some(stream) = raced {
            let mut events = race_events.lock().expect("race").clone();
            collect_stream(stream, &mut events).await;
            *race_events.lock().expect("race") = events;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        manager.flush().await;
        registry.flush().await;
        let agent = to_agent_payload(
            &manager.get_agent(AGENT_ID).expect("agent").payload_view(),
            None,
        )
        .expect("payload");
        let rows = JsValue::Array(manager.get_timeline_rows(AGENT_ID).expect("rows"));
        let race_events = JsValue::Array(race_events.lock().expect("race").clone());
        let hook_calls = JsValue::Array(hook_calls.lock().expect("hook").clone());
        out.insert(
            name.as_str(),
            object(vec![
                ("results", JsValue::Array(results)),
                ("heldEvents", JsValue::Array(held_events)),
                ("raceEvents", race_events),
                ("hookCalls", hook_calls),
                (
                    "calls",
                    JsValue::Array(calls.lock().expect("calls").clone()),
                ),
                ("feed", JsValue::Array(feed.lock().expect("feed").clone())),
                ("agent", agent),
                ("rows", rows),
            ]),
        );
    }
    JsValue::Object(out)
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn timeline_items_scenario(cwd: &str, home: &Path) -> JsValue {
    const UNKNOWN: &str = "00000000-0000-4000-8000-0000000000f3";
    const OTHER: &str = "00000000-0000-4000-8000-0000000000f4";
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("timeline-items"));
    let manager = manager_with(&calls, &registry, vec![(spec("fake"), enabled())]);
    let feed = record_feed(&manager);
    // An agent restored with recorded timestamps shows what each call touches.
    for id in [AGENT_ID, OTHER] {
        manager
            .resume_agent_from_persistence(
                object(vec![
                    ("provider", text("fake")),
                    ("sessionId", text(&format!("sess-{id}"))),
                    ("nativeHandle", text(&format!("thread-{id}"))),
                    (
                        "metadata",
                        object(vec![("cwd", text(cwd)), ("model", text("model-a"))]),
                    ),
                ]),
                None,
                Some(id.to_owned()),
                ResumeAgentOptions {
                    created_at_millis: Some(1_700_000_000_000),
                    updated_at_millis: Some(1_700_000_005_000),
                    last_user_message_at_millis: Some(1_700_000_004_000),
                    ..ResumeAgentOptions::default()
                },
                Some(AgentResumeSessionOptions {
                    purpose: Some(AgentResumePurpose::Interactive),
                }),
            )
            .await
            .expect("resume");
    }
    let message = |body: &str| {
        object(vec![
            ("type", text("assistant_message")),
            ("text", text(body)),
        ])
    };
    let appended = |result: Result<AppendedTimelineItem, AgentError>| {
        outcome(result.map(|item| {
            object(vec![
                ("seq", number(item.seq)),
                ("epoch", text(&item.epoch)),
            ])
        }))
    };
    let running_call = object(vec![
        ("type", text("tool_call")),
        ("callId", text("x")),
        ("name", text("shell")),
        ("status", text("running")),
        ("error", JsValue::Null),
    ]);
    let big_call = object(vec![
        ("type", text("tool_call")),
        ("callId", text("big")),
        ("name", text("shell")),
        ("status", text("completed")),
        ("error", JsValue::Null),
        (
            "detail",
            object(vec![
                ("type", text("shell")),
                ("command", text("ls")),
                ("output", text(&"x".repeat(70_000))),
            ]),
        ),
    ]);
    let mut results = Vec::new();
    macro_rules! step {
        ($id:expr, $result:expr) => {{
            let result = $result;
            tokio::time::sleep(Duration::from_millis(50)).await;
            manager.flush().await;
            registry.flush().await;
            let stored = registry.get($id).await.unwrap_or(JsValue::Null);
            results.push(object(vec![
                ("result", result),
                (
                    "agent",
                    to_agent_payload(&manager.get_agent($id).expect("agent").payload_view(), None)
                        .expect("payload"),
                ),
                ("stored", stored),
            ]));
        }};
    }
    step!(
        AGENT_ID,
        outcome(
            manager
                .emit_live_timeline_item(AGENT_ID, message("live"))
                .map(|()| JsValue::Null)
        )
    );
    step!(
        AGENT_ID,
        outcome(
            manager
                .emit_live_timeline_item(UNKNOWN, message("x"))
                .map(|()| JsValue::Null)
        )
    );
    step!(
        OTHER,
        appended(
            manager
                .append_timeline_item(OTHER, message("appended"))
                .await
        )
    );
    step!(
        OTHER,
        appended(manager.append_timeline_item(OTHER, running_call).await)
    );
    step!(
        OTHER,
        appended(manager.append_timeline_item(OTHER, big_call).await)
    );
    step!(
        OTHER,
        appended(manager.append_timeline_item(UNKNOWN, message("x")).await)
    );
    object(vec![
        ("results", JsValue::Array(results)),
        (
            "rows",
            JsValue::Array(manager.get_timeline_rows(AGENT_ID).expect("rows")),
        ),
        (
            "otherRows",
            JsValue::Array(manager.get_timeline_rows(OTHER).expect("rows")),
        ),
        ("feed", JsValue::Array(feed.lock().expect("feed").clone())),
    ])
}

async fn availability_scenario(home: &Path) -> JsValue {
    let calls = Calls::default();
    let warns: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let warn_sink = Arc::clone(&warns);
    let client = |provider: &str, available: Availability, delay_ms: Option<u64>| {
        let mut fake = spec(provider);
        fake.available = available;
        fake.available_delay = delay_ms.map(Duration::from_millis);
        (
            provider.to_owned(),
            Arc::new(FakeClient {
                spec: fake,
                calls: Arc::clone(&calls),
            }) as Arc<dyn AgentClient>,
        )
    };
    let manager = AgentManager::new(AgentManagerOptions {
        clients: vec![
            client("slowbad", Err("slow failure".to_owned()), Some(120)),
            client("fake", Ok(true), None),
            client("gone", Ok(false), None),
            client("fastbad", Err("fast failure".to_owned()), None),
        ],
        provider_definitions: ["slowbad", "fake", "gone", "fastbad"]
            .into_iter()
            .map(|provider| (provider.to_owned(), enabled()))
            .collect(),
        registry: Some(AgentStorage::new(home.join("availability"))),
        log_warn: Some(Arc::new(move |bindings, message| {
            warn_sink
                .lock()
                .expect("warns")
                .push(JsValue::Array(vec![bindings, text(message)]));
        })),
        ..AgentManagerOptions::default()
    });
    let entry = |(provider, available, error): (String, bool, Option<String>)| {
        object(vec![
            ("provider", text(&provider)),
            ("available", JsValue::Bool(available)),
            ("error", error.map_or(JsValue::Null, |error| text(&error))),
        ])
    };
    let mut results = vec![outcome(Ok(JsValue::Array(
        manager
            .list_provider_availability()
            .await
            .into_iter()
            .map(entry)
            .collect(),
    )))];
    let after_list = std::mem::take(&mut *warns.lock().expect("warns"));
    for provider in ["fake", "gone", "fastbad", "nope"] {
        results.push(outcome(Ok::<_, AgentError>(entry(
            manager.get_provider_availability(provider).await,
        ))));
    }
    let warns = warns.lock().expect("warns").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("afterList", JsValue::Array(after_list)),
        ("warns", JsValue::Array(warns)),
    ])
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn importable_scenario(home: &Path) -> JsValue {
    const PROVIDERS: [&str; 9] = [
        "alpha", "slow", "beta", "nolist", "slowfail", "failing", "bulk", "nocap", "off",
    ];
    let scripted = json(SCENARIO_TURNS)
        .get("importable")
        .cloned()
        .expect("importable");
    let bulk = object(vec![(
        "sessions",
        JsValue::Array(
            (0..22_i64)
                .map(|index| {
                    object(vec![
                        ("providerHandleId", text(&format!("bulk-{index}"))),
                        ("cwd", text("/bulk")),
                        ("title", text(&format!("Bulk {index}"))),
                        ("firstPromptPreview", JsValue::Null),
                        ("lastPromptPreview", JsValue::Null),
                        ("lastActivityAt", number(1_600_000_000_000 + index * 1000)),
                    ])
                })
                .collect(),
        ),
    )]);
    let calls = Calls::default();
    let warns: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let warn_sink = Arc::clone(&warns);
    let mut listing = json(CAPABILITIES);
    if let JsValue::Object(object) = &mut listing {
        object.insert("supportsSessionListing", JsValue::Bool(true));
    }
    let make = |provider: &str, importable: Option<JsValue>, capabilities: Option<JsValue>| {
        let mut fake = spec(provider);
        fake.capabilities = capabilities.unwrap_or_else(|| listing.clone());
        fake.importable = importable;
        (
            provider.to_owned(),
            Arc::new(FakeClient {
                spec: fake,
                calls: Arc::clone(&calls),
            }) as Arc<dyn AgentClient>,
        )
    };
    let scripted_for = |provider: &str| scripted.get(provider).cloned();
    let clients = vec![
        make("alpha", scripted_for("alpha"), None),
        make("slow", scripted_for("slow"), None),
        make("beta", scripted_for("beta"), None),
        make("nolist", None, None),
        make("slowfail", scripted_for("slowfail"), None),
        make("failing", scripted_for("failing"), None),
        make("bulk", Some(bulk), None),
        make("nocap", scripted_for("nocap"), Some(json(CAPABILITIES))),
        make("off", scripted_for("off"), None),
    ];
    let manager = AgentManager::new(AgentManagerOptions {
        clients,
        provider_definitions: PROVIDERS
            .into_iter()
            .map(|provider| {
                (
                    provider.to_owned(),
                    ProviderDefinition {
                        enabled: provider != "off",
                        ..ProviderDefinition::default()
                    },
                )
            })
            .collect(),
        registry: Some(AgentStorage::new(home.join("importable"))),
        log_warn: Some(Arc::new(move |bindings, message| {
            warn_sink
                .lock()
                .expect("warns")
                .push(JsValue::Array(vec![bindings, text(message)]));
        })),
        ..AgentManagerOptions::default()
    });
    let query = |limit: Option<f64>,
                 query: Option<&str>,
                 scan_limit: Option<f64>,
                 cwd: Option<&str>,
                 filter: Option<&[&str]>| {
        Some(ImportablePersistedAgentQueryOptions {
            list: ListImportableSessionsOptions {
                limit,
                query: query.map(str::to_owned),
                scan_limit,
                cwd: cwd.map(str::to_owned),
            },
            provider_filter: filter
                .map(|filter| filter.iter().map(|id| (*id).to_owned()).collect()),
        })
    };
    let cases = vec![
        ("all", None),
        ("limit2", query(Some(2.0), None, None, None, None)),
        ("limitNegative", query(Some(-1.0), None, None, None, None)),
        ("limitFraction", query(Some(1.5), None, None, None, None)),
        ("limitZero", query(Some(0.0), None, None, None, None)),
        ("query", query(None, Some("  LOGIN  "), None, None, None)),
        ("queryCwd", query(None, Some("beta"), None, None, None)),
        (
            "queryBackslashCwd",
            query(None, Some("C:"), None, None, None),
        ),
        ("queryNone", query(None, Some("zzz"), None, None, None)),
        ("queryBlank", query(None, Some("   "), None, None, None)),
        (
            "filter",
            query(None, None, None, None, Some(&["alpha", "off", "nocap"])),
        ),
        (
            "passthrough",
            query(Some(3.0), None, Some(7.0), Some("/work"), Some(&["beta"])),
        ),
    ];
    let mut results = Vec::new();
    for (name, options) in cases {
        let listed = manager.list_importable_sessions(options).await;
        let sessions = listed.sessions.into_iter().map(|managed| {
            let session = managed.session;
            object(vec![
                ("providerHandleId", text(&session.provider_handle_id)),
                ("cwd", text(&session.cwd)),
                (
                    "title",
                    session.title.as_deref().map_or(JsValue::Null, text),
                ),
                (
                    "firstPromptPreview",
                    session
                        .first_prompt_preview
                        .as_deref()
                        .map_or(JsValue::Null, text),
                ),
                (
                    "lastPromptPreview",
                    session
                        .last_prompt_preview
                        .as_deref()
                        .map_or(JsValue::Null, text),
                ),
                (
                    "lastActivityAt",
                    JsValue::Number(session.last_activity_at_millis),
                ),
                ("provider", text(&managed.provider)),
            ])
        });
        let provider_errors = listed.provider_errors.into_iter().map(|error| {
            object(vec![
                ("provider", text(&error.provider)),
                ("message", text(&error.message)),
            ])
        });
        let result = outcome(Ok::<_, AgentError>(object(vec![
            ("sessions", JsValue::Array(sessions.collect())),
            ("providerErrors", JsValue::Array(provider_errors.collect())),
        ])));
        let taken = std::mem::take(&mut *calls.lock().expect("calls"));
        let per_provider = PROVIDERS.map(|provider| {
            (
                provider,
                JsValue::Array(
                    taken
                        .iter()
                        .filter(|call| {
                            call.as_array().and_then(|call| call.get(1)) == Some(&text(provider))
                        })
                        .cloned()
                        .collect(),
                ),
            )
        });
        results.push(object(vec![
            ("name", text(name)),
            ("result", result),
            (
                "warns",
                JsValue::Array(std::mem::take(&mut *warns.lock().expect("warns"))),
            ),
            ("calls", object(per_provider.into_iter().collect())),
        ]));
    }
    JsValue::Array(results)
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn draft_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let warns: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let warn_sink = Arc::clone(&warns);
    let commands = json(r#"[{"name":"review","description":"Review code","argumentHint":""}]"#);
    let features = json(r#"[{"type":"toggle","id":"fast","label":"Fast","value":false}]"#);
    let session_commands =
        json(r#"[{"name":"session-review","description":"From a session","argumentHint":""}]"#);
    let session_features = json(r#"[{"type":"toggle","id":"plan","label":"Plan","value":true}]"#);
    let client = |provider: &str, configure: &dyn Fn(&mut Spec)| {
        let mut fake = spec(provider);
        configure(&mut fake);
        (
            provider.to_owned(),
            Arc::new(FakeClient {
                spec: fake,
                calls: Arc::clone(&calls),
            }) as Arc<dyn AgentClient>,
        )
    };
    let clients = vec![
        client("viaClient", &|fake| {
            fake.client_commands = Some(commands.clone());
            fake.client_features = Some(features.clone());
        }),
        client("viaSession", &|fake| {
            fake.session_commands = Some(session_commands.clone());
            fake.session_features = Some(session_features.clone());
        }),
        client("bare", &|_| {}),
        client("nullFeatures", &|fake| {
            fake.session_features = Some(JsValue::Null);
        }),
        client("closeFails", &|fake| {
            fake.session_commands = Some(session_commands.clone());
            fake.session_features = Some(session_features.clone());
            fake.close_fails = true;
        }),
        client("featuresOnly", &|fake| {
            fake.client_features = Some(features.clone());
        }),
        client("gone", &|fake| {
            fake.available = Ok(false);
            fake.client_features = Some(features.clone());
        }),
        client("broken", &|fake| {
            fake.available = Err("missing binary".to_owned());
            fake.client_commands = Some(commands.clone());
        }),
    ];
    let provider_definitions = clients
        .iter()
        .map(|(provider, _)| (provider.clone(), enabled()))
        .collect();
    let manager = AgentManager::new(AgentManagerOptions {
        clients,
        provider_definitions,
        registry: Some(AgentStorage::new(home.join("draft"))),
        log_warn: Some(Arc::new(move |bindings, message| {
            warn_sink
                .lock()
                .expect("warns")
                .push(JsValue::Array(vec![bindings, text(message)]));
        })),
        ..AgentManagerOptions::default()
    });
    let config = |provider: &str, cwd: Option<&str>, model: Option<&str>, options: bool| {
        let mut entries = vec![("provider", text(provider))];
        if let Some(cwd) = cwd {
            entries.push(("cwd", text(cwd)));
        }
        if let Some(model) = model {
            entries.push(("model", text(model)));
        }
        if options {
            entries.push(("providerOptions", object(vec![])));
        }
        object(entries)
    };
    let cases = [
        (
            "viaClient",
            config("viaClient", Some(cwd), Some("m1"), false),
        ),
        (
            "viaClientTrimmed",
            config("viaClient", Some(cwd), Some("  m2  "), false),
        ),
        (
            "viaSession",
            config("viaSession", Some(cwd), Some("m1"), false),
        ),
        ("bare", config("bare", Some(cwd), Some("m1"), false)),
        (
            "nullFeatures",
            config("nullFeatures", Some(cwd), Some("m1"), false),
        ),
        (
            "closeFails",
            config("closeFails", Some(cwd), Some("m1"), false),
        ),
        ("noModel", config("viaSession", Some(cwd), None, false)),
        (
            "defaultModel",
            config("viaClient", Some(cwd), Some(" default "), false),
        ),
        (
            "blankModel",
            config("featuresOnly", Some(cwd), Some(""), false),
        ),
        (
            "noModelNoClientFeatures",
            config("bare", Some(cwd), Some("default"), false),
        ),
        ("gone", config("gone", Some(cwd), Some("m1"), false)),
        ("broken", config("broken", Some(cwd), Some("m1"), false)),
        ("unknown", config("nope", Some(cwd), Some("m1"), false)),
        (
            "missingCwd",
            config(
                "viaClient",
                Some("/nonexistent/spocky-draft"),
                Some("m1"),
                false,
            ),
        ),
        ("noCwd", config("viaSession", None, Some("m1"), false)),
        (
            "providerOptions",
            config("viaClient", Some(cwd), Some("m1"), true),
        ),
    ];
    let mut results = Vec::new();
    for (name, config) in cases {
        let commands_result = outcome(manager.list_draft_commands(&config).await);
        let features_result = outcome(manager.list_draft_features(&config).await);
        results.push(object(vec![
            ("name", text(name)),
            ("commands", commands_result),
            ("features", features_result),
            (
                "calls",
                JsValue::Array(std::mem::take(&mut *calls.lock().expect("calls"))),
            ),
            (
                "warns",
                JsValue::Array(std::mem::take(&mut *warns.lock().expect("warns"))),
            ),
        ]));
    }
    JsValue::Array(results)
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn registry_scenario(cwd: &str, home: &Path) -> JsValue {
    const GAMMA_ID: &str = "00000000-0000-4000-8000-0000000000f5";
    let calls = Calls::default();
    let warns: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let warn_sink = Arc::clone(&warns);
    let client = |provider: &str, configure: &dyn Fn(&mut Spec)| {
        let mut fake = spec(provider);
        configure(&mut fake);
        Arc::new(FakeClient {
            spec: fake,
            calls: Arc::clone(&calls),
        }) as Arc<dyn AgentClient>
    };
    let plain = |provider: &str| client(provider, &|_| {});
    let alpha = client("alpha", &|fake| scripted(fake, &["held"]));
    let beta = client("beta", &|fake| fake.close_fails = true);
    let manager = AgentManager::new(AgentManagerOptions {
        clients: vec![
            ("alpha".to_owned(), alpha),
            ("beta".to_owned(), beta),
            ("gamma".to_owned(), plain("gamma")),
        ],
        provider_definitions: ["alpha", "beta", "gamma"]
            .into_iter()
            .map(|provider| (provider.to_owned(), enabled()))
            .collect(),
        registry: Some(AgentStorage::new(home.join("provider-registry"))),
        mcp_auth_token: Some("secret-token".to_owned()),
        resolve_paseo_tool_policy: Some(Arc::new(|provider| {
            Some(object(vec![
                ("enabled", JsValue::Bool(true)),
                ("name", text(provider)),
            ]))
        })),
        log_warn: Some(Arc::new(move |bindings, message| {
            warn_sink
                .lock()
                .expect("warns")
                .push(JsValue::Array(vec![bindings, text(message)]));
        })),
        ..AgentManagerOptions::default()
    });
    let feed = record_feed(&manager);
    for (id, provider) in [(AGENT_ID, "alpha"), (OTHER_ID, "beta"), (GAMMA_ID, "gamma")] {
        manager
            .create_agent(
                object(vec![("provider", text(provider)), ("cwd", text(cwd))]),
                Some(id.to_owned()),
                CreateAgentOptions::default(),
            )
            .await
            .expect("create");
    }
    for (id, body) in [(AGENT_ID, "one"), (AGENT_ID, "two"), (GAMMA_ID, "three")] {
        manager
            .append_timeline_item(
                id,
                object(vec![
                    ("type", text("assistant_message")),
                    ("text", text(body)),
                ]),
            )
            .await
            .expect("append");
    }
    let mut held = manager
        .stream_agent(AGENT_ID, AgentPromptInput::Text("hold".to_owned()), None)
        .expect("stream");
    let held_first = held.next().await.expect("first").expect("event");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let view = || async {
        let metrics = manager.metrics_snapshot();
        let mut by_lifecycle = JsObject::new();
        for (name, count) in &metrics.by_lifecycle {
            by_lifecycle.insert(name, number(i64::try_from(*count).expect("count")));
        }
        let count = |value: usize| number(i64::try_from(value).expect("count"));
        let availability = manager
            .list_provider_availability()
            .await
            .into_iter()
            .map(|(provider, available, error)| {
                object(vec![
                    ("provider", text(&provider)),
                    ("available", JsValue::Bool(available)),
                    ("error", error.map_or(JsValue::Null, |error| text(&error))),
                ])
            })
            .collect();
        object(vec![
            (
                "ids",
                JsValue::Array(
                    manager
                        .registered_provider_ids()
                        .iter()
                        .map(|id| text(id))
                        .collect(),
                ),
            ),
            (
                "metrics",
                object(vec![
                    ("total", count(metrics.total)),
                    ("subscriptionCount", count(metrics.subscription_count)),
                    ("byLifecycle", JsValue::Object(by_lifecycle)),
                    (
                        "withActiveForegroundTurn",
                        count(metrics.with_active_foreground_turn),
                    ),
                    (
                        "timelineStats",
                        object(vec![
                            ("totalItems", count(metrics.timeline_total_items)),
                            (
                                "maxItemsPerAgent",
                                count(metrics.timeline_max_items_per_agent),
                            ),
                        ]),
                    ),
                ]),
            ),
            ("availability", JsValue::Array(availability)),
            (
                "policies",
                JsValue::Array(
                    [AGENT_ID, OTHER_ID, UNKNOWN_ID]
                        .map(|id| manager.paseo_tool_policy(id).unwrap_or(JsValue::Null))
                        .to_vec(),
                ),
            ),
            (
                "token",
                manager.mcp_auth_token().map_or(JsValue::Null, text),
            ),
        ])
    };
    let create = |provider: &'static str| {
        let manager = manager.clone();
        let cwd = cwd.to_owned();
        async move {
            outcome(
                manager
                    .create_agent(
                        object(vec![("provider", text(provider)), ("cwd", text(&cwd))]),
                        None,
                        CreateAgentOptions::default(),
                    )
                    .await
                    .map(|agent| text(&agent.provider)),
            )
        }
    };
    let mut steps = Vec::new();
    steps.push(object(vec![
        ("name", text("initial")),
        ("view", view().await),
    ]));
    manager.register_client("alpha", plain("alpha"));
    manager.register_client("delta", plain("delta"));
    steps.push(object(vec![
        ("name", text("registered")),
        ("view", view().await),
        ("zeta", create("zeta").await),
        ("delta", create("delta").await),
    ]));
    manager.update_provider_registry(ProviderRegistryUpdate {
        provider_definitions: vec![
            ("gamma".to_owned(), enabled()),
            ("alpha".to_owned(), enabled()),
            (
                "off".to_owned(),
                ProviderDefinition {
                    enabled: false,
                    ..ProviderDefinition::default()
                },
            ),
        ],
        clients: vec![
            ("gamma".to_owned(), plain("gamma")),
            ("alpha".to_owned(), plain("alpha")),
            ("off".to_owned(), plain("off")),
        ],
        retired_providers: vec!["beta".to_owned(), "gamma".to_owned(), "nobody".to_owned()],
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    let updated_view = view().await;
    let beta_result = create("beta").await;
    let off_result = create("off").await;
    let alpha_result = create("alpha").await;
    let agents = manager
        .list_agents()
        .into_iter()
        .map(|agent| {
            JsValue::Array(vec![
                text(&agent.id),
                text(&agent.provider),
                text(agent.lifecycle.as_str()),
            ])
        })
        .collect();
    steps.push(object(vec![
        ("name", text("updated")),
        ("view", updated_view),
        ("beta", beta_result),
        ("off", off_result),
        ("alpha", alpha_result),
        ("agents", JsValue::Array(agents)),
    ]));
    manager.update_provider_registry(ProviderRegistryUpdate::default());
    steps.push(object(vec![
        ("name", text("emptied")),
        ("view", view().await),
        ("alpha", create("alpha").await),
    ]));
    manager
        .cancel_agent_run(AGENT_ID)
        .await
        .expect("cancel the held run");
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    object(vec![
        ("steps", JsValue::Array(steps)),
        ("heldFirst", held_first),
        ("feed", JsValue::Array(feed.lock().expect("feed").clone())),
        (
            "warns",
            JsValue::Array(warns.lock().expect("warns").clone()),
        ),
        (
            "calls",
            JsValue::Array(calls.lock().expect("calls").clone()),
        ),
    ])
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn callbacks_scenario(cwd: &str, home: &Path) -> JsValue {
    const GAMMA_ID: &str = "00000000-0000-4000-8000-0000000000f5";
    const CHILD_ID: &str = "00000000-0000-4000-8000-0000000000f6";
    let calls = Calls::default();
    let log: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let warns: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let warn_sink = Arc::clone(&warns);
    let client = |provider: &str, turns: &[&str]| {
        let fake = spec(provider);
        scripted(&fake, turns);
        (
            provider.to_owned(),
            Arc::new(FakeClient {
                spec: fake,
                calls: Arc::clone(&calls),
            }) as Arc<dyn AgentClient>,
        )
    };
    let clients = vec![
        client("p1", &["coalesce"]),
        client("p2", &["failed"]),
        client("p3", &["permission"]),
        client("p4", &["coalesce"]),
    ];
    let provider_definitions = clients
        .iter()
        .map(|(provider, _)| (provider.clone(), enabled()))
        .collect();
    let attention = |label: &'static str| -> AttentionCallback {
        let log = Arc::clone(&log);
        Arc::new(move |notice| {
            log.lock().expect("log").push(JsValue::Array(vec![
                text(label),
                object(vec![
                    ("agentId", text(&notice.agent_id)),
                    ("provider", text(&notice.provider)),
                    ("reason", text(&notice.reason)),
                ]),
            ]));
        })
    };
    let archived = |label: &'static str, fail: bool| -> AgentArchivedCallback {
        let log = Arc::clone(&log);
        Arc::new(move |id| {
            log.lock()
                .expect("log")
                .push(JsValue::Array(vec![text(label), text(&id)]));
            Box::pin(async move {
                if fail {
                    Err(AgentError::new("callback broke"))
                } else {
                    Ok(())
                }
            })
        })
    };
    let manager = AgentManager::new(AgentManagerOptions {
        clients,
        provider_definitions,
        registry: Some(AgentStorage::new(home.join("callbacks"))),
        on_agent_attention: Some(attention("first attention")),
        log_warn: Some(Arc::new(move |bindings, message| {
            warn_sink
                .lock()
                .expect("warns")
                .push(JsValue::Array(vec![bindings, text(message)]));
        })),
        ..AgentManagerOptions::default()
    });
    manager.set_agent_archived_callback(archived("first archived", false));
    let feed = record_feed(&manager);
    for (provider, id, parent) in [
        ("p1", AGENT_ID, false),
        ("p2", OTHER_ID, false),
        ("p3", GAMMA_ID, false),
        ("p4", CHILD_ID, true),
    ] {
        manager
            .create_agent(
                object(vec![("provider", text(provider)), ("cwd", text(cwd))]),
                Some(id.to_owned()),
                CreateAgentOptions {
                    labels: parent.then(|| object(vec![("paseo.parent-agent-id", text(AGENT_ID))])),
                    ..CreateAgentOptions::default()
                },
            )
            .await
            .expect("create");
    }
    let run = |id: &'static str| {
        let manager = manager.clone();
        async move {
            outcome(
                manager
                    .run_agent(id, AgentPromptInput::Text("go".to_owned()), None)
                    .await
                    .map(|_| JsValue::Null),
            )
        }
    };
    let archive = |id: &'static str| {
        let manager = manager.clone();
        async move { outcome(manager.archive_agent(id).await.map(|_| JsValue::Null)) }
    };
    let mut results = Vec::new();
    results.push(run(AGENT_ID).await);
    tokio::time::sleep(Duration::from_millis(50)).await;
    manager.set_agent_attention_callback(attention("second attention"));
    results.push(run(OTHER_ID).await);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let asking = tokio::spawn({
        let manager = manager.clone();
        async move {
            manager
                .run_agent(GAMMA_ID, AgentPromptInput::Text("ask".to_owned()), None)
                .await
        }
    });
    wait_for_turn_started(&feed, "turn-6").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    results.push(run(CHILD_ID).await);
    tokio::time::sleep(Duration::from_millis(50)).await;
    results.push(archive(AGENT_ID).await);
    manager.set_agent_archived_callback(archived("second archived", true));
    results.push(archive(OTHER_ID).await);
    tokio::time::sleep(Duration::from_millis(100)).await;
    // A callback sent through the dispatcher must not leave `flush` waiting.
    tokio::time::timeout(Duration::from_secs(10), manager.flush())
        .await
        .expect("flush settles once the callbacks have run");
    asking.abort();
    object(vec![
        ("results", JsValue::Array(results)),
        ("log", JsValue::Array(log.lock().expect("log").clone())),
        (
            "warns",
            JsValue::Array(warns.lock().expect("warns").clone()),
        ),
        ("feed", JsValue::Array(feed.lock().expect("feed").clone())),
    ])
}

/// The record directory, process id, clock and uuid of a temporary file's
/// path, the parts that differ between the two runs.
fn mask_temp_path(path: &str) -> String {
    let relative = path
        .rsplit_once("/persist-failure/")
        .map_or(path, |(_, rest)| rest);
    let Some((directory, name)) = relative.rsplit_once('/') else {
        return relative.to_owned();
    };
    let mut parts: Vec<&str> = name.split('.').collect();
    if parts.len() == 7 && parts[6] == "tmp" && parts[5].len() == 36 {
        parts[3] = "<pid>";
        parts[4] = "<ms>";
        parts[5] = "<uuid>";
    }
    format!("{directory}/{}", parts.join("."))
}

#[test]
fn temp_path_mask_keeps_the_directory_and_record_name() {
    let id = "00000000-0000-4000-8000-0000000000a1";
    let uuid = "3f2b8c1e-9a4d-4e6f-8b7a-1c2d3e4f5a6b";
    assert_eq!(
        mask_temp_path(&format!(
            "/tmp/x-1/persist-failure/proj/.{id}.json.4242.1791005720937.{uuid}.tmp"
        )),
        format!("proj/.{id}.json.<pid>.<ms>.<uuid>.tmp")
    );
    assert_eq!(mask_temp_path("/a/b/record.json"), "/a/b/record.json");
}

async fn persist_failure_scenario(cwd: &str, home: &Path) -> JsValue {
    use std::os::unix::fs::PermissionsExt;
    let calls = Calls::default();
    let errors: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let error_sink = Arc::clone(&errors);
    let base = home.join("persist-failure");
    let fake = spec("fake");
    scripted(&fake, &["coalesce"]);
    let manager = AgentManager::new(AgentManagerOptions {
        clients: vec![(
            "fake".to_owned(),
            Arc::new(FakeClient {
                spec: fake,
                calls: Arc::clone(&calls),
            }) as Arc<dyn AgentClient>,
        )],
        provider_definitions: vec![("fake".to_owned(), enabled())],
        registry: Some(AgentStorage::new(&base)),
        log_error: Some(Arc::new(move |bindings, message| {
            error_sink
                .lock()
                .expect("errors")
                .push(JsValue::Array(vec![bindings, text(message)]));
        })),
        ..AgentManagerOptions::default()
    });
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    tokio::time::sleep(Duration::from_millis(50)).await;
    manager.flush().await;
    // A read-only record directory makes every later write fail.
    let mut dirs = vec![base.clone()];
    for entry in std::fs::read_dir(&base).expect("records") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    let set_mode = |mode: u32| {
        for dir in &dirs {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).expect("mode");
        }
    };
    set_mode(0o500);
    let result = outcome(
        manager
            .run_agent(AGENT_ID, AgentPromptInput::Text("go".to_owned()), None)
            .await
            .map(|_| JsValue::Null),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    set_mode(0o700);
    let masked = errors
        .lock()
        .expect("errors")
        .iter()
        .map(|entry| {
            let [JsValue::Object(bindings), message] = entry.as_array().expect("entry") else {
                panic!("an error log is [bindings, message]");
            };
            let mut bindings = bindings.clone();
            let mut err = bindings
                .get("err")
                .and_then(JsValue::as_object)
                .expect("err")
                .clone();
            let path = err
                .get("path")
                .and_then(JsValue::as_str)
                .expect("path")
                .to_owned();
            let err_message = err
                .get("message")
                .and_then(JsValue::as_str)
                .expect("message");
            let masked_message = err_message.replace(&path, &mask_temp_path(&path));
            err.insert("message", text(&masked_message));
            err.insert("path", text(&mask_temp_path(&path)));
            bindings.insert("err", JsValue::Object(err));
            JsValue::Array(vec![JsValue::Object(bindings), message.clone()])
        })
        .collect();
    object(vec![
        ("result", result),
        ("errors", JsValue::Array(masked)),
        ("feed", JsValue::Array(feed.lock().expect("feed").clone())),
    ])
}

/// A catalog that names the agent it was built for.
struct CallerCatalog(String);

impl PaseoToolCatalog for CallerCatalog {
    fn tools(&self) -> Vec<PaseoToolDefinition> {
        vec![PaseoToolDefinition {
            name: self.0.clone(),
            title: None,
            description: String::new(),
            input_schema: None,
            output_schema: None,
        }]
    }
    fn get_tool(&self, _name: &str) -> Option<PaseoToolDefinition> {
        None
    }
    fn execute_tool(
        &self,
        _name: &str,
        _input: JsValue,
        _context: Option<PaseoToolExecutionContext>,
    ) -> BoxFuture<'_, AgentResult<JsValue>> {
        Box::pin(async { Err(AgentError::new("unused")) })
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn catalog_scenario(cwd: &str, home: &Path) -> JsValue {
    const GAMMA_ID: &str = "00000000-0000-4000-8000-0000000000f5";
    let calls = Calls::default();
    let factory_log: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let working: PaseoToolCatalogFactory = {
        let log = Arc::clone(&factory_log);
        Arc::new(move |context: PaseoToolRuntimeContext| {
            log.lock().expect("log").push(object(vec![
                (
                    "callerAgentId",
                    context
                        .caller_agent_id
                        .as_deref()
                        .map_or(JsValue::Undefined, text),
                ),
                (
                    "paseoToolPolicy",
                    context.paseo_tool_policy.unwrap_or(JsValue::Undefined),
                ),
            ]));
            let caller = context.caller_agent_id.unwrap_or_default();
            Box::pin(
                async move { Ok(Arc::new(CallerCatalog(caller)) as Arc<dyn PaseoToolCatalog>) },
            )
        })
    };
    let failing: PaseoToolCatalogFactory = Arc::new(|_| {
        Box::pin(async { Err::<Arc<dyn PaseoToolCatalog>, _>(AgentError::new("catalog broke")) })
    });
    let mut native = json(CAPABILITIES);
    if let JsValue::Object(capabilities) = &mut native {
        capabilities.insert("supportsNativePaseoTools", JsValue::Bool(true));
    }
    let client = |provider: &str, capabilities: Option<&JsValue>| {
        let mut fake = spec(provider);
        if let Some(capabilities) = capabilities {
            fake.capabilities = capabilities.clone();
        }
        (
            provider.to_owned(),
            Arc::new(FakeClient {
                spec: fake,
                calls: Arc::clone(&calls),
            }) as Arc<dyn AgentClient>,
        )
    };
    let clients = vec![
        client("native", Some(&native)),
        client("plain", None),
        client("quiet", Some(&native)),
    ];
    let provider_definitions = clients
        .iter()
        .map(|(provider, _)| (provider.clone(), enabled()))
        .collect();
    let manager = AgentManager::new(AgentManagerOptions {
        clients,
        provider_definitions,
        registry: Some(AgentStorage::new(home.join("catalog"))),
        paseo_tool_catalog_factory: Some(Arc::clone(&working)),
        mcp_auth_token: Some("secret-token".to_owned()),
        resolve_paseo_tool_policy: Some(Arc::new(|provider| {
            Some(if provider == "quiet" {
                object(vec![("enabled", JsValue::Bool(false))])
            } else {
                object(vec![
                    ("enabled", JsValue::Bool(true)),
                    ("name", text(provider)),
                ])
            })
        })),
        ..AgentManagerOptions::default()
    });
    // With an MCP URL on the internal path (/mcp/agents) the launch config
    // carries the internal Paseo server, which a provider that gets the tools
    // natively launches without.
    manager.set_mcp_base_url(Some("http://127.0.0.1:1/mcp/agents".to_owned()));
    let mut steps = Vec::new();
    let mut step = |name: &'static str, result: JsValue| {
        steps.push(object(vec![
            ("name", text(name)),
            ("result", result),
            (
                "factory",
                JsValue::Array(std::mem::take(&mut *factory_log.lock().expect("log"))),
            ),
            (
                "calls",
                JsValue::Array(std::mem::take(&mut *calls.lock().expect("calls"))),
            ),
        ]));
    };
    let create = |provider: &'static str, id: Option<&'static str>| {
        let manager = manager.clone();
        let cwd = cwd.to_owned();
        async move {
            outcome(
                manager
                    .create_agent(
                        object(vec![("provider", text(provider)), ("cwd", text(&cwd))]),
                        id.map(str::to_owned),
                        CreateAgentOptions::default(),
                    )
                    .await
                    .map(|agent| text(&agent.provider)),
            )
        }
    };
    step("native", create("native", Some(AGENT_ID)).await);
    step("plain", create("plain", Some(OTHER_ID)).await);
    step("quiet", create("quiet", Some(GAMMA_ID)).await);
    manager.set_paseo_tools_enabled(false);
    step("toolsOff", create("native", None).await);
    manager.set_paseo_tools_enabled(true);
    manager.set_paseo_tool_catalog_factory(None);
    step("noFactory", create("native", None).await);
    manager.set_paseo_tool_catalog_factory(Some(failing));
    step("failing", create("native", None).await);
    manager.set_paseo_tool_catalog_factory(Some(working));
    let resumed = manager
        .resume_agent_from_persistence(
            object(vec![
                ("provider", text("native")),
                ("sessionId", text("sess-c")),
                ("nativeHandle", text("thread-c")),
                (
                    "metadata",
                    object(vec![("cwd", text(cwd)), ("model", text("m"))]),
                ),
            ]),
            None,
            None,
            ResumeAgentOptions::default(),
            None,
        )
        .await
        .map(|agent| text(&agent.provider));
    step("resume", outcome(resumed));
    let reloaded = manager
        .reload_agent_session(AGENT_ID, None, ReloadAgentOptions::default())
        .await
        .map(|agent| text(&agent.provider));
    step("reload", outcome(reloaded));
    object(vec![("steps", JsValue::Array(steps))])
}

/// A plugin lifecycle that records each request; `adds_env` makes it a plugin
/// that also records events and adds a variable to the env of the requests it
/// transforms, where the other one only validates.
struct RecordingLifecycle {
    log: Arc<Mutex<Vec<JsValue>>>,
    order: Arc<Mutex<Vec<JsValue>>>,
    adds_env: bool,
}

impl PluginLifecycle for RecordingLifecycle {
    fn before(&self, name: &str, request: JsValue) -> BoxFuture<'_, AgentResult<JsValue>> {
        let name = name.to_owned();
        Box::pin(async move {
            self.log.lock().expect("log").push(JsValue::Array(vec![
                text("before"),
                text(&name),
                request.clone(),
            ]));
            let checked = NoPluginLifecycle.before(&name, request).await?;
            if !self.adds_env {
                return Ok(checked);
            }
            let added = if name == "agent.session_open" {
                "PLUGIN_OPEN"
            } else {
                "PLUGIN_CREATE"
            };
            let mut env = checked
                .get("env")
                .and_then(JsValue::as_object)
                .cloned()
                .unwrap_or_default();
            env.insert(added, text("1"));
            let mut out = checked.as_object().cloned().expect("a request object");
            out.insert("env", JsValue::Object(env));
            Ok(JsValue::Object(out))
        })
    }

    fn emit(&self, name: &str, event: JsValue) {
        // The validating plugin does not listen for events.
        if self.adds_env {
            self.order
                .lock()
                .expect("order")
                .push(JsValue::Array(vec![text("emit"), text(name)]));
            self.log.lock().expect("log").push(JsValue::Array(vec![
                text("emit"),
                text(name),
                event,
            ]));
        }
    }
}

/// A plugin that refuses to open any session.
struct RefusingLifecycle;

impl PluginLifecycle for RefusingLifecycle {
    fn before(&self, name: &str, request: JsValue) -> BoxFuture<'_, AgentResult<JsValue>> {
        let name = name.to_owned();
        Box::pin(async move {
            if name == "agent.session_open" {
                return Err(AgentError::new("blocked by plugin"));
            }
            NoPluginLifecycle.before(&name, request).await
        })
    }

    fn emit(&self, _name: &str, _event: JsValue) {}
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn plugin_lifecycle_scenario(cwd: &str, home: &Path) -> JsValue {
    const INTERNAL_ID: &str = "00000000-0000-4000-8000-0000000000f7";
    let turns = json(SCENARIO_TURNS);
    let log: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let order: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let build = |name: &str, adds_env: bool, configure: &dyn Fn(&mut Spec)| {
        let calls = Calls::default();
        let mut fake = spec("fake");
        configure(&mut fake);
        let manager = AgentManager::new(AgentManagerOptions {
            clients: vec![(
                "fake".to_owned(),
                Arc::new(FakeClient {
                    spec: fake,
                    calls: Arc::clone(&calls),
                }) as Arc<dyn AgentClient>,
            )],
            provider_definitions: vec![("fake".to_owned(), enabled())],
            registry: Some(AgentStorage::new(home.join(format!("plugin-{name}")))),
            plugin_lifecycle_host: Some(Arc::new(RecordingLifecycle {
                log: Arc::clone(&log),
                order: Arc::clone(&order),
                adds_env,
            })),
            ..AgentManagerOptions::default()
        });
        let feed = record_feed(&manager);
        if adds_env {
            let sink = Arc::clone(&order);
            let unsubscribe = manager
                .subscribe(
                    Arc::new(move |event| {
                        let (kind, detail) = match event {
                            AgentManagerEvent::AgentState(agent) => {
                                ("agent_state", text(agent.lifecycle.as_str()))
                            }
                            AgentManagerEvent::AgentStream { event, .. } => (
                                "agent_stream",
                                event.get("type").cloned().unwrap_or(JsValue::Null),
                            ),
                            AgentManagerEvent::TimelineReplacement { .. } => {
                                ("timeline_replacement", JsValue::Null)
                            }
                            AgentManagerEvent::ProviderSubagent(_) => {
                                ("provider_subagent", JsValue::Null)
                            }
                        };
                        sink.lock().expect("order").push(JsValue::Array(vec![
                            text("event"),
                            text(kind),
                            detail,
                        ]));
                    }),
                    SubscribeOptions::default(),
                )
                .expect("subscribe");
            std::mem::forget(unsubscribe);
        }
        (manager, calls, feed)
    };
    let (manager, calls, feed) = build("recording", true, &|fake| {
        scripted(fake, &["ask", "long", "failed"]);
        fake.response = turns.get("response").cloned();
        fake.interrupt = turns.get("interrupt").cloned();
        fake.import = turns.get("import").cloned();
    });
    manager
        .create_agent(
            object(vec![
                ("provider", text("fake")),
                ("cwd", text(cwd)),
                ("title", text("Plugin agent")),
            ]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions {
                labels: Some(object(vec![("paseo.parent-agent-id", text(OTHER_ID))])),
                workspace_id: Some("wks_1".to_owned()),
                ..CreateAgentOptions::default()
            },
        )
        .await
        .expect("create");
    manager
        .create_agent(
            object(vec![
                ("provider", text("fake")),
                ("cwd", text(cwd)),
                ("internal", JsValue::Bool(true)),
            ]),
            Some(INTERNAL_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create internal");
    let prompt = |body: &str| AgentPromptInput::Text(body.to_owned());
    let mut second = manager
        .stream_agent(AGENT_ID, prompt("remove x"), None)
        .expect("second stream");
    let mut second_events = vec![second.next().await.expect("first").expect("event")];
    manager
        .wait_for_agent_event(AGENT_ID, WaitForAgentOptions::default())
        .await
        .expect("permission wait");
    manager
        .respond_to_permission(
            AGENT_ID,
            "perm-1",
            object(vec![("behavior", text("allow"))]),
        )
        .await
        .expect("respond");
    collect(&mut second, &mut second_events).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut third = manager
        .stream_agent(AGENT_ID, prompt("long task"), None)
        .expect("third stream");
    let mut third_events = vec![third.next().await.expect("first").expect("event")];
    tokio::time::sleep(Duration::from_millis(50)).await;
    manager.cancel_agent_run(AGENT_ID).await.expect("cancel");
    collect(&mut third, &mut third_events).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut fourth = manager
        .stream_agent(AGENT_ID, prompt("fail me"), None)
        .expect("fourth stream");
    let mut fourth_events = Vec::new();
    collect(&mut fourth, &mut fourth_events).await;
    let fourth_result = outcome(Ok(JsValue::Array(fourth_events)));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let archived = outcome(manager.archive_agent(AGENT_ID).await.map(|_| JsValue::Null));
    let archived_internal = outcome(
        manager
            .archive_agent(INTERNAL_ID)
            .await
            .map(|_| JsValue::Null),
    );
    let archived_again = outcome(
        manager
            .archive_snapshot(AGENT_ID, "2026-07-12T10:00:00.000Z".to_owned())
            .await
            .map(|_| JsValue::Null),
    );
    let resumed = outcome(
        manager
            .resume_agent_from_persistence(
                object(vec![
                    ("provider", text("fake")),
                    ("sessionId", text("sess-p")),
                    ("nativeHandle", text("thread-p")),
                    (
                        "metadata",
                        object(vec![("cwd", text(cwd)), ("model", text("m"))]),
                    ),
                ]),
                None,
                Some(OTHER_ID.to_owned()),
                ResumeAgentOptions {
                    workspace_id: Some("wks_2".to_owned()),
                    ..ResumeAgentOptions::default()
                },
                None,
            )
            .await
            .map(|agent| text(&agent.id)),
    );
    let reloaded = outcome(
        manager
            .reload_agent_session(OTHER_ID, None, ReloadAgentOptions::default())
            .await
            .map(|agent| text(&agent.id)),
    );
    let imported = outcome(
        manager
            .import_provider_session(ImportProviderSessionRequest {
                provider: "fake".to_owned(),
                provider_handle_id: "h1".to_owned(),
                cwd: cwd.to_owned(),
                workspace_id: "wks_3".to_owned(),
                labels: None,
            })
            .await
            .map(|agent| text(&agent.provider)),
    );
    let resumed_archived = outcome(
        manager
            .resume_agent_from_persistence(
                object(vec![
                    ("provider", text("fake")),
                    ("sessionId", text("sess-p")),
                    ("nativeHandle", text("thread-p")),
                    (
                        "metadata",
                        object(vec![("cwd", text(cwd)), ("model", text("m"))]),
                    ),
                ]),
                None,
                Some(AGENT_ID.to_owned()),
                ResumeAgentOptions {
                    workspace_id: Some("wks_1".to_owned()),
                    ..ResumeAgentOptions::default()
                },
                None,
            )
            .await
            .map(|agent| text(&agent.id)),
    );
    let archive_failed = {
        let base = home.join("plugin-recording");
        let mut dirs = vec![base.clone()];
        for entry in std::fs::read_dir(&base).expect("record directories") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                dirs.push(path);
            }
        }
        let set_mode = |mode: u32| {
            for dir in &dirs {
                std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(mode))
                    .expect("permissions");
            }
        };
        set_mode(0o500);
        let result = manager.archive_agent(OTHER_ID).await;
        set_mode(0o700);
        text(if result.is_ok() { "ok" } else { "threw" })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    let recording_log = std::mem::take(&mut *log.lock().expect("log"));
    let (befores, emits): (Vec<_>, Vec<_>) = recording_log.into_iter().partition(|entry| {
        matches!(entry, JsValue::Array(items)
                if items.first().and_then(JsValue::as_str) == Some("before"))
    });
    let recording_case = object(vec![
        ("secondEvents", JsValue::Array(second_events)),
        ("thirdEvents", JsValue::Array(third_events)),
        ("fourthEvents", fourth_result),
        ("archived", archived),
        ("archivedInternal", archived_internal),
        ("archivedAgain", archived_again),
        ("resumed", resumed),
        ("reloaded", reloaded),
        ("imported", imported),
        ("resumedArchived", resumed_archived),
        ("archiveFailed", archive_failed),
        (
            "order",
            JsValue::Array(std::mem::take(&mut *order.lock().expect("order"))),
        ),
        ("befores", JsValue::Array(befores)),
        ("emits", JsValue::Array(emits)),
        (
            "calls",
            JsValue::Array(calls.lock().expect("calls").clone()),
        ),
        ("feed", JsValue::Array(feed.lock().expect("feed").clone())),
    ]);

    // The runtime with no plugin loaded: the requests are only validated.
    let (plain, plain_calls, _) = build("validating", false, &|_| {});
    let env = |name: &str, value: JsValue| {
        let mut env = JsObject::new();
        env.insert(name, value);
        env
    };
    let created = outcome(
        plain
            .create_agent(
                object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
                None,
                CreateAgentOptions {
                    env: Some(env("KEEP", text("me"))),
                    ..CreateAgentOptions::default()
                },
            )
            .await
            .map(|agent| text(&agent.provider)),
    );
    let bad_env = outcome(
        plain
            .create_agent(
                object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
                None,
                CreateAgentOptions {
                    env: Some(env("BAD", JsValue::Number(1.0))),
                    ..CreateAgentOptions::default()
                },
            )
            .await
            .map(|agent| text(&agent.provider)),
    );
    let validating_case = object(vec![
        ("created", created),
        ("badEnv", bad_env),
        (
            "log",
            JsValue::Array(std::mem::take(&mut *log.lock().expect("log"))),
        ),
        (
            "calls",
            JsValue::Array(plain_calls.lock().expect("calls").clone()),
        ),
    ]);
    // A plugin that refuses to open the session.
    let refusing_calls = Calls::default();
    let refusing = AgentManager::new(AgentManagerOptions {
        clients: vec![(
            "fake".to_owned(),
            Arc::new(FakeClient {
                spec: spec("fake"),
                calls: Arc::clone(&refusing_calls),
            }) as Arc<dyn AgentClient>,
        )],
        provider_definitions: vec![("fake".to_owned(), enabled())],
        registry: Some(AgentStorage::new(home.join("plugin-refusing"))),
        plugin_lifecycle_host: Some(Arc::new(RefusingLifecycle)),
        ..AgentManagerOptions::default()
    });
    let refusing_feed = record_feed(&refusing);
    let refused = outcome(
        refusing
            .create_agent(
                object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
                None,
                CreateAgentOptions::default(),
            )
            .await
            .map(|agent| text(&agent.provider)),
    );
    let refusing_case = object(vec![
        ("refused", refused),
        (
            "calls",
            JsValue::Array(refusing_calls.lock().expect("calls").clone()),
        ),
        (
            "feed",
            JsValue::Array(refusing_feed.lock().expect("feed").clone()),
        ),
    ]);
    object(vec![
        ("recordingCase", recording_case),
        ("validatingCase", validating_case),
        ("refusingCase", refusing_case),
    ])
}

/// The manager's `logger.trace` calls through a run of every foreground
/// path: a permission turn, a cancelled turn refused a second run, a failed
/// turn, coalesced rows, a turn whose events arrive while it starts, and a
/// close. The traces compare in the order they were logged.
#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn trace_scenario(cwd: &str, home: &Path) -> JsValue {
    let turns = json(SCENARIO_TURNS);
    let traces: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let sink = Arc::clone(&traces);
    let calls = Calls::default();
    let mut fake = spec("fake");
    scripted(
        &fake,
        &["ask", "long", "failed", "coalesce", "startSlowDone"],
    );
    fake.response = turns.get("response").cloned();
    fake.interrupt = turns.get("interrupt").cloned();
    let manager = AgentManager::new(AgentManagerOptions {
        clients: vec![(
            "fake".to_owned(),
            Arc::new(FakeClient {
                spec: fake,
                calls: Arc::clone(&calls),
            }) as Arc<dyn AgentClient>,
        )],
        provider_definitions: vec![("fake".to_owned(), enabled())],
        registry: Some(AgentStorage::new(home.join("trace"))),
        log_trace: Some(Arc::new(move |bindings, message| {
            sink.lock()
                .expect("traces")
                .push(JsValue::Array(vec![bindings, text(message)]));
        })),
        ..AgentManagerOptions::default()
    });
    manager
        .create_agent(
            object(vec![
                ("provider", text("fake")),
                ("cwd", text(cwd)),
                ("title", text("Traced")),
            ]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions {
                workspace_id: Some("wks_1".to_owned()),
                ..CreateAgentOptions::default()
            },
        )
        .await
        .expect("create");
    let prompt = |body: &str| AgentPromptInput::Text(body.to_owned());
    let mut second = manager
        .stream_agent(AGENT_ID, prompt("remove x"), None)
        .expect("second stream");
    let mut second_events = vec![second.next().await.expect("first").expect("event")];
    manager
        .wait_for_agent_event(AGENT_ID, WaitForAgentOptions::default())
        .await
        .expect("permission wait");
    manager
        .respond_to_permission(
            AGENT_ID,
            "perm-1",
            object(vec![("behavior", text("allow"))]),
        )
        .await
        .expect("respond");
    collect(&mut second, &mut second_events).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut third = manager
        .stream_agent(AGENT_ID, prompt("long task"), None)
        .expect("third stream");
    let mut third_events = vec![third.next().await.expect("first").expect("event")];
    tokio::time::sleep(Duration::from_millis(50)).await;
    let duplicate = outcome(
        manager
            .stream_agent(AGENT_ID, prompt("dup"), None)
            .map(|_| JsValue::Null),
    );
    manager.cancel_agent_run(AGENT_ID).await.expect("cancel");
    collect(&mut third, &mut third_events).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut streams = Vec::new();
    for body in ["fail me", "coalesce", "start slowly"] {
        let mut stream = manager
            .stream_agent(AGENT_ID, prompt(body), None)
            .expect("stream");
        let mut events = Vec::new();
        collect(&mut stream, &mut events).await;
        streams.push(outcome(Ok(JsValue::Array(events))));
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let closed = outcome(manager.close_agent(AGENT_ID).await.map(|()| JsValue::Null));
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    let mut streams = streams.into_iter();
    let mut next = || streams.next().expect("stream outcome");
    let (failed, coalesced, staged) = (next(), next(), next());
    object(vec![
        ("secondEvents", JsValue::Array(second_events)),
        ("thirdEvents", JsValue::Array(third_events)),
        ("duplicate", duplicate),
        ("failed", failed),
        ("coalesced", coalesced),
        ("staged", staged),
        ("closed", closed),
        (
            "traces",
            JsValue::Array(std::mem::take(&mut *traces.lock().expect("traces"))),
        ),
        (
            "calls",
            JsValue::Array(calls.lock().expect("calls").clone()),
        ),
    ])
}

#[allow(
    clippy::too_many_lines,
    reason = "one scripted scenario mirrors its node twin"
)]
async fn archive_scenario(cwd: &str, home: &Path) -> JsValue {
    const PARENT_LABEL: &str = "paseo.parent-agent-id";
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("archive"));
    let manager = manager_with(&calls, &registry, vec![(spec("fake"), enabled())]);
    let feed = record_feed(&manager);
    let create =
        |id: &'static str, labels: Vec<(&'static str, JsValue)>, workspace: &'static str| {
            let manager = manager.clone();
            let cwd = cwd.to_owned();
            async move {
                manager
                    .create_agent(
                        object(vec![("provider", text("fake")), ("cwd", text(&cwd))]),
                        Some(id.to_owned()),
                        CreateAgentOptions {
                            labels: Some(object(labels)),
                            workspace_id: Some(workspace.to_owned()),
                            ..CreateAgentOptions::default()
                        },
                    )
                    .await
                    .expect("create");
            }
        };
    let parent = || vec![(PARENT_LABEL, text(AGENT_ID))];
    create(AGENT_ID, vec![], "wks_1").await;
    create(CHILD_SAME_ID, parent(), "wks_1").await;
    create(CHILD_OTHER_WORKSPACE_ID, parent(), "wks_2").await;
    let mut tab = parent();
    tab.push(("paseo.open-agent-tab.c1", text("true")));
    create(CHILD_TAB_ID, tab, "wks_1").await;
    create(CHILD_STORED_ID, parent(), "wks_1").await;
    manager.close_agent(CHILD_STORED_ID).await.expect("close");
    manager.flush().await;
    registry.flush().await;
    let mut results = vec![outcome(
        manager.archive_agent(AGENT_ID).await.map(JsValue::String),
    )];
    manager.flush().await;
    registry.flush().await;
    let names = [
        ("parent", AGENT_ID),
        ("same", CHILD_SAME_ID),
        ("other", CHILD_OTHER_WORKSPACE_ID),
        ("tab", CHILD_TAB_ID),
        ("stored", CHILD_STORED_ID),
    ];
    let stored_records = |registry: &AgentStorage| {
        let registry = registry.clone();
        async move {
            let mut out = JsObject::new();
            for (name, id) in names {
                out.insert(name, registry.get(id).await.unwrap_or(JsValue::Null));
            }
            JsValue::Object(out)
        }
    };
    let stored = stored_records(&registry).await;
    results.push(outcome(
        manager
            .unarchive_snapshot(
                AGENT_ID,
                Some(UnarchiveUpdates {
                    workspace_id: Some("wks_3".to_owned()),
                    labels: Some(json(r#"{"a":"b","gone":null}"#)),
                }),
            )
            .await
            .map(JsValue::Bool),
    ));
    results.push(outcome(
        manager
            .unarchive_snapshot(AGENT_ID, None)
            .await
            .map(JsValue::Bool),
    ));
    results.push(outcome(
        manager
            .unarchive_snapshot(UNKNOWN_ID, None)
            .await
            .map(JsValue::Bool),
    ));
    results.push(outcome(manager.detach_agent(CHILD_SAME_ID).await.map(
        |detached| {
            object(vec![
                (
                    "id",
                    detached
                        .record
                        .get("id")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
                (
                    "labels",
                    detached
                        .record
                        .get("labels")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
                ("live", JsValue::Bool(detached.live)),
                (
                    "previousParentAgentId",
                    detached
                        .previous_parent_agent_id
                        .map_or(JsValue::Null, JsValue::String),
                ),
            ])
        },
    )));
    results.push(outcome(
        manager
            .clear_agent_attention(CHILD_SAME_ID)
            .await
            .map(|()| JsValue::Null),
    ));
    results.push(outcome(
        manager
            .clear_agent_attention(UNKNOWN_ID)
            .await
            .map(|()| JsValue::Null),
    ));
    manager.flush().await;
    registry.flush().await;
    let after_stored = stored_records(&registry).await;
    let by_handle_calls = Calls::default();
    let by_handle_registry = AgentStorage::new(home.join("archive-handle"));
    let warns: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let warn_sink = Arc::clone(&warns);
    let mut failing = spec("fake");
    failing.archive_fails = true;
    let by_handle = AgentManager::new(AgentManagerOptions {
        clients: vec![(
            "fake".to_owned(),
            Arc::new(FakeClient {
                spec: failing,
                calls: Arc::clone(&by_handle_calls),
            }) as Arc<dyn AgentClient>,
        )],
        provider_definitions: vec![("fake".to_owned(), enabled())],
        registry: Some(by_handle_registry.clone()),
        log_warn: Some(Arc::new(move |bindings, message| {
            warn_sink
                .lock()
                .expect("warns")
                .push(JsValue::Array(vec![bindings, text(message)]));
        })),
        ..AgentManagerOptions::default()
    });
    by_handle
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions {
                labels: Some(object(vec![])),
                workspace_id: Some("wks_1".to_owned()),
                ..CreateAgentOptions::default()
            },
        )
        .await
        .expect("create");
    by_handle.close_agent(AGENT_ID).await.expect("close");
    by_handle_registry.flush().await;
    let archived_record = outcome(
        by_handle
            .archive_snapshot(AGENT_ID, "2026-07-12T10:00:00.000Z".to_owned())
            .await,
    );
    let handle = by_handle_registry
        .get(AGENT_ID)
        .await
        .and_then(|record| record.get("persistence").cloned())
        .expect("handle");
    let unarchived = outcome(
        by_handle
            .unarchive_snapshot_by_handle(&handle)
            .await
            .map(|()| JsValue::Null),
    );
    by_handle_registry.flush().await;
    let by_handle_record = by_handle_registry
        .get(AGENT_ID)
        .await
        .unwrap_or(JsValue::Null);
    let by_handle_calls = by_handle_calls.lock().expect("calls").clone();
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("stored", stored),
        ("afterStored", after_stored),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
        (
            "byHandle",
            object(vec![
                ("archivedRecord", archived_record),
                ("unarchived", unarchived),
                ("record", by_handle_record),
                ("calls", JsValue::Array(by_handle_calls)),
                (
                    "warns",
                    JsValue::Array(warns.lock().expect("warns").clone()),
                ),
            ]),
        ),
    ])
}

/// A closed agent whose stored dates are not ISO text: `archiveSnapshot`
/// then `unarchiveSnapshot` read them with `new Date(text)` and
/// `Date.parse`, which accept every form V8 reads. `times` are the
/// `agent_state` dates in epoch milliseconds, which `normalize` leaves alone.
async fn stored_dates_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("stored-dates"));
    let first = manager_with(&calls, &registry, vec![(spec("fake"), enabled())]);
    first
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions {
                labels: Some(object(vec![])),
                workspace_id: Some("wks_1".to_owned()),
                ..CreateAgentOptions::default()
            },
        )
        .await
        .expect("create");
    first.close_agent(AGENT_ID).await.expect("close");
    first.flush().await;
    registry.flush().await;
    let stored = registry.get(AGENT_ID).await.expect("stored");
    let JsValue::Object(record) = &stored else {
        panic!("record");
    };
    let mut record = record.clone();
    record.insert("id", text(OTHER_ID));
    record.insert("createdAt", text("Jan 3 2020 00:00:00 GMT"));
    record.insert("updatedAt", text("Wed, 01 Jan 2020 12:00:00 GMT"));
    record.insert("lastUserMessageAt", text("1/3/2020 00:00:00 GMT"));
    registry
        .upsert(JsValue::Object(record))
        .await
        .expect("upsert");
    let second = manager_with(&calls, &registry, vec![(spec("fake"), enabled())]);
    let feed = record_feed(&second);
    let mut results = vec![outcome(
        second
            .archive_snapshot(OTHER_ID, "Jan 4 2020 00:00:00 GMT".to_owned())
            .await,
    )];
    // Epoch milliseconds are below 2^53.
    #[allow(clippy::cast_precision_loss)]
    let epoch_millis =
        |payload: &JsValue, key: &str| match payload.get(key).and_then(JsValue::as_str) {
            Some(at) => JsValue::Number(date_parse(at).expect("ISO date") as f64),
            None => JsValue::Null,
        };
    let times: Vec<JsValue> = feed
        .lock()
        .expect("feed")
        .iter()
        .filter_map(|entry| match entry.as_array()? {
            [kind, payload] if kind.as_str() == Some("agent_state") => Some(payload),
            _ => None,
        })
        .map(|payload| {
            JsValue::Array(
                ["createdAt", "updatedAt", "lastUserMessageAt"]
                    .map(|key| epoch_millis(payload, key))
                    .to_vec(),
            )
        })
        .collect();
    results.push(outcome(
        second
            .unarchive_snapshot(OTHER_ID, None)
            .await
            .map(JsValue::Bool),
    ));
    second.flush().await;
    registry.flush().await;
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("times", JsValue::Array(times)),
        ("feed", JsValue::Array(feed)),
        (
            "stored",
            registry.get(OTHER_ID).await.unwrap_or(JsValue::Null),
        ),
    ])
}

async fn loading_scenario(cwd: &str, home: &Path) -> JsValue {
    const GHOST_ID: &str = "00000000-0000-4000-8000-0000000000c3";
    const MISSING_ID: &str = "00000000-0000-4000-8000-0000000000d4";
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("loading"));
    let first = manager_with(&calls, &registry, vec![(spec("fake"), enabled())]);
    first
        .create_agent(
            object(vec![
                ("provider", text("fake")),
                ("cwd", text(cwd)),
                ("model", text("m1")),
            ]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions {
                labels: Some(object(vec![("surface", text("x"))])),
                workspace_id: Some("wks_1".to_owned()),
                ..CreateAgentOptions::default()
            },
        )
        .await
        .expect("create");
    first.flush().await;
    registry.flush().await;
    let stored = registry.get(AGENT_ID).await.expect("stored");
    let variant = |id: &str, changes: Vec<(&str, JsValue)>| {
        let JsValue::Object(record) = &stored else {
            panic!("record");
        };
        let mut record = record.clone();
        record.insert("id", text(id));
        for (key, value) in changes {
            record.insert(key, value);
        }
        JsValue::Object(record)
    };
    registry
        .upsert(variant(
            OTHER_ID,
            vec![("persistence", JsValue::Null), ("title", text("No handle"))],
        ))
        .await
        .expect("upsert");
    registry
        .upsert(variant(GHOST_ID, vec![("provider", text("ghost"))]))
        .await
        .expect("upsert");
    let second = manager_with(&calls, &registry, vec![(spec("fake"), enabled())]);
    let feed = record_feed(&second);
    let deps = |broadcast_timeline: bool| EnsureAgentLoadedDeps {
        agent_manager: second.clone(),
        agent_storage: registry.clone(),
        valid_providers: None,
        broadcast_timeline,
    };
    let load = |id: &'static str, broadcast_timeline: bool| {
        let deps = deps(broadcast_timeline);
        async move {
            outcome(
                ensure_agent_loaded(id, &deps)
                    .await
                    .map(|agent| to_agent_payload(&agent.payload_view(), None).expect("payload")),
            )
        }
    };
    let (a, b) = tokio::join!(load(AGENT_ID, false), load(AGENT_ID, true));
    let mut results = vec![a, b];
    results.push(load(AGENT_ID, false).await);
    results.push(load(OTHER_ID, false).await);
    results.push(load(GHOST_ID, false).await);
    results.push(load(MISSING_ID, false).await);
    tokio::time::sleep(Duration::from_millis(50)).await;
    second.flush().await;
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
    ])
}

async fn shutdown_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("shutdown"));
    let mut fake = spec("fake");
    fake.create_delay = Some(Duration::from_millis(150));
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    let order = Arc::new(Mutex::new(Vec::new()));
    let create = |id: &'static str, name: &'static str| {
        let manager = manager.clone();
        let order = Arc::clone(&order);
        let cwd = cwd.to_owned();
        tokio::spawn(async move {
            let line = match manager
                .create_agent(
                    object(vec![("provider", text("fake")), ("cwd", text(&cwd))]),
                    Some(id.to_owned()),
                    CreateAgentOptions::default(),
                )
                .await
            {
                Ok(_) => format!("created {name}"),
                Err(error) => format!("failed {name}: {}", error.message),
            };
            order.lock().expect("order").push(JsValue::String(line));
        })
    };
    let push = |line: &str| order.lock().expect("order").push(text(line));
    let first = create(AGENT_ID, "first");
    tokio::time::sleep(Duration::from_millis(20)).await;
    manager.flush().await;
    push("flush done");
    first.await.expect("first");
    let second = create(OTHER_ID, "second");
    tokio::time::sleep(Duration::from_millis(20)).await;
    manager.prepare_for_shutdown();
    manager.flush_for_shutdown().await;
    push("flushForShutdown done");
    second.await.expect("second");
    create(UNKNOWN_ID, "third").await.expect("third");
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    let order = order.lock().expect("order").clone();
    object(vec![
        ("order", JsValue::Array(order)),
        ("calls", JsValue::Array(calls)),
    ])
}

async fn outofband_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("outofband"));
    let manager = manager_with(&calls, &registry, vec![(spec("fake"), enabled())]);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let attempt = |id: &str, prompt: &str, client_message_id: Option<&str>| {
        let options = client_message_id.map(|id| AgentRunOptions {
            client_message_id: Some(id.to_owned()),
            ..AgentRunOptions::default()
        });
        outcome(
            manager
                .try_run_out_of_band(
                    id,
                    &AgentPromptInput::Text(prompt.to_owned()),
                    options.as_ref(),
                )
                .map(JsValue::Bool),
        )
    };
    let pause = Duration::from_millis(50);
    let mut results = vec![attempt(AGENT_ID, "hello", None)];
    results.push(attempt(AGENT_ID, "/goal pause", Some("client-oob")));
    tokio::time::sleep(pause).await;
    results.push(attempt(AGENT_ID, "/goal fail", Some("")));
    tokio::time::sleep(pause).await;
    results.push(attempt(UNKNOWN_ID, "/goal pause", None));
    tokio::time::sleep(pause).await;
    manager.flush().await;
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    let feed = feed.lock().expect("feed").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("calls", JsValue::Array(calls)),
        ("feed", JsValue::Array(feed)),
        (
            "rows",
            JsValue::Array(manager.get_timeline_rows(AGENT_ID).expect("rows")),
        ),
    ])
}

/// `runAgent` registers its pending run synchronously: polls the run once so
/// it is registered before the caller waits, then lets it finish in a task.
async fn start_run(
    manager: &AgentManager,
    prompt: &'static str,
) -> tokio::task::JoinHandle<JsValue> {
    let manager = manager.clone();
    let mut run = Box::pin(async move {
        match manager
            .run_agent(AGENT_ID, AgentPromptInput::Text(prompt.to_owned()), None)
            .await
        {
            Ok(run) => run
                .to_js()
                .get("finalText")
                .cloned()
                .unwrap_or(JsValue::Null),
            Err(error) => JsValue::String(error.message),
        }
    });
    match poll_once(&mut run).await {
        Some(done) => tokio::spawn(async move { done }),
        None => tokio::spawn(run),
    }
}

async fn runstart_scenario(cwd: &str, home: &Path) -> JsValue {
    let calls = Calls::default();
    let registry = AgentStorage::new(home.join("runstart"));
    let fake = spec("fake");
    scripted(&fake, &["startSlowDone", "startFail", "startSlowAbort"]);
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let start = |id: &'static str, signal: Option<AbortSignal>| {
        let manager = manager.clone();
        async move {
            outcome(
                manager
                    .wait_for_agent_run_start(id, signal)
                    .await
                    .map(|()| text("started")),
            )
        }
    };
    let mut results = vec![start(UNKNOWN_ID, None).await, start(AGENT_ID, None).await];
    let slow = start_run(&manager, "slow").await;
    results.push(start(AGENT_ID, None).await);
    results.push(outcome(Ok(slow.await.expect("slow run"))));
    let failed = start_run(&manager, "fail").await;
    results.push(start(AGENT_ID, None).await);
    results.push(outcome(Ok(failed.await.expect("failed run"))));
    let aborted = start_run(&manager, "abort").await;
    let stop = AbortController::default();
    let stop_signal = stop.signal();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        stop.abort(AbortReason::Value(text("stop")));
    });
    results.push(start(AGENT_ID, Some(stop_signal)).await);
    let pre = AbortController::default();
    pre.abort(AbortReason::Error(AgentError::new("pre")));
    results.push(start(AGENT_ID, Some(pre.signal())).await);
    results.push(start(AGENT_ID, None).await);
    results.push(outcome(Ok(aborted.await.expect("aborted run"))));
    results.push(start(AGENT_ID, None).await);
    tokio::time::sleep(Duration::from_millis(100)).await;
    manager.flush().await;
    registry.flush().await;
    let calls = calls.lock().expect("calls").clone();
    object(vec![
        ("results", JsValue::Array(results)),
        ("calls", JsValue::Array(calls)),
    ])
}

/// Polls `future` once.
async fn poll_once<F: Future + Unpin>(future: &mut F) -> Option<F::Output> {
    std::future::poll_fn(|context| {
        Poll::Ready(match Pin::new(&mut *future).poll(context) {
            Poll::Ready(output) => Some(output),
            Poll::Pending => None,
        })
    })
    .await
}

/// A wait on a busy agent holds one subscription until it settles, is
/// aborted, or is dropped by its caller; none of those leak it.
#[tokio::test]
async fn wait_releases_its_subscription() {
    let workspace = home("wait-cwd");
    let records = home("wait-records");
    let cwd = workspace.0.to_string_lossy().into_owned();
    let calls = Calls::default();
    let registry = AgentStorage::new(&records.0);
    let fake = spec("fake");
    scripted(&fake, &["held"]);
    let manager = manager_with(&calls, &registry, vec![(fake, enabled())]);
    let feed = record_feed(&manager);
    manager
        .create_agent(
            object(vec![("provider", text("fake")), ("cwd", text(&cwd))]),
            Some(AGENT_ID.to_owned()),
            CreateAgentOptions::default(),
        )
        .await
        .expect("create");
    let held = tokio::spawn({
        let manager = manager.clone();
        async move {
            manager
                .run_agent(AGENT_ID, AgentPromptInput::Text("hold".to_owned()), None)
                .await
        }
    });
    wait_for_turn_started(&feed, "turn-5").await;
    let idle = manager.subscription_count();

    let mut dropped =
        Box::pin(manager.wait_for_agent_event(AGENT_ID, WaitForAgentOptions::default()));
    assert!(poll_once(&mut dropped).await.is_none());
    assert_eq!(manager.subscription_count(), idle + 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut dropped)
            .await
            .is_err()
    );
    drop(dropped);
    assert_eq!(manager.subscription_count(), idle);

    let controller = AbortController::default();
    let mut aborted = Box::pin(manager.wait_for_agent_event(
        AGENT_ID,
        WaitForAgentOptions {
            signal: Some(controller.signal()),
            ..WaitForAgentOptions::default()
        },
    ));
    assert!(poll_once(&mut aborted).await.is_none());
    assert_eq!(manager.subscription_count(), idle + 1);
    controller.abort(AbortReason::Value(text("stop")));
    let error = aborted.await.expect_err("aborted");
    assert_eq!(
        (error.name.as_str(), error.message.as_str()),
        ("AbortError", "stop")
    );
    assert_eq!(manager.subscription_count(), idle);
    held.abort();
}

/// The node build waits 90 000 ms for a provider's listing (a real-time wait
/// the pinned differential cannot afford), so this runs on a paused clock.
#[tokio::test(start_paused = true)]
async fn importable_listing_gives_up_after_ninety_seconds() {
    let records = home("importable-timeout");
    let calls = Calls::default();
    let warns: Arc<Mutex<Vec<JsValue>>> = Arc::default();
    let warn_sink = Arc::clone(&warns);
    let mut listing = json(CAPABILITIES);
    if let JsValue::Object(capabilities) = &mut listing {
        capabilities.insert("supportsSessionListing", JsValue::Bool(true));
    }
    let session = |handle: &str| {
        object(vec![
            ("providerHandleId", text(handle)),
            ("cwd", text("/work")),
            ("title", JsValue::Null),
            ("firstPromptPreview", JsValue::Null),
            ("lastPromptPreview", JsValue::Null),
            ("lastActivityAt", number(1_700_000_000_000)),
        ])
    };
    let client = |provider: &str, delay_ms: f64| {
        let mut fake = spec(provider);
        fake.capabilities = listing.clone();
        fake.importable = Some(object(vec![
            ("delayMs", JsValue::Number(delay_ms)),
            ("sessions", JsValue::Array(vec![session(provider)])),
        ]));
        (
            provider.to_owned(),
            Arc::new(FakeClient {
                spec: fake,
                calls: Arc::clone(&calls),
            }) as Arc<dyn AgentClient>,
        )
    };
    let manager = AgentManager::new(AgentManagerOptions {
        clients: vec![client("last-moment", 89_999.0), client("hung", 90_001.0)],
        provider_definitions: ["last-moment", "hung"]
            .into_iter()
            .map(|provider| (provider.to_owned(), enabled()))
            .collect(),
        registry: Some(AgentStorage::new(&records.0)),
        log_warn: Some(Arc::new(move |bindings, message| {
            warn_sink
                .lock()
                .expect("warns")
                .push(JsValue::Array(vec![bindings, text(message)]));
        })),
        ..AgentManagerOptions::default()
    });
    let started = tokio::time::Instant::now();
    let listed = manager.list_importable_sessions(None).await;
    assert_eq!(started.elapsed(), Duration::from_secs(90));
    assert_eq!(
        listed
            .sessions
            .iter()
            .map(|managed| managed.session.provider_handle_id.as_str())
            .collect::<Vec<_>>(),
        ["last-moment"]
    );
    assert_eq!(
        listed.provider_errors,
        [ImportableSessionProviderError {
            provider: "hung".to_owned(),
            message: "Timed out listing importable sessions for provider 'hung' after 90000ms"
                .to_owned(),
        }]
    );
    assert_eq!(
        stringify(&JsValue::Array(warns.lock().expect("warns").clone())),
        r#"[[{"module":"agent","component":"agent-manager","err":{"type":"Error","message":"Timed out listing importable sessions for provider 'hung' after 90000ms"},"provider":"hung"},"Failed to list importable sessions for provider"]]"#
    );
}
