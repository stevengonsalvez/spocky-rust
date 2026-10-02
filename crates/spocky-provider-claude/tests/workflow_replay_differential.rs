//! Differential check of `parseClaudeWorkflowRun` and
//! `observeReplayWorkflows` against the pinned build. The fixtures are those
//! of the baseline `workflow-replay-source.test.ts`, plus the Run ID
//! pattern, the status mapping, repeated results, epoch and ISO timestamps,
//! and the timestamp sort. Runs under two time zones because offset-less
//! timestamps are local time. `convertEntry` is a stub returning the case's
//! items for every entry. Runs reach both sides through
//! `parseClaudeWorkflowRun`.

mod support;

use std::collections::HashMap;

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_provider_claude::subagents::observation::{
    SubagentObservation, fold_subagent_observations,
};
use spocky_provider_claude::subagents::workflow_replay_source::{
    ClaudeWorkflowRun, observe_replay_workflows, parse_claude_workflow_run,
};

const CHILD_ENV: &str = "SPOCKY_WORKFLOW_REPLAY_CHILD";
const RESULT_PREFIX: &str = "RESULT ";
const ZONES: [&str; 2] = ["UTC", "Europe/London"];

const TOOL: &str = "toolu_01XskpjeASyuFyXC5qsLHYps";
const RUN: &str = "wf_4a0af4f7-f56";

const NODE_SCRIPT: &str = r#"
const [dist, casesJson] = process.argv.slice(1);
const base = `${dist}/server/agent/providers/claude/subagents`;
const { observeReplayWorkflows, parseClaudeWorkflowRun } = await import(`${base}/workflow-replay-source.js`);
const { foldSubagentObservations } = await import(`${base}/observation.js`);
const KEYS = ["kind", "id", "title", "description", "toolCallId", "parentSubagentId", "status", "subtitle", "item", "timestamp"];
const canon = (observation) => {
  const out = {};
  for (const key of KEYS) if (observation[key] !== undefined) out[key] = observation[key];
  return out;
};
const lines = [];
const put = (value) => lines.push(`RESULT ${value === undefined ? "undefined" : JSON.stringify(value)}`);
for (const testCase of JSON.parse(casesJson)) {
  if (testCase.parseRun !== undefined) {
    put(parseClaudeWorkflowRun(testCase.parseRun) ?? null);
    continue;
  }
  try {
    const items = testCase.convert ?? [];
    const workflows = testCase.workflows.map((workflow) => parseClaudeWorkflowRun(JSON.stringify(workflow)));
    const entriesByRunId = new Map(Object.entries(testCase.entriesByRunId ?? {}));
    const observations = observeReplayWorkflows({
      workflows,
      parentEntries: testCase.parentEntries,
      entriesByRunId,
      convertEntry: () => items,
    });
    put(observations.map(canon));
    put(foldSubagentObservations(observations));
  } catch (error) {
    put({ threw: error instanceof Error ? error.message : String(error) });
  }
}
process.stdout.write(lines.join("\n") + "\n");
"#;

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/agent/providers/claude/subagents/workflow-replay-source.js",
        "26b4530c767a3bcb4eb77b92bda381b8822041eff5354255a6783d61f83a8872",
    ),
    (
        "server/agent/providers/claude/subagents/observation.js",
        "7b17147b5c5e222b5e29326ec6fda33a69f66da86d827b1cad59b5a68dc65176",
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
        "server/agent/provider-history-timestamps.js",
        "2b9749871c429dc99fa9062f891dfefd1034a8cb5e9d9cbf71c52797cd9cf7c5",
    ),
];

/// The parent transcript of the baseline tests: a Workflow call and its
/// result with the given content (JSON text).
fn parent_entries(result_content: &str) -> String {
    format!(
        r#"[{{"message":{{"content":[{{"type":"tool_use","id":"{TOOL}","name":"Workflow","input":{{}}}}]}}}},{{"message":{{"content":[{{"type":"tool_result","tool_use_id":"{TOOL}","content":{result_content}}}]}}}}]"#
    )
}

fn launched() -> String {
    stringify(&JsValue::String(format!(
        "Workflow launched in background.\nRun ID: {RUN}"
    )))
}

