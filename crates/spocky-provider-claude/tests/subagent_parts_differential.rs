//! Differential check of the task state, task notification mapping,
//! subagent subtitle, workflow result, live task protocol source, and
//! replay timestamp helpers against the pinned build. Every operation's
//! output must be the same `JSON.stringify` text in both.

mod support;

use spocky_contracts::js_value::{JsValue, parse, stringify};
use spocky_provider_claude::subagents::live_source::ClaudeTaskProtocolSource;
use spocky_provider_claude::subagents::observation::fold_subagent_observations;
use spocky_provider_claude::subagents::presentation::{
    PresentationFacts, build_claude_subagent_subtitle,
};
use spocky_provider_claude::subagents::workflow_output::{
    format_claude_workflow_result, parse_claude_workflow_result,
};
use spocky_provider_claude::task_notification::{
    map_system_record_to_tool_call, map_user_content_to_tool_call,
    read_tool_use_id_from_history_record,
};
use spocky_provider_claude::task_state::ClaudeTaskState;
use spocky_provider_claude::timestamps::normalize_replay_timestamp;

/// One operation per entry; see `NODE_SCRIPT` for their meaning.
const OPS: &str = r#"[
 {"op":"taskState","seq":[
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":"create-1","name":"TaskCreate","input":{"subject":"Alpha","activeForm":"Doing alpha"}}]}},
  {"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"create-1","content":"ok"}]},"toolUseResult":{"task":{"id":"1","subject":"Alpha"}}},
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":"update-1","name":"TaskUpdate","input":{"taskId":"1","status":"in_progress"}}]}},
  {"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"update-1","content":"ok"}]},"toolUseResult":{"success":true,"taskId":"1"}}
 ]},
 {"op":"taskState","seq":[
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":"create","name":"TaskCreate","input":{"subject":"Disposable"}}]}},
  {"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"create","content":"ok"}]},"toolUseResult":{"task":{"id":"1","subject":"Disposable"}}},
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":"delete","name":"TaskUpdate","input":{"taskId":"1","status":"deleted"}}]}},
  {"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"delete","content":"ok"}]},"toolUseResult":{"success":true,"taskId":"1"}},
  {"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"delete","content":"ok"}]},"toolUseResult":{"success":true,"taskId":"1"}}
 ]},
 {"op":"taskState","seq":[
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":"create","name":"TaskCreate","input":{"subject":"Original"}}]}},
  {"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"create","content":"ok"}]},"toolUseResult":{"task":{"id":"1","subject":"Original"}}},
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":"complete","name":"TaskUpdate","input":{"taskId":"1","status":"completed"}}]}},
  {"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"complete","content":"ok"}]},"toolUseResult":{"success":true,"taskId":"1"}},
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":"rename","name":"TaskUpdate","input":{"taskId":"1","subject":"Renamed"}}]}},
  {"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"rename","content":"ok"}]},"toolUseResult":{"success":true,"taskId":"1"}}
 ]},
 {"op":"taskState","seq":[
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":"legacy","name":"TodoWrite","input":{"todos":[{"content":"Legacy","status":"in_progress","activeForm":"Working"}]}}]}},
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":"list","name":"TaskList","input":{}}]}},
  {"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"list","content":"ok"}]},"toolUseResult":{"tasks":[{"id":"7","subject":"Current","status":"completed","activeForm":"Finishing"}]}}
 ]},
 {"op":"taskState","seq":[
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":"first","name":"TodoWrite","input":{"todos":[{"content":"Stable","status":"pending"}]}}]}},
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":"second","name":"TodoWrite","input":{"todos":[{"content":"Stable","status":"completed"}]}}]}},
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":"subagent","name":"Task","input":{"description":"delegate"}}]}}
 ]},
 {"op":"taskState","seq":[
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":" t1 ","name":" TodoWrite ","input":{"todos":[{"id":"x","text":" a ","status":"deleted"},{"subject":"b","id":"x","active_form":"B"},{"text":"c","completed":true},7,{"content":""},{"taskId":"t","content":"d","status":"bogus"}]}},{"type":"tool_use","id":"t2","name":"TaskCreate","input":[1]}]}},
  {"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t2"}]},"tool_use_result":{"taskId":"9"}},
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":"c2","name":"TaskCreate","input":{"subject":"new"}}]}},
  {"type":"user","message":{"content":[{"type":"text"},{"type":"tool_result","tool_use_id":"c2"}]},"toolUseResult":{"taskId":"9","success":false}},
  {"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"c2"}]},"toolUseResult":{"taskId":"9"}},
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":"u2","name":"TaskUpdate","input":{"taskId":"9","status":null,"activeForm":" go "}}]}},
  {"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"u2"}]},"toolUseResult":{"statusChange":{"to":"in_progress"}}},
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":"u3","name":"TaskUpdate","input":{"taskId":"9"}}]}},
  {"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"u3"}]},"toolUseResult":{"statusChange":{"to":null}}},
  {"type":"assistant","message":{"content":[{"type":"tool_use","id":"l2","name":"TaskList","input":{}}]}},
  {"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"l2"}]},"toolUseResult":{"tasks":"no"}}
 ]},
 {"op":"notifUser","content":"<task-notification>\n<task-id>abc</task-id>\n<tool-use-id> toolu_1 </tool-use-id>\n<status>Completed</status>\n<summary>Ran tests</summary>\n<output-file>/tmp/o</output-file>\n</task-notification>","messageId":"m 1/x"},
 {"op":"notifUser","content":[{"type":"text","text":"  <task-notification><STATUS>failed</status><tool_use_id>t</tool_use_id><output_file>f</output_file></task-notification>"},{"input":" more "}],"messageId":null},
 {"op":"notifUser","content":[{"text":"<task-notification><status>cancelled</status></task-notification>"},{"text":5}],"messageId":""},
 {"op":"notifUser","content":"<task-notification><status>error</status></task-notification>"},
 {"op":"notifUser","content":"plain"},
 {"op":"notifUser","content":"<task-notification>nothing</task-notification>"},
 {"op":"notifSystem","record":{"type":"system","subtype":"task_notification","uuid":"u-1","task_id":"t-1","status":"completed","summary":"Done","output_file":"/o"}},
 {"op":"notifSystem","record":{"type":"system","subtype":"task_notification","task_id":"  ","content":"<task-notification><task-id>from tag</task-id><status>stopped</status></task-notification>"}},
 {"op":"notifSystem","record":{"type":"queue-operation","content":"<task-notification><summary>queued</summary></task-notification>","message_id":"q"}},
 {"op":"notifSystem","record":{"type":"queue-operation","content":"no marker"}},
 {"op":"notifSystem","record":{"type":"system","subtype":"task_notification","status":5}},
 {"op":"notifSystem","record":{"type":"system","subtype":"task_notification","message":[]}},
 {"op":"notifToolUseId","record":{"type":"user","uuid":"u","message":{"content":"<task-notification><tool-use-id>tid</tool-use-id></task-notification>"}}},
 {"op":"notifToolUseId","record":{"type":"system","subtype":"task_notification","tool_use_id":"direct"}},
 {"op":"notifToolUseId","record":{"type":"user","message":{"content":"plain"}}},
 {"op":"subtitle","facts":{"title":" Explore ","model":"claude-opus-4-8-20260101","effort":"xhigh","usage":{"totalTokens":43210}}},
 {"op":"subtitle","facts":{"model":"glm-4.6","effort":"very_high-max low","usage":{"totalTokens":999.5}}},
 {"op":"subtitle","facts":{"effort":"ßeta","usage":{"totalTokens":0}}},
 {"op":"subtitle","facts":{}},
 {"op":"workflowFormat","result":{"a":{"b":"  text  "}}},
 {"op":"workflowFormat","result":{"a":1,"b":[true,null]}},
 {"op":"workflowFormat","result":12.5},
 {"op":"workflowFormat","result":{"x":null}},
 {"op":"workflowFormat","result":"   "},
 {"op":"workflowParse","contents":"{\"result\":{\"summary\":\"ok\"}}"},
 {"op":"workflowParse","contents":"{\"other\":1}"},
 {"op":"workflowParse","contents":"not json"},
 {"op":"live","toolInputs":{"toolu_task":{"name":"  Named  ","subagent_type":"Explore"}},"workflowResults":{"/out/wf":"Workflow done"},"steps":[
  {"call":"observe","message":{"type":"system","subtype":"task_started","task_id":"bash1","tool_use_id":"toolu_bash","task_type":"local_bash"}},
  {"call":"query"},
  {"call":"observe","message":{"type":"system","subtype":"task_started","task_id":"t1","tool_use_id":"toolu_task","task_type":"local_agent","subagent_type":"Explore","description":"Find the bug","prompt":"Look at src"}},
  {"call":"observe","message":{"type":"system","subtype":"task_started","task_id":"t2","tool_use_id":"toolu_skip","subagent_type":"Explore","skip_transcript":true}},
  {"call":"observe","message":{"type":"system","subtype":"task_started","task_id":"t3","tool_use_id":"toolu_legacy","subagent_type":"general-purpose","description":"Legacy"}},
  {"call":"observe","message":{"type":"system","subtype":"task_started","task_id":"wf1","tool_use_id":"toolu_wf","task_type":"local_workflow","description":"Run workflow","prompt":"source code"}},
  {"call":"frame","id":"toolu_task","message":{"type":"assistant","message":{"model":"claude-sonnet-4-6","content":[{"type":"tool_use","id":"toolu_child_bash","name":"Bash"}]}}},
  {"call":"observe","message":{"type":"system","subtype":"task_started","task_id":"child_bash","tool_use_id":"toolu_child_bash","task_type":"local_bash"}},
  {"call":"query"},
  {"call":"hook","input":{"agent_id":"t1","effort":{"level":"high"}}},
  {"call":"hook","input":{"agent_id":"t1","effort":{"level":"high"}}},
  {"call":"observe","message":{"type":"system","subtype":"task_progress","task_id":"t1","usage":{"total_tokens":1500}}},
  {"call":"observe","message":{"type":"system","subtype":"task_updated","task_id":"t3","patch":{"is_backgrounded":true,"status":"running"}}},
  {"call":"observe","message":{"type":"system","subtype":"task_started","task_id":"t1","tool_use_id":"toolu_task_again","task_type":"local_agent","prompt":"Resume"}},
  {"call":"query"},
  {"call":"cancel"},
  {"call":"observe","message":{"type":"system","subtype":"task_notification","task_id":"t1","status":"completed","usage":{"total_tokens":2000}}},
  {"call":"observe","message":{"type":"system","subtype":"task_notification","task_id":"wf1","status":"completed","output_file":"/out/wf"}},
  {"call":"observe","message":{"type":"system","subtype":"task_notification","task_id":"wf1","status":"completed","output_file":"/out/wf"}},
  {"call":"observe","message":{"type":"system","subtype":"task_notification","task_id":"bash1","status":"completed"}},
  {"call":"observe","message":{"type":"system","subtype":"task_updated","task_id":"t3","patch":{"status":"killed"}}},
  {"call":"observe","message":{"type":"system","subtype":"task_started","task_id":"t1","tool_use_id":"toolu_task_third","task_type":"local_agent"}},
  {"call":"fail"},
  {"call":"observe","message":{"type":"user","subtype":"task_started"}},
  {"call":"query"}
 ]},
 {"op":"timestamp","value":"2026-10-01T10:00:00Z"},
 {"op":"timestamp","value":"2026-13-01T10:00:00Z"},
 {"op":"timestamp","value":1700000000},
 {"op":"timestamp","value":true}
]"#;

/// Ids each live `query` step reports on.
const QUERY_IDS: [&str; 6] = [
    "toolu_task",
    "toolu_task_again",
    "toolu_skip",
    "toolu_wf",
    "toolu_child_bash",
    "missing",
];

const NODE_SCRIPT: &str = r#"
const [dist, opsJson, queryJson] = process.argv.slice(1);
const base = `${dist}/server/agent`;
const { ClaudeTaskState } = await import(`${base}/providers/claude/task-state.js`);
const notif = await import(`${base}/providers/claude/task-notification-tool-call.js`);
const { buildClaudeSubagentSubtitle } = await import(`${base}/providers/claude/subagents/presentation.js`);
const workflow = await import(`${base}/providers/claude/subagents/workflow-output.js`);
const { ClaudeTaskProtocolSource } = await import(`${base}/providers/claude/subagents/live-source.js`);
const { foldSubagentObservations } = await import(`${base}/providers/claude/subagents/observation.js`);
const { normalizeProviderReplayTimestamp } = await import(`${base}/provider-history-timestamps.js`);
const queryIds = JSON.parse(queryJson);
const out = [];
const put = (value) => out.push(value === undefined ? "undefined" : JSON.stringify(value));
for (const op of JSON.parse(opsJson)) {
  switch (op.op) {
    case "taskState": {
      const state = new ClaudeTaskState();
      for (const message of op.seq) put(state.observe(message));
      break;
    }
    case "notifUser":
      put(notif.mapTaskNotificationUserContentToToolCall({ content: op.content, messageId: op.messageId }));
      break;
    case "notifSystem":
      put(notif.mapTaskNotificationSystemRecordToToolCall(op.record));
      break;
    case "notifToolUseId":
      put(notif.readTaskNotificationToolUseIdFromHistoryRecord(op.record));
      break;
    case "subtitle":
      put(buildClaudeSubagentSubtitle(op.facts));
      break;
    case "workflowFormat":
      put(workflow.formatClaudeWorkflowResult(op.result));
      break;
    case "workflowParse":
      put(workflow.parseClaudeWorkflowResult(op.contents));
      break;
    case "live": {
      const source = new ClaudeTaskProtocolSource({
        getToolInput: (id) => op.toolInputs[id] ?? null,
        readWorkflowResult: (file) => op.workflowResults[file],
      });
      for (const step of op.steps) {
        switch (step.call) {
          case "observe": put(foldSubagentObservations(source.observe(step.message))); break;
          case "hook": put(foldSubagentObservations(source.observeHook(step.input))); break;
          case "frame": put(foldSubagentObservations(source.observeSidechainFrame(step.message, step.id))); break;
          case "cancel": put(foldSubagentObservations(source.cancelRunningForegroundTasks())); break;
          case "fail": put(foldSubagentObservations(source.failRunningTasks())); break;
          case "query":
            put([source.isActive, source.announcesTasks,
              ...queryIds.map((id) => [source.isDeclared(id), source.resolveSubagentId(id) ?? null,
                source.needsSyntheticParentToolCard(id)]),
              ["bash1", "t1", "child_bash", "wf1", "nope"].map((task) =>
                [source.isDeclaredTask(task), source.resolveTaskOwner(task, "toolu_child_bash") ?? null,
                 source.resolveTaskOwner(task) ?? null])]);
            break;
        }
      }
      break;
    }
    case "timestamp":
      put(normalizeProviderReplayTimestamp(op.value));
      break;
  }
}
process.stdout.write(out.join("\n") + "\n");
"#;

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/agent/providers/claude/task-state.js",
        "514e17f1f0ea7700a74f7ed52be65cf0370195a8b1bc9860f0fcf715ef424433",
    ),
    (
        "server/agent/providers/claude/task-notification-tool-call.js",
        "a02dc023fcabf224f368619371a4df2e22784241bd8b719c49176bd648001e2a",
    ),
    (
        "server/agent/providers/claude/subagents/presentation.js",
        "cdf0316e3f8592a658567ce042841b1699c5a28621188da9ba6a0f040e66e3f3",
    ),
    (
        "server/agent/providers/claude/subagents/workflow-output.js",
        "ad0143ea0b6dbe7101548dab78780ed8530f89f6f60fbb944a937beedce2f3ee",
    ),
    (
        "server/agent/providers/claude/subagents/live-source.js",
        "e8d0fad4a66e32ceab38af2b54b84f9fe401c4f36160d8a6e7711275f4df2036",
    ),
    (
        "server/agent/providers/claude/subagents/observation.js",
        "7b17147b5c5e222b5e29326ec6fda33a69f66da86d827b1cad59b5a68dc65176",
    ),
    (
        "server/agent/provider-history-timestamps.js",
        "2b9749871c429dc99fa9062f891dfefd1034a8cb5e9d9cbf71c52797cd9cf7c5",
    ),
];

fn opt_string(value: Option<String>) -> JsValue {
    value.map_or(JsValue::Undefined, JsValue::String)
}

fn line(value: &JsValue) -> String {
    if matches!(value, JsValue::Undefined) {
        "undefined".to_owned()
    } else {
        stringify(value)
    }
}

fn get_str<'a>(op: &'a JsValue, key: &str) -> Option<&'a str> {
    op.get(key).and_then(JsValue::as_str)
}

fn facts(value: &JsValue) -> PresentationFacts {
    PresentationFacts {
        title: get_str(value, "title").map(str::to_owned),
        model: get_str(value, "model").map(str::to_owned),
        effort: get_str(value, "effort").map(str::to_owned),
        total_tokens: value
            .get("usage")
            .and_then(|usage| usage.get("totalTokens"))
            .and_then(JsValue::as_f64),
    }
}

fn folded(
    observations: &[spocky_provider_claude::subagents::observation::SubagentObservation],
) -> JsValue {
    JsValue::Array(fold_subagent_observations(observations))
}

fn live(op: &JsValue, out: &mut Vec<String>) {
    let tool_inputs = op.get("toolInputs").cloned().unwrap_or(JsValue::Null);
    let workflow_results = op.get("workflowResults").cloned().unwrap_or(JsValue::Null);
    let mut source = ClaudeTaskProtocolSource::new(
        Box::new(move |id| tool_inputs.get(id).and_then(JsValue::as_object).cloned()),
        Box::new(move |file| {
            workflow_results
                .get(file)
                .and_then(JsValue::as_str)
                .map(str::to_owned)
        }),
    );
    for step in op
        .get("steps")
        .and_then(JsValue::as_array)
        .unwrap_or_default()
    {
        let message = step.get("message").cloned().unwrap_or(JsValue::Undefined);
        let value = match get_str(step, "call") {
            Some("observe") => folded(&source.observe(&message)),
            Some("hook") => {
                folded(&source.observe_hook(step.get("input").unwrap_or(&JsValue::Null)))
            }
            Some("frame") => folded(
                &source.observe_sidechain_frame(&message, get_str(step, "id").unwrap_or_default()),
            ),
            Some("cancel") => folded(&source.cancel_running_foreground_tasks()),
            Some("fail") => folded(&source.fail_running_tasks()),
            _ => {
                let mut row = vec![
                    JsValue::Bool(source.is_active()),
                    JsValue::Bool(source.announces_tasks()),
                ];
                for id in QUERY_IDS {
                    row.push(JsValue::Array(vec![
                        JsValue::Bool(source.is_declared(id)),
                        source
                            .resolve_subagent_id(id)
                            .map_or(JsValue::Null, JsValue::String),
                        JsValue::Bool(source.needs_synthetic_parent_tool_card(id)),
                    ]));
                }
                let tasks = ["bash1", "t1", "child_bash", "wf1", "nope"]
                    .iter()
                    .map(|task| {
                        let task = JsValue::String((*task).to_owned());
                        JsValue::Array(vec![
                            JsValue::Bool(source.is_declared_task(Some(&task))),
                            source
                                .resolve_task_owner(Some(&task), Some("toolu_child_bash"))
                                .map_or(JsValue::Null, JsValue::String),
                            source
                                .resolve_task_owner(Some(&task), None)
                                .map_or(JsValue::Null, JsValue::String),
                        ])
                    })
                    .collect();
                row.push(JsValue::Array(tasks));
                JsValue::Array(row)
            }
        };
        out.push(line(&value));
    }
}

fn rust_output() -> String {
    let ops = parse(OPS).expect("ops JSON");
    let mut out = Vec::new();
    for op in ops.as_array().expect("ops array") {
        match get_str(op, "op") {
            Some("taskState") => {
                let mut state = ClaudeTaskState::default();
                for message in op
                    .get("seq")
                    .and_then(JsValue::as_array)
                    .unwrap_or_default()
                {
                    out.push(line(&state.observe(message).unwrap_or(JsValue::Null)));
                }
            }
            Some("notifUser") => out.push(line(
                &map_user_content_to_tool_call(op.get("content"), get_str(op, "messageId"))
                    .unwrap_or(JsValue::Null),
            )),
            Some("notifSystem") => out.push(line(
                &map_system_record_to_tool_call(op.get("record").unwrap_or(&JsValue::Null))
                    .unwrap_or(JsValue::Null),
            )),
            Some("notifToolUseId") => out.push(line(
                &read_tool_use_id_from_history_record(op.get("record").unwrap_or(&JsValue::Null))
                    .map_or(JsValue::Null, JsValue::String),
            )),
            Some("subtitle") => out.push(line(&opt_string(build_claude_subagent_subtitle(
                &facts(op.get("facts").unwrap_or(&JsValue::Null)),
            )))),
            Some("workflowFormat") => out.push(line(&opt_string(
                op.get("result").and_then(format_claude_workflow_result),
            ))),
            Some("workflowParse") => out.push(line(&opt_string(parse_claude_workflow_result(
                get_str(op, "contents").unwrap_or_default(),
            )))),
            Some("live") => live(op, &mut out),
            _ => out.push(line(
                &normalize_replay_timestamp(op.get("value")).map_or(JsValue::Null, JsValue::String),
            )),
        }
    }
    out.join("\n") + "\n"
}

#[test]
fn subagent_parts_match_the_pinned_build() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let query = stringify(&JsValue::Array(
        QUERY_IDS
            .iter()
            .map(|id| JsValue::String((*id).to_owned()))
            .collect(),
    ));
    let expected = support::run_node(
        &node,
        &dist,
        NODE_SCRIPT,
        &[stringify(&parse(OPS).expect("ops JSON")), query],
    );
    let actual = rust_output();
    for (index, (node_line, rust_line)) in expected.lines().zip(actual.lines()).enumerate() {
        assert_eq!(rust_line, node_line, "line {index} differs");
    }
    assert_eq!(actual, expected);
}
