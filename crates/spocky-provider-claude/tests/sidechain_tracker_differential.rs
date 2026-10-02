//! Differential check of `ClaudeSidechainTracker` against the pinned build:
//! every call's events (or thrown message) must be the same `JSON.stringify`
//! text in both. Covers the baseline test, the action and tool-result
//! extraction, the 200-entry trim, the 160-unit text cut, the descriptor
//! ownership switch, and the display summaries of every tool-call detail.

mod support;

use std::collections::HashMap;
use std::fmt::Write as _;

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_provider_claude::sidechain_tracker::{ClaudeSidechainTracker, TrackerContext};

const NODE_SCRIPT: &str = r#"
const [dist, scenariosJson] = process.argv.slice(1);
const base = `${dist}/server/agent`;
const { ClaudeSidechainTracker } = await import(`${base}/providers/claude/sidechain-tracker.js`);
const out = [];
const put = (value) => out.push(JSON.stringify(value));
for (const scenario of JSON.parse(scenariosJson)) {
  const inputs = { ...(scenario.toolInputs ?? {}) };
  const synthetic = scenario.synthetic ?? {};
  let owned = false;
  const tracker = new ClaudeSidechainTracker({
    getToolInput: (id) => inputs[id],
    isDescriptorOwnedElsewhere: () => owned,
    needsSyntheticParentToolCard: (id) => synthetic[id] ?? true,
  });
  for (const step of scenario.steps) {
    try {
      switch (step.call) {
        case "handle": put(tracker.handleMessage(step.message, step.parent)); break;
        case "finish": put(tracker.finish(step.id, step.status)); break;
        case "finishAll": put(tracker.finishAll(step.status)); break;
        case "clear": tracker.clear(); put(null); break;
        case "owned": owned = step.value; put(null); break;
        case "input": inputs[step.id] = step.value; put(null); break;
      }
    } catch (error) {
      put({ threw: error instanceof Error ? error.message : String(error) });
    }
  }
}
process.stdout.write(out.join("\n") + "\n");
"#;

/// Digests of every dist module the tracker runs, relative to the dist root.
const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/agent/providers/claude/sidechain-tracker.js",
        "6f2de8ccb0e52688caf8030c000c190e98829c37a8370d43bd639ab4e3a978b7",
    ),
    (
        "server/agent/providers/claude/tool-call-mapper.js",
        "ce7a4ce016271bc7637d8e226952b0ccee411352fcad9514efba5660339bfda1",
    ),
    (
        "server/agent/providers/claude/tool-call-detail-parser.js",
        "cef7f2fe967057dfac71c0b0012875e4496c1a4755c0ddf7c9618b6be6174534",
    ),
    (
        "../../../protocol/dist/tool-call-display.js",
        "107c62e7fcc69bb651739305e2617153944e1cff0fecba5e37a9445ea346f13f",
    ),
    (
        "../../../protocol/dist/tool-name-normalization.js",
        "7e2123e11ee9ddb683c3008ea1a9ee2794a120124a42f72537d552084c29b921",
    ),
];

fn assistant(content: &str) -> String {
    format!(
        r#"{{"type":"assistant","parent_tool_use_id":"task-1","message":{{"content":{content}}}}}"#
    )
}