fn observe(workflows: &str, parent: &str, extra: &str) -> String {
    format!(r#"{{"workflows":{workflows},"parentEntries":{parent}{extra}}}"#)
}

fn parse_run(contents: &str) -> String {
    format!(
        r#"{{"parseRun":{}}}"#,
        stringify(&JsValue::String(contents.to_owned()))
    )
}

#[allow(clippy::too_many_lines)] // A list of fixtures.
fn cases() -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    for contents in [
        r#"{"runId":"wf_4a0af4f7-f56","timestamp":"2026-08-06T08:04:46.347Z","summary":"Runs one deterministic child and returns its structured result","workflowName":"paseo-workflow-one-child","status":"completed","startTime":1786003484150,"defaultModel":"claude-sonnet-5","totalTokens":20417,"workflowProgress":[{"type":"workflow_phase","title":"Inspect"}]}"#,
        r#"{"runId":"wf_1","result":{"report":"R"},"script":"SECRET WORKFLOW SOURCE","workflowProgress":[{"promptPreview":"SECRET CHILD PROMPT"}]}"#,
        "",
        "not json",
        "[]",
        r#"{"status":"completed"}"#,
        r#"{"runId":5}"#,
        r#"{"runId":"wf_1","timestamp":5,"summary":null,"startTime":"x","totalTokens":"1","status":false}"#,
        r#"{"runId":"wf_1","result":null}"#,
        r#"{"runId":"wf_1","result":0}"#,
        r#"{"runId":""}"#,
        "null",
        "true",
    ] {
        all.push(parse_run(contents));
    }

    let completed = format!(
        r#"[{{"runId":"{RUN}","summary":"Runs one deterministic child and returns its structured result","status":"completed","startTime":1786003484150,"defaultModel":"claude-sonnet-5","totalTokens":20417}}]"#
    );
    all.push(observe(&completed, &parent_entries(&launched()), ""));
    all.push(observe(
        &format!(
            r#"[{{"runId":"{RUN}","summary":"Structured result workflow","status":"completed"}}]"#
        ),
        &parent_entries(&format!(r#"[{{"type":"text","text":{}}}]"#, launched())),
        "",
    ));
    all.push(observe(
        &format!(
            r#"[{{"runId":"{RUN}","summary":"Inspect Paseo","status":"completed","result":{{"report":"What Paseo is\nA local-first coding-agent environment."}},"script":"SECRET WORKFLOW SOURCE","workflowProgress":[{{"promptPreview":"SECRET CHILD PROMPT"}}]}}]"#
        ),
        &parent_entries(&launched()),
        "",
    ));
    let report = "What Paseo is\nA local-first coding-agent environment.";
    all.push(observe(
        &format!(
            r#"[{{"runId":"{RUN}","summary":"Inspect Paseo","status":"completed","result":{{"report":{}}}}}]"#,
            stringify(&JsValue::String(report.to_owned()))
        ),
        &parent_entries(&launched()),
        &format!(
            r#","entriesByRunId":{{"{RUN}":[{{}}]}},"convert":[{{"type":"assistant_message","text":{}}}]"#,
            stringify(&JsValue::String(report.to_owned()))
        ),
    ));
    all.push(observe(
        &format!(r#"[{{"runId":"{RUN}","summary":"Interrupted workflow","status":"running"}}]"#),
        &parent_entries(&launched()),
        "",
    ));
    all.push(observe(
        &format!(r#"[{{"runId":"{RUN}","status":"completed"}}]"#),
        "[]",
        "",
    ));

    // Run ID pattern: anchors, whitespace, charset, content shapes.
    for content in [
        r#""Run ID: wf_abc-123""#,
        r#""Run ID:wf_abc""#,
        r#""Run ID:   wf_abc""#,
        r#""Run ID:\twf_abc""#,
        r#""Run ID:\nwf_abc""#,
        r#""prefix Run ID: wf_abc""#,
        r#""line one\nRun ID: wf_abc\nline three""#,
        r#""\nRun ID: wf_abc""#,
        r#""Run ID: wf_abc_def""#,
        r#""Run ID: wf_abc.def""#,
        r#""Run ID: wf_""#,
        r#""Run ID: WF_abc""#,
        r#""run id: wf_abc""#,
        r#""Run ID: wf_ABC-xyz-9 extra""#,
        r#""Run ID: wf_first\nRun ID: wf_second""#,
        r#"[{"type":"text","text":"Run ID: wf_abc"}]"#,
        r#"[{"type":"text","text":"a"},{"type":"text","text":"Run ID: wf_abc"}]"#,
        r#"[{"type":"text","text":"Run ID:"},{"type":"text","text":"wf_abc"}]"#,
        r#"[{"type":"image","text":"Run ID: wf_abc"},{"type":"text","text":5}]"#,
        r#"[{"type":"text","text":""}]"#,
        r"[]",
        r#"{"type":"text","text":"Run ID: wf_abc"}"#,
        "null",
        "5",
    ] {
        all.push(observe(
            r#"[{"runId":"wf_abc","summary":"S","status":"completed"},{"runId":"wf_first","status":"completed"},{"runId":"wf_second","status":"failed"},{"runId":"wf_abc-123","status":"completed"},{"runId":"wf_ABC-xyz-9","status":"completed"}]"#,
            &parent_entries(content),
            "",
        ));
    }
    // Calls that are not Workflow, missing ids, repeated results.
    all.push(observe(
        r#"[{"runId":"wf_a","status":"completed"},{"runId":"wf_b","status":"completed"}]"#,
        r#"[{"message":{"content":[{"type":"tool_use","id":"t1","name":"Workflow"},{"type":"tool_use","id":"t2","name":"Task"},{"type":"tool_use","id":" ","name":"Workflow"},{"type":"tool_use","name":"Workflow"},5,null,{"type":"tool_use","id":"t3","name":"Workflow"}]}},{"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Run ID: wf_a"},{"type":"tool_result","tool_use_id":"t2","content":"Run ID: wf_b"},{"type":"tool_result","tool_use_id":"t3","content":"Run ID: wf_a"},{"type":"tool_result","content":"Run ID: wf_b"}]}},{"message":{"content":"text"}},{"message":null},{}]"#,
        "",
    ));

    // Status mapping.
    for status in [
        r#""completed""#,
        r#""  COMPLETED  ""#,
        r#""failed""#,
        r#""error""#,
        r#""canceled""#,
        r#""cancelled""#,
        r#""killed""#,
        r#""stopped""#,
        r#""running""#,
        r#""pending""#,
        r#""""#,
        r#""İ""#,
        "null",
        "5",
    ] {
        all.push(observe(
            &format!(r#"[{{"runId":"{RUN}","status":{status}}}]"#),
            &parent_entries(&launched()),
            "",
        ));
    }

    // Timestamps: start time epochs and the finished-at ISO text.
    for (start, finished) in [
        ("1786003484150", r#""2026-08-06T08:04:46.347Z""#),
        ("0", r#""2026-08-06T08:04:46""#),
        ("-1", r#""2026-08-06 08:04:46""#),
        ("1700000000", r#""Aug 6 2026 08:04""#),
        ("1e15", r#""2026-08-06T08:04:46+01:00""#),
        ("8.64e15", r#""not a date""#),
        ("8.64e15", r#""+275760-09-13T00:00:00.000Z""#),
        ("8.640000000000001e15", r#""""#),
        ("1.5", r#""  2026-08-06T08:04:46Z  ""#),
        ("null", r#""2026-03-29T01:30:00""#),
        ("null", r#""2026-10-25T01:30:00""#),
        ("null", "5"),
    ] {
        all.push(observe(
            &format!(
                r#"[{{"runId":"{RUN}","summary":"S","status":"completed","startTime":{start},"timestamp":{finished},"defaultModel":"claude-opus-5","totalTokens":1234}}]"#
            ),
            &parent_entries(&launched()),
            "",
        ));
    }

    // Subtitle inputs.
    for (model, tokens) in [
        (r#""claude-opus-5""#, "20417"),
        (r#""   ""#, "5"),
        ("null", "0"),
        (r#""glm-5.1""#, "999.5"),
        (r#""claude-sonnet-5""#, "0.49999999999999994"),
        ("5", "-3"),
        (r#""claude-haiku-4-5-20251001""#, "null"),
    ] {
        all.push(observe(
            &format!(
                r#"[{{"runId":"{RUN}","status":"completed","defaultModel":{model},"totalTokens":{tokens}}}]"#
            ),
            &parent_entries(&launched()),
            "",
        ));
    }

    // Description sources and the timeline: summary, workflowName, blanks.
    for (summary, name) in [
        (r#""  Summary  ""#, r#""Name""#),
        (r#""   ""#, r#""  Name  ""#),
        ("null", r#""Name""#),
        (r#""""#, r#""""#),
    ] {
        all.push(observe(
            &format!(
                r#"[{{"runId":"{RUN}","status":"completed","summary":{summary},"workflowName":{name}}}]"#
            ),
            &parent_entries(&launched()),
            "",
        ));
    }

    // Child entries: sorted by timestamp (ISO, legacy, missing, equal),
    // converted, and the result de-duplicated against assistant text.
    let entries = r#"[{"id":1,"timestamp":"2026-08-06T08:00:03.000Z"},{"id":2,"timestamp":"2026-08-06T08:00:01.000Z"},{"id":3},{"id":4,"timestamp":"2026-08-06T08:00:01.000Z"},{"id":5,"timestamp":"2026-08-06 08:00:02"},{"id":6,"timestamp":1786003484},{"id":7,"timestamp":"junk"},{"id":8,"timestamp":"2026-08-06T08:00:02.000Z"},{"id":9,"timestamp":"Aug 6 2026 09:00"},{"id":10,"timestamp":"2026-08-06T08:00:02"}]"#;
    for convert in [
        r#"[{"type":"user_message","text":"u"}]"#,
        r#"[{"type":"assistant_message","text":"  The report  "}]"#,
        r#"[{"type":"assistant_message","text":"other"},{"type":"reasoning","text":"r"}]"#,
        "[]",
    ] {
        all.push(observe(
            &format!(
                r#"[{{"runId":"{RUN}","summary":"S","status":"completed","timestamp":"2026-08-06T09:30:00Z","result":{{"report":"The report"}}}}]"#
            ),
            &parent_entries(&launched()),
            &format!(r#","entriesByRunId":{{"{RUN}":{entries}}},"convert":{convert}"#),
        ));
    }
    // Result shapes through the formatter.
    for result in [
        r#""plain text""#,
        r#"{"a":{"b":"  text  "}}"#,
        r#"{"x":null}"#,
        "12.5",
        r#""   ""#,
        "[1,2]",
        "{}",
        "false",
    ] {
        all.push(observe(
            &format!(r#"[{{"runId":"{RUN}","status":"completed","result":{result}}}]"#),
            &parent_entries(&launched()),
            "",
        ));
    }
    // Several workflows, in order, with a duplicate run id.
    all.push(observe(
        r#"[{"runId":"wf_a","summary":"A","status":"completed"},{"runId":"wf_a","summary":"A again","status":"failed"},{"runId":"wf_b","summary":"B","status":"killed"},{"runId":"wf_unlinked","status":"completed"}]"#,
        r#"[{"message":{"content":[{"type":"tool_use","id":"t1","name":"Workflow"},{"type":"tool_use","id":"t2","name":"Workflow"}]}},{"message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Run ID: wf_a"},{"type":"tool_result","tool_use_id":"t2","content":"Run ID: wf_b"}]}}]"#,
        "",
    ));
    all
}

fn text_of(value: Option<&String>) -> JsValue {
    value.map_or(JsValue::Undefined, |text| JsValue::String(text.clone()))
}

fn number_of(value: Option<f64>) -> JsValue {
    value.map_or(JsValue::Undefined, JsValue::Number)
}

fn run_json(run: &ClaudeWorkflowRun) -> JsValue {
    let mut object = JsObject::new();
    object.insert("runId", JsValue::String(run.run_id.clone()));
    object.insert("timestamp", text_of(run.timestamp.as_ref()));
    object.insert("summary", text_of(run.summary.as_ref()));
    object.insert("workflowName", text_of(run.workflow_name.as_ref()));
    object.insert("status", text_of(run.status.as_ref()));
    object.insert("startTime", number_of(run.start_time));
    object.insert("defaultModel", text_of(run.default_model.as_ref()));
    object.insert("totalTokens", number_of(run.total_tokens));
    object.insert("result", run.result.clone().unwrap_or(JsValue::Undefined));
    JsValue::Object(object)
}

/// One observation with the keys in the order the script's `canon` uses.
fn canon(observation: &SubagentObservation) -> JsValue {
    let mut object = JsObject::new();
    let string = |text: &str| JsValue::String(text.to_owned());
    match observation {
        SubagentObservation::Declared {
            id,
            title,
            description,
            tool_call_id,
            parent_subagent_id,
            timestamp,
        } => {
            object.insert("kind", string("declared"));
            object.insert("id", string(id));
            object.insert("title", text_of(title.as_ref()));
            object.insert("description", text_of(description.as_ref()));
            object.insert("toolCallId", text_of(tool_call_id.as_ref()));
            object.insert("parentSubagentId", text_of(parent_subagent_id.as_ref()));
            object.insert("timestamp", text_of(timestamp.as_ref()));
        }
        SubagentObservation::Status {
            id,
            status,
            timestamp,
        } => {
            object.insert("kind", string("status"));
            object.insert("id", string(id));
            object.insert("status", string(status.as_str()));
            object.insert("timestamp", text_of(timestamp.as_ref()));
        }
        SubagentObservation::Subtitle {
            id,
            subtitle,
            timestamp,
        } => {
            object.insert("kind", string("subtitle"));
            object.insert("id", string(id));
            object.insert("subtitle", string(subtitle));
            object.insert("timestamp", text_of(timestamp.as_ref()));
        }
        SubagentObservation::Timeline {
            id,
            item,
            timestamp,
        } => {
            object.insert("kind", string("timeline"));
            object.insert("id", string(id));
            object.insert("item", item.clone());
            object.insert("timestamp", text_of(timestamp.as_ref()));
        }
    }
    JsValue::Object(object)
}

fn result_line(value: &JsValue) -> String {
    format!("{RESULT_PREFIX}{}", stringify(value))
}

#[allow(clippy::too_many_lines)] // A list of fixtures.
fn rust_lines(cases_json: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for test_case in parse(cases_json)
        .expect("cases JSON")
        .as_array()
        .expect("cases array")
    {
        if let Some(contents) = test_case.get("parseRun").and_then(JsValue::as_str) {
            lines.push(result_line(
                &parse_claude_workflow_run(contents)
                    .as_ref()
                    .map_or(JsValue::Null, run_json),
            ));
            continue;
        }
        let items: Vec<JsValue> = test_case
            .get("convert")
            .and_then(JsValue::as_array)
            .map(<[JsValue]>::to_vec)
            .unwrap_or_default();
        let workflows: Vec<ClaudeWorkflowRun> = test_case
            .get("workflows")
            .and_then(JsValue::as_array)
            .unwrap_or_default()
            .iter()
            .map(|workflow| {
                parse_claude_workflow_run(&stringify(workflow)).expect("fixture workflow parses")
            })
            .collect();
        let parent_entries: Vec<JsObject> = test_case
            .get("parentEntries")
            .and_then(JsValue::as_array)
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| entry.as_object().cloned())
            .collect();
        let entries_by_run_id: HashMap<String, Vec<JsObject>> = test_case
            .get("entriesByRunId")
            .and_then(JsValue::as_object)
            .map(|object| {
                object
                    .iter()
                    .map(|(run, entries)| {
                        (
                            run.to_owned(),
                            entries
                                .as_array()
                                .unwrap_or_default()
                                .iter()
                                .filter_map(|entry| entry.as_object().cloned())
                                .collect(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut convert = |_: &JsObject| Ok(items.clone());
        let observations = observe_replay_workflows(
            &workflows,
            &parent_entries,
            &entries_by_run_id,
            &mut convert,
        )
        .expect("replay");
        lines.push(result_line(&JsValue::Array(
            observations.iter().map(canon).collect(),
        )));
        lines.push(result_line(&JsValue::Array(fold_subagent_observations(
            &observations,
        ))));
    }
    lines
}

/// The child: prints the Rust side's result lines under the inherited `TZ`.
#[test]
fn child_prints_workflow_replay_results() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let cases_json = format!("[{}]", cases().join(","));
    println!();
    for line in rust_lines(&cases_json) {
        println!("{line}");
    }
}

#[test]
fn workflow_replay_matches_the_pinned_build() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let cases_json = format!("[{}]", cases().join(","));
    for zone in ZONES {
        let expected: Vec<String> = support::run_node_with_env(
            &node,
            &dist,
            NODE_SCRIPT,
            std::slice::from_ref(&cases_json),
            &[("TZ", zone)],
        )
        .lines()
        .map(str::to_owned)
        .collect();
        let actual = support::run_self_child(
            "child_prints_workflow_replay_results",
            CHILD_ENV,
            zone,
            RESULT_PREFIX,
        );
        assert_eq!(actual.len(), expected.len(), "{zone}: line count");
        for (index, (rust, node_line)) in actual.iter().zip(&expected).enumerate() {
            assert_eq!(rust, node_line, "{zone}: line {index}");
        }
    }
}