fn handle(message: &str, parent: &str) -> String {
    format!(r#"{{"call":"handle","parent":"{parent}","message":{message}}}"#)
}

fn scenario(tool_inputs: &str, extra: &str, steps: &[String]) -> String {
    format!(
        r#"{{"toolInputs":{tool_inputs},{extra}"steps":[{}]}}"#,
        steps.join(",")
    )
}

/// One `tool_use` block per tool name and input.
fn tool_use(id: &str, name: &str, input: &str) -> String {
    format!(r#"{{"type":"tool_use","id":"{id}","name":"{name}","input":{input}}}"#)
}

#[allow(clippy::too_many_lines)] // A list of fixtures.
fn scenarios() -> Vec<String> {
    let named_input = r#"{"task-1":{"name":"repo_researcher","subagent_type":"Explore","description":"Inspect the repository"}}"#;
    let mut all = Vec::new();

    // The baseline test.
    all.push(scenario(
        named_input,
        "",
        &[handle(
            r#"{"type":"assistant","parent_tool_use_id":"task-1","message":{"content":[]}}"#,
            "task-1",
        )],
    ));

    // Text, thinking, ids, and every action block type.
    all.push(scenario(
        r"{}",
        "",
        &[
            handle(
                &assistant(
                    r#"[{"type":"text","text":"  hello  "},{"type":"thinking","thinking":" hmm "},{"type":"text","text":"   "},{"type":"text","text":5},"str",null,{"type":5}]"#,
                ),
                "task-1",
            ),
            handle(
                &format!(
                    r#"{{"type":"assistant","message":{{"id":" msg-1 ","content":[{},{},{},{},{}]}}}}"#,
                    r#"{"type":"text","text":"with id"}"#,
                    tool_use("tu-1", "Read", r#"{"file_path":"/repo/a.rs"}"#),
                    r#"{"type":"tool_use","name":"Bash","input":{"command":"ls -la"}}"#,
                    r#"{"type":"mcp_tool_use","id":"mcp-1","name":"mcp__server__tool","input":{"q":1}}"#,
                    r#"{"type":"server_tool_use","id":"srv-1","name":"web_search","input":{"query":"rust"}}"#,
                ),
                "task-1",
            ),
            handle(
                &assistant(r#"[{"type":"tool_use","id":"x","name":5},{"type":"tool_use","id":" ","name":"Grep"},{"type":"tool_use","id":"y","name":"  "}]"#),
                "task-1",
            ),
            handle(
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu-1","content":"file text"},{"type":"tool_result","tool_use_id":"tu-1","content":"again"}]}}"#,
                "task-1",
            ),
            handle(
                r#"{"type":"user","message":{"content":[{"type":"mcp_tool_result","tool_use_id":"mcp-1","content":[{"type":"text","text":"ok"}]},{"type":"tool_result","tool_use_id":"srv-1","is_error":true,"content":"boom"},{"type":"tool_result","tool_use_id":"orphan","tool_name":"Glob","content":"x"},{"type":"tool_result","tool_use_id":"orphan2","content":"no name"},{"type":"tool_result","content":"no id"}]}}"#,
                "task-1",
            ),
            handle(
                r#"{"type":"user","message":{"content":"plain text"}}"#,
                "task-1",
            ),
            handle(r#"{"type":"system"}"#, "task-1"),
            handle(r#"{"type":"assistant"}"#, "task-1"),
        ],
    ));

    // Stream events and tool progress.
    all.push(scenario(
        r"{}",
        "",
        &[
            handle(
                r#"{"type":"stream_event","event":{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"s1","name":"Read","input":{"file_path":"/a"}}}}"#,
                "task-1",
            ),
            handle(
                r#"{"type":"stream_event","event":{"type":"content_block_start","index":3,"content_block":{"type":"tool_use","name":"Write","input":{"file_path":"/b","content":"c"}}}}"#,
                "task-1",
            ),
            handle(
                r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"server_tool_use","name":"web_fetch"}}}"#,
                "task-1",
            ),
            handle(
                r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"text"}}}"#,
                "task-1",
            ),
            handle(
                r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"x"}}}"#,
                "task-1",
            ),
            handle(
                r#"{"type":"tool_progress","tool_name":" Bash ","tool_use_id":" p1 ","elapsed_time_seconds":2}"#,
                "task-1",
            ),
            handle(r#"{"type":"tool_progress","tool_name":"Bash"}"#, "task-1"),
            handle(r#"{"type":"tool_progress","tool_name":" "}"#, "task-1"),
            handle(r#"{"type":"tool_progress"}"#, "task-1"),
            handle(
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"s1","content":"done"},{"type":"tool_result","tool_use_id":"p1","content":"done"}]}}"#,
                "task-1",
            ),
        ],
    ));

    // The 200-entry trim: 205 actions in one sidechain, then a result for
    // an action that scrolled out and one that did not.
    let mut trim_steps = Vec::new();
    for index in 0..205 {
        trim_steps.push(handle(
            &assistant(&format!(
                "[{}]",
                tool_use(
                    &format!("t{index}"),
                    "Read",
                    &format!(r#"{{"file_path":"/f{index}"}}"#)
                )
            )),
            "task-1",
        ));
    }
    trim_steps.push(handle(
        r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t0","tool_name":"Read","content":"old"},{"type":"tool_result","tool_use_id":"t204","content":"new"}]}}"#,
        "task-1",
    ));
    trim_steps.push(handle(
        &assistant(&format!(
            "[{}]",
            tool_use("t204", "Read", r#"{"file_path":"/f204"}"#)
        )),
        "task-1",
    ));
    all.push(scenario(r"{}", "", &trim_steps));

    // The 160-unit cut, whitespace collapse, and a surrogate pair at the cut.
    let long_name = "n".repeat(200);
    let spaced = format!("  {}   {}\t\n{}  ", "a".repeat(100), "b".repeat(100), "c");
    let astral = format!("{}\u{1d11e}{}", "d".repeat(159), "e".repeat(20));
    let exact = "f".repeat(160);
    let over = "g".repeat(161);
    let inputs = serde_inputs(&[
        ("long", &[("name", &long_name), ("description", &spaced)]),
        ("astral", &[("name", &astral), ("subagent_type", &exact)]),
        (
            "over",
            &[("description", &over), ("subagent_type", "  Explore  ")],
        ),
    ]);
    all.push(scenario(
        &inputs,
        "",
        &["long", "astral", "over"]
            .iter()
            .map(|id| handle(&assistant("[]"), id))
            .collect::<Vec<_>>(),
    ));

    // Task input changes between frames; name and type precedence; a
    // non-object input; unknown ids.
    all.push(scenario(
        r#"{"task-1":{"description":"first"}}"#,
        "",
        &[
            handle(&assistant("[]"), "task-1"),
            handle(&assistant("[]"), "task-1"),
            r#"{"call":"input","id":"task-1","value":{"description":"first","subagent_type":"Explore"}}"#.to_owned(),
            handle(&assistant("[]"), "task-1"),
            r#"{"call":"input","id":"task-1","value":{"name":"renamed","description":"second"}}"#.to_owned(),
            handle(&assistant("[]"), "task-1"),
            r#"{"call":"input","id":"task-1","value":null}"#.to_owned(),
            handle(&assistant("[]"), "task-1"),
            r#"{"call":"input","id":"task-1","value":{"name":5,"subagent_type":[],"description":{}}}"#.to_owned(),
            handle(&assistant("[]"), "task-1"),
            handle(&assistant("[]"), "unknown"),
        ],
    ));

    // Descriptor ownership and the synthetic parent card.
    all.push(scenario(
        named_input,
        r#""synthetic":{"task-1":false},"#,
        &[
            handle(
                &assistant(&format!(
                    "[{}]",
                    tool_use("a", "Read", r#"{"file_path":"/x"}"#)
                )),
                "task-1",
            ),
            r#"{"call":"owned","value":true}"#.to_owned(),
            handle(
                &assistant(&format!(
                    "[{}]",
                    tool_use("b", "Read", r#"{"file_path":"/y"}"#)
                )),
                "task-1",
            ),
            handle(
                &assistant(&format!(
                    "[{}]",
                    tool_use("c", "Read", r#"{"file_path":"/z"}"#)
                )),
                "task-2",
            ),
            r#"{"call":"finish","id":"task-1","status":"completed"}"#.to_owned(),
            r#"{"call":"finishAll","status":"failed"}"#.to_owned(),
            r#"{"call":"owned","value":false}"#.to_owned(),
            handle(
                &assistant(&format!(
                    "[{}]",
                    tool_use("d", "Read", r#"{"file_path":"/w"}"#)
                )),
                "task-1",
            ),
            handle(
                &assistant(&format!(
                    "[{}]",
                    tool_use("e", "Read", r#"{"file_path":"/v"}"#)
                )),
                "task-2",
            ),
            r#"{"call":"finish","id":"missing","status":"completed"}"#.to_owned(),
            r#"{"call":"finish","id":"task-1","status":"canceled"}"#.to_owned(),
            r#"{"call":"finishAll","status":"canceled"}"#.to_owned(),
            r#"{"call":"finishAll","status":"completed"}"#.to_owned(),
            handle(&assistant("[]"), "task-1"),
            r#"{"call":"clear"}"#.to_owned(),
            r#"{"call":"finishAll","status":"completed"}"#.to_owned(),
        ],
    ));

    // The display summary of every tool-call detail type, and tool names
    // that throw or map to nothing.
    let tools: &[(&str, &str)] = &[
        ("Bash", r#"{"command":"cargo test"}"#),
        ("Bash", r#"{"command":"  spaced   out  "}"#),
        ("Read", r#"{"file_path":"/repo/src/lib.rs"}"#),
        ("Read", r#"{"file_path":"/repo/a.rs","offset":2,"limit":5}"#),
        (
            "Edit",
            r#"{"file_path":"/repo/a.rs","old_string":"a","new_string":"b"}"#,
        ),
        (
            "MultiEdit",
            r#"{"file_path":"/repo/a.rs","edits":[{"old_string":"a","new_string":"b"}]}"#,
        ),
        (
            "Write",
            r#"{"file_path":"/repo/new.rs","content":"fn main() {}"}"#,
        ),
        ("Grep", r#"{"pattern":"TODO","path":"/repo"}"#),
        ("Glob", r#"{"pattern":"**/*.rs"}"#),
        ("WebSearch", r#"{"query":"rust async"}"#),
        ("WebFetch", r#"{"url":"https://example.com","prompt":"p"}"#),
        (
            "Task",
            r#"{"description":"child task","subagent_type":"Explore","prompt":"x"}"#,
        ),
        (
            "Agent",
            r#"{"description":"agent task","subagent_type":"general-purpose"}"#,
        ),
        (
            "TodoWrite",
            r#"{"todos":[{"content":"a","status":"pending"}]}"#,
        ),
        ("Skill", r#"{"skill":"commit"}"#),
        (
            "NotebookEdit",
            r#"{"notebook_path":"/n.ipynb","new_source":"x"}"#,
        ),
        ("Thinking", r"{}"),
        ("Terminal", r#"{"command":"ls"}"#),
        ("terminal", r#"{"label":"x"}"#),
        ("mcp__server__tool", r#"{"a":1}"#),
        ("paseo__create_agent", r#"{"title":"t"}"#),
        ("Unknown_Tool-name.v2", r#"{"a":{"b":1}}"#),
        ("Read", r#"{"file_path":5}"#),
        ("Read", r"[1,2]"),
        ("Read", r"null"),
        ("Bash", r#""just a string""#),
        ("Edit", r#"{"file_path":"/x","patch":"@@"}"#),
        ("SomeTool", r#"{"filePath":"/p"}"#),
    ];
    let mut tool_steps = Vec::new();
    for (index, (name, input)) in tools.iter().enumerate() {
        tool_steps.push(handle(
            &assistant(&format!(
                "[{}]",
                tool_use(&format!("k{index}"), name, input)
            )),
            "task-1",
        ));
    }
    all.push(scenario(r"{}", "", &tool_steps));

    // Results mapped through the completed and failed mappers.
    let mut result_steps = Vec::new();
    for (index, (name, input)) in tools.iter().enumerate() {
        result_steps.push(handle(
            &assistant(&format!(
                "[{}]",
                tool_use(&format!("r{index}"), name, input)
            )),
            "task-2",
        ));
        let failing = if index % 2 == 0 { "true" } else { "false" };
        result_steps.push(handle(
            &format!(
                r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","tool_use_id":"r{index}","is_error":{failing},"content":"result {index}"}}]}}}}"#
            ),
            "task-2",
        ));
    }
    all.push(scenario(r"{}", "", &result_steps));
    all
}

/// `{ id: { key: "text" } }` as JSON text.
fn serde_inputs(entries: &[(&str, &[(&str, &str)])]) -> String {
    let mut out = String::from("{");
    for (index, (id, fields)) in entries.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let mut object = JsObject::new();
        for (key, value) in *fields {
            object.insert(*key, JsValue::String((*value).to_owned()));
        }
        let _ = write!(
            out,
            "{}:{}",
            stringify(&JsValue::String((*id).to_owned())),
            stringify(&JsValue::Object(object))
        );
    }
    out.push('}');
    out
}

fn rust_output(scenarios_json: &str) -> String {
    let scenarios = parse(scenarios_json).expect("scenarios JSON");
    let mut out = Vec::new();
    for scenario in scenarios.as_array().expect("scenarios array") {
        let mut inputs: HashMap<String, JsValue> = scenario
            .get("toolInputs")
            .and_then(JsValue::as_object)
            .map(|object| {
                object
                    .iter()
                    .map(|(key, value)| (key.to_owned(), value.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let synthetic: HashMap<String, bool> = scenario
            .get("synthetic")
            .and_then(JsValue::as_object)
            .map(|object| {
                object
                    .iter()
                    .map(|(key, value)| (key.to_owned(), value.as_bool().unwrap_or(true)))
                    .collect()
            })
            .unwrap_or_default();
        let mut owned = false;
        let mut tracker = ClaudeSidechainTracker::default();
        for step in scenario
            .get("steps")
            .and_then(JsValue::as_array)
            .unwrap_or_default()
        {
            let text = |key: &str| {
                step.get(key)
                    .and_then(JsValue::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let rendered = match step.get("call").and_then(JsValue::as_str) {
                Some("handle") => {
                    let lookup_inputs = inputs.clone();
                    let lookup =
                        move |id: &str| lookup_inputs.get(id).and_then(JsValue::as_object).cloned();
                    let needs_card = |id: &str| synthetic.get(id).copied().unwrap_or(true);
                    let context = TrackerContext {
                        get_tool_input: &lookup,
                        is_descriptor_owned_elsewhere: owned,
                        needs_synthetic_parent_tool_card: &needs_card,
                    };
                    match tracker.handle_message(
                        step.get("message").unwrap_or(&JsValue::Undefined),
                        &text("parent"),
                        &context,
                    ) {
                        Ok(events) => stringify(&JsValue::Array(events)),
                        Err(error) => threw(&error.message),
                    }
                }
                Some("finish") => stringify(&JsValue::Array(tracker.finish(
                    &text("id"),
                    &text("status"),
                    owned,
                ))),
                Some("finishAll") => {
                    stringify(&JsValue::Array(tracker.finish_all(&text("status"), owned)))
                }
                Some("clear") => {
                    tracker.clear();
                    "null".to_owned()
                }
                Some("owned") => {
                    owned = step
                        .get("value")
                        .and_then(JsValue::as_bool)
                        .unwrap_or(false);
                    "null".to_owned()
                }
                Some("input") => {
                    inputs.insert(
                        text("id"),
                        step.get("value").cloned().unwrap_or(JsValue::Null),
                    );
                    "null".to_owned()
                }
                other => panic!("unknown call {other:?}"),
            };
            out.push(rendered);
        }
    }
    out.join("\n") + "\n"
}

fn threw(message: &str) -> String {
    let mut object = JsObject::new();
    object.insert("threw", JsValue::String(message.to_owned()));
    stringify(&JsValue::Object(object))
}

#[test]
fn sidechain_tracker_matches_the_pinned_build() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let scenarios_json = format!("[{}]", scenarios().join(","));
    let expected = support::run_node(
        &node,
        &dist,
        NODE_SCRIPT,
        std::slice::from_ref(&scenarios_json),
    );
    let actual = rust_output(&scenarios_json);
    for (index, (node_line, rust_line)) in expected.lines().zip(actual.lines()).enumerate() {
        assert_eq!(rust_line, node_line, "line {index} differs");
    }
    assert_eq!(actual, expected);
}
