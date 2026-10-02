//! Differential check of `parseClaudeSubagentMeta` and
//! `observeReplaySubagents` against the pinned build. Each case prints the
//! observations (one fixed key order on both sides), their fold into store
//! events, and the tool owners (sorted by key; the baseline only `get`s
//! them). The fixtures are those of the baseline `replay-source.test.ts`,
//! with `convertEntry` a stub that returns the case's items for every entry,
//! plus edge cases. The run happens under two time zones.

mod support;

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_provider_claude::subagents::observation::{
    SubagentObservation, fold_subagent_observations,
};
use spocky_provider_claude::subagents::replay_source::{
    ClaudeReplayParentFacts, ClaudeReplaySubagentInput, ClaudeSubagentMeta, ReplayToolCall,
    observe_replay_subagents, parse_claude_subagent_meta,
};

const CHILD_ENV: &str = "SPOCKY_REPLAY_SOURCE_CHILD";
const RESULT_PREFIX: &str = "RESULT ";
const ZONES: [&str; 2] = ["UTC", "Europe/London"];

const TOOL: &str = "toolu_01DgLoPMW9";
const AGENT: &str = "a1730a6215e1f5cf6";

const NODE_SCRIPT: &str = r#"
const [dist, casesJson] = process.argv.slice(1);
const base = `${dist}/server/agent/providers/claude/subagents`;
const { observeReplaySubagents, parseClaudeSubagentMeta } = await import(`${base}/replay-source.js`);
const { foldSubagentObservations } = await import(`${base}/observation.js`);
const KEYS = ["kind", "id", "title", "description", "toolCallId", "parentSubagentId", "status", "subtitle", "item", "timestamp"];
const canon = (observation) => {
  const out = {};
  for (const key of KEYS) if (observation[key] !== undefined) out[key] = observation[key];
  return out;
};
const facts = (raw) => ({
  toolCalls: new Map(Object.entries(raw?.toolCalls ?? {})),
  linksByAgentId: new Map(Object.entries(raw?.linksByAgentId ?? {})),
  outcomesByToolCallId: new Map(Object.entries(raw?.outcomesByToolCallId ?? {})),
});
const lines = [];
const put = (value) => lines.push(`RESULT ${value === undefined ? "undefined" : JSON.stringify(value)}`);
for (const testCase of JSON.parse(casesJson)) {
  if (testCase.parseMeta !== undefined) {
    put(parseClaudeSubagentMeta(testCase.parseMeta));
    continue;
  }
  try {
    const items = testCase.convert ?? [];
    const result = observeReplaySubagents({
      subagents: testCase.subagents.map((subagent) => ({
        agentId: subagent.agentId,
        meta: subagent.meta,
        entries: subagent.entries,
        ...(subagent.parentFacts ? { parentFacts: facts(subagent.parentFacts) } : {}),
      })),
      parent: facts(testCase.parent),
      convertEntry: () => items,
    });
    put(result.observations.map(canon));
    put(foldSubagentObservations(result.observations));
    put([...result.toolOwners].sort((left, right) => (left[0] < right[0] ? -1 : left[0] > right[0] ? 1 : 0)));
  } catch (error) {
    put({ threw: error instanceof Error ? error.message : String(error) });
  }
}
process.stdout.write(lines.join("\n") + "\n");
"#;

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/agent/providers/claude/subagents/replay-source.js",
        "6004429185a16d9500661959158ee1b43dc8ce8f2be37e66ffa501200551b6b2",
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
        "server/agent/provider-history-timestamps.js",
        "2b9749871c429dc99fa9062f891dfefd1034a8cb5e9d9cbf71c52797cd9cf7c5",
    ),
    (
        "server/agent/providers/claude/models.js",
        "b1b0c2017f73b016eb980ee6748dbb5df61ec9591cd484c1e7e22c2eebefac03",
    ),
];

fn parent_with_task(failed: bool) -> String {
    format!(
        r#"{{"toolCalls":{{"{TOOL}":{{"title":"general-purpose","description":"Summarize hover and unistyles docs"}}}},"outcomesByToolCallId":{{"{TOOL}":{{"failed":{failed}}}}}}}"#
    )
}

fn parent_without_outcome() -> String {
    format!(
        r#"{{"toolCalls":{{"{TOOL}":{{"title":"general-purpose","description":"Summarize hover and unistyles docs"}}}}}}"#
    )
}

const EMPTY_PARENT: &str = "{}";

fn subagent(agent_id: &str, meta: &str, entries: &str) -> String {
    format!(r#"{{"agentId":"{agent_id}","meta":{meta},"entries":{entries}}}"#)
}

fn case(subagents: &[String], parent: &str, convert: &str) -> String {
    format!(
        r#"{{"subagents":[{}],"parent":{parent},"convert":{convert}}}"#,
        subagents.join(",")
    )
}

fn assistant(extra: &str) -> String {
    format!(r#"{{"type":"assistant"{extra}}}"#)
}

fn usage_case(entries: &str) -> String {
    case(
        &[subagent(
            AGENT,
            &format!(r#"{{"toolUseId":"{TOOL}"}}"#),
            entries,
        )],
        &parent_with_task(false),
        "[]",
    )
}

fn tool_meta() -> String {
    format!(r#"{{"toolUseId":"{TOOL}"}}"#)
}

#[allow(clippy::too_many_lines)] // A list of fixtures.
fn cases() -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    // parseClaudeSubagentMeta, including the baseline fixtures.
    for contents in [
        r#"{"agentType":"general-purpose","description":"Reply banana","toolUseId":"toolu_013i","spawnDepth":1}"#,
        r#"{"agentType":"Explore"#,
        "agentType: Explore",
        "",
        "[1,2,3]",
        r#"{"unrelated":true}"#,
        r#"{"agentType":"Explore","toolUseId":42}"#,
        r#"{"spawnDepth":"2"}"#,
        r#"{"spawnDepth":0}"#,
        r#"{"spawnDepth":-1.5,"agentType":""}"#,
        r#"{"description":null}"#,
        "null",
        "7",
        r#""text""#,
        r#"{"toolUseId":" t "}"#,
    ] {
        all.push(format!(
            r#"{{"parseMeta":{}}}"#,
            stringify(&JsValue::String(contents.to_owned()))
        ));
    }

    // observeReplaySubagents fixtures.
    all.push(case(
        &[subagent(AGENT, &tool_meta(), "[]")],
        &parent_with_task(false),
        "[]",
    ));
    let nested = "toolu_012rzYnFZA";
    all.push(case(
        &[
            format!(
                r#"{{"agentId":"{AGENT}","meta":{{"toolUseId":"{TOOL}","spawnDepth":1}},"entries":[],"parentFacts":{{"toolCalls":{{"{nested}":{{"title":"Explore","description":"Nested audit"}}}},"outcomesByToolCallId":{{"{nested}":{{"failed":false}}}}}}}}"#
            ),
            subagent(
                "a6acb4b898",
                &format!(r#"{{"toolUseId":"{nested}","agentType":"Explore","spawnDepth":2}}"#),
                "[]",
            ),
        ],
        &parent_with_task(false),
        "[]",
    ));
    // The grandchild listed first, and a spawnDepth sort with ties.
    all.push(case(
        &[
            subagent(
                "a6acb4b898",
                &format!(r#"{{"toolUseId":"{nested}","spawnDepth":2}}"#),
                "[]",
            ),
            format!(
                r#"{{"agentId":"{AGENT}","meta":{{"toolUseId":"{TOOL}","spawnDepth":1}},"entries":[{}],"parentFacts":{{"toolCalls":{{"{nested}":{{"title":"Explore"}}}},"outcomesByToolCallId":{{}}}}}}"#,
                assistant(r#","message":{"content":[{"type":"tool_use","id":"tu-own","name":"Read"},{"type":"tool_use"},{"type":"text"},null,5]}"#)
            ),
        ],
        &parent_with_task(true),
        "[]",
    ));
    all.push(case(
        &[subagent(AGENT, &tool_meta(), "[]")],
        &parent_with_task(false),
        "[]",
    ));
    all.push(case(
        &[subagent(
            AGENT,
            &format!(
                r#"{{"toolUseId":"{TOOL}","agentType":"Explore","description":"Find the code"}}"#
            ),
            "[]",
        )],
        EMPTY_PARENT,
        "[]",
    ));
    let scraped = format!(
        r#"{{"toolCalls":{{"{TOOL}":{{"title":"Explore"}}}},"linksByAgentId":{{"{AGENT}":{{"toolCallId":"{TOOL}","failed":false}}}}}}"#
    );
    all.push(case(&[subagent(AGENT, "null", "[]")], &scraped, "[]"));
    all.push(case(&[subagent(AGENT, "null", "[]")], EMPTY_PARENT, "[]"));
    let end_turn = assistant(
        r#","timestamp":"2026-07-31T03:41:00.000Z","message":{"stop_reason":"end_turn"}"#,
    );
    all.push(case(
        &[subagent(AGENT, &tool_meta(), &format!("[{end_turn}]"))],
        EMPTY_PARENT,
        "[]",
    ));
    all.push(case(
        &[subagent(
            AGENT,
            r#"{"agentType":"general-purpose"}"#,
            &format!("[{end_turn}]"),
        )],
        EMPTY_PARENT,
        "[]",
    ));
    all.push(case(
        &[subagent(AGENT, "null", "[]")],
        &format!(
            r#"{{"toolCalls":{{"{TOOL}":{{"title":"Explore"}}}},"linksByAgentId":{{"{AGENT}":{{"toolCallId":"{TOOL}","failed":true}}}}}}"#
        ),
        "[]",
    ));
    all.push(case(
        &[subagent(
            AGENT,
            r#"{"agentType":"general-purpose"}"#,
            &format!(
                "[{}]",
                assistant(
                    r#","timestamp":"2026-07-31T03:41:00.000Z","message":{"stop_reason":null}"#
                )
            ),
        )],
        EMPTY_PARENT,
        "[]",
    ));
    all.push(case(
        &[subagent(AGENT, &tool_meta(), &format!("[{end_turn}]"))],
        &parent_without_outcome(),
        "[]",
    ));
    all.push(case(
        &[subagent(AGENT, &tool_meta(), "[]")],
        &parent_without_outcome(),
        "[]",
    ));
    all.push(case(
        &[subagent(AGENT, &tool_meta(), "[]")],
        &parent_with_task(true),
        "[]",
    ));
    all.push(case(
        &[subagent(
            AGENT,
            &tool_meta(),
            r#"[{"type":"assistant","timestamp":"2026-07-26T06:27:47.034Z"}]"#,
        )],
        &parent_with_task(false),
        r#"[{"type":"reasoning","text":"thinking"}]"#,
    ));

    // Runtime: model and effort.
    for entries in [
        r#"[{"type":"assistant","effort":"high","message":{"model":"claude-opus-5"}}]"#,
        r#"[{"type":"assistant","effort":"high","message":{"model":"claude-opus-5"}},{"type":"assistant","effort":"low","message":{"model":"claude-sonnet-5"}}]"#,
        "[]",
        r#"[{"type":"assistant","message":{"model":"glm-5.1"}}]"#,
        r#"[{"type":"assistant","effort":"  xhigh  ","message":{"model":5}},{"type":"user","effort":"low","message":{"model":"claude-haiku-4-5"}}]"#,
        r#"[{"type":"assistant","effort":5},{"type":"assistant","effort":"   "}]"#,
        r#"[{"type":"assistant","message":"text"},{"type":"assistant"}]"#,
    ] {
        all.push(usage_case(entries));
    }

    // Usage: the baseline fixtures and edge cases.
    for entries in [
        r#"[{"type":"assistant","timestamp":"2026-07-23T23:18:43.023Z","message":{"model":"claude-opus-5","content":[{"type":"tool_use","name":"Read"},{"type":"text","text":"hi"}],"usage":{"input_tokens":4,"cache_creation_input_tokens":15000,"cache_read_input_tokens":0,"output_tokens":120}}},{"type":"user","timestamp":"2026-07-23T23:20:00.000Z","message":{"content":[]}},{"type":"assistant","timestamp":"2026-07-23T23:49:05.068Z","message":{"model":"claude-opus-5","content":[{"type":"tool_use","name":"Grep"}],"usage":{"input_tokens":2293,"cache_creation_input_tokens":0,"cache_read_input_tokens":65024,"output_tokens":1376}}}]"#,
        r#"[{"type":"assistant","timestamp":"2026-07-27T16:42:14.698Z","message":{"content":[{"type":"text","text":"done"}],"usage":{"input_tokens":16434}}},{"type":"user","timestamp":"2026-07-27T16:42:16.926Z","message":{"content":"ignored"}}]"#,
        r#"[{"type":"assistant","timestamp":"2026-07-27T16:42:14.698Z","message":{"content":[],"usage":{"input_tokens":10}}}]"#,
        r#"[{"type":"assistant","timestamp":"2026-07-27T16:42:14.698Z","message":{"content":[]}},{"type":"assistant","timestamp":"2026-07-27T16:42:20.000Z","message":{"content":[{"type":"tool_use","name":"Bash"}],"usage":"not-an-object"}}]"#,
        r#"[{"type":"assistant","timestamp":"2026-07-27T16:42:14.698Z","message":{"content":[],"usage":{"input_tokens":900,"output_tokens":100}}},{"type":"assistant","timestamp":"2026-07-27T16:42:20.000Z","message":{"content":[]}}]"#,
        r#"[{"type":"user","timestamp":"2026-07-27T16:42:14.698Z","message":{"content":"prompt"}},{"type":"user","timestamp":"2026-07-27T16:42:20.000Z","message":{"content":"more"}}]"#,
        r#"[{"type":"assistant","message":{"usage":{"input_tokens":7}},"timestamp":"2026-07-27T16:42:14.698Z"}]"#,
        r#"[{"type":"assistant","message":{"usage":{"input_tokens":"7","output_tokens":3}}}]"#,
        r#"[{"type":"assistant","message":{"usage":{}}}]"#,
        r#"[{"type":"assistant","message":{"usage":{"input_tokens":0}}}]"#,
        r#"[{"type":"assistant","message":{"usage":{"input_tokens":999.5,"output_tokens":0.25}}}]"#,
        r#"[{"type":"assistant","message":{"usage":{"input_tokens":0.49999999999999994}}}]"#,
        r#"[{"type":"assistant","message":{"usage":{"input_tokens":-5,"output_tokens":3}}}]"#,
    ] {
        all.push(usage_case(entries));
    }

    // Timestamps on entries: ISO, legacy, offset-less, numeric, invalid.
    for stamp in [
        r#""2026-10-01T10:00:00Z""#,
        r#""2026-10-01T10:00:00""#,
        r#""2026-10-01 10:00:00""#,
        r#""Oct 1 2026""#,
        r#"" 2026-10-01T10:00:00+01:00 ""#,
        "1700000000",
        "1700000000123",
        "-5",
        "1e300",
        "true",
        "null",
        r#""not a date""#,
        r#""""#,
    ] {
        all.push(case(
            &[subagent(
                AGENT,
                &tool_meta(),
                &format!(r#"[{{"type":"assistant","timestamp":{stamp}}},{{"type":"user","timestamp":{stamp}}}]"#),
            )],
            &parent_with_task(false),
            r#"[{"type":"assistant_message","text":"x"}]"#,
        ));
    }

    // Link edge cases: whitespace meta id, scraped link to an undeclared
    // call, two children for one call, multiple generations, tool owners.
    all.push(case(
        &[subagent(
            AGENT,
            &format!(r#"{{"toolUseId":"  {TOOL}  "}}"#),
            "[]",
        )],
        &parent_with_task(false),
        "[]",
    ));
    all.push(case(
        &[subagent(AGENT, r#"{"toolUseId":"   "}"#, "[]")],
        &scraped,
        "[]",
    ));
    all.push(case(
        &[subagent(AGENT, "null", "[]")],
        &format!(
            r#"{{"toolCalls":{{}},"linksByAgentId":{{"{AGENT}":{{"toolCallId":"{TOOL}","failed":false}}}}}}"#
        ),
        "[]",
    ));
    all.push(case(
        &[
            subagent("agent-a", &tool_meta(), r#"[{"type":"assistant","message":{"content":[{"type":"tool_use","id":"own-1"}]}}]"#),
            subagent("agent-b", &tool_meta(), r#"[{"type":"assistant","message":{"content":[{"type":"tool_use","id":"own-2"},{"type":"tool_use","id":"own-1"}]}}]"#),
        ],
        &parent_with_task(false),
        "[]",
    ));
    all.push(case(
        &[
            subagent("deep", r#"{"toolUseId":"t3","spawnDepth":3}"#, "[]"),
            r#"{"agentId":"mid","meta":{"toolUseId":"t2","spawnDepth":2},"entries":[],"parentFacts":{"toolCalls":{"t3":{"title":"C"}},"outcomesByToolCallId":{"t3":{"failed":true}}}}"#.to_owned(),
            r#"{"agentId":"top","meta":{"toolUseId":"t1"},"entries":[],"parentFacts":{"toolCalls":{"t2":{"title":"B","description":"d"}},"outcomesByToolCallId":{}}}"#.to_owned(),
        ],
        r#"{"toolCalls":{"t1":{"title":"A"}},"outcomesByToolCallId":{"t1":{"failed":false}}}"#,
        r#"[{"type":"user_message","text":"hi"},{"type":"tool_call","name":"Read"}]"#,
    ));
    // An unlinkable sidecar next to a linked one.
    all.push(case(
        &[
            subagent("ambient", r#"{"agentType":"x"}"#, "[]"),
            subagent(AGENT, &tool_meta(), "[]"),
        ],
        &parent_with_task(false),
        "[]",
    ));
    all
}

fn facts_from(value: Option<&JsValue>) -> ClaudeReplayParentFacts {
    let mut facts = ClaudeReplayParentFacts::default();
    let Some(value) = value else {
        return facts;
    };
    let entries = |key: &str| {
        value
            .get(key)
            .and_then(JsValue::as_object)
            .map(|object| {
                object
                    .iter()
                    .map(|(name, entry)| (name.to_owned(), entry.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    for (id, call) in entries("toolCalls") {
        let text = |key: &str| call.get(key).and_then(JsValue::as_str).map(str::to_owned);
        facts.tool_calls.insert(
            id,
            ReplayToolCall {
                title: text("title"),
                description: text("description"),
            },
        );
    }
    for (agent, link) in entries("linksByAgentId") {
        facts.links_by_agent_id.insert(
            agent,
            (
                link.get("toolCallId")
                    .and_then(JsValue::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                link.get("failed")
                    .and_then(JsValue::as_bool)
                    .unwrap_or(false),
            ),
        );
    }
    for (id, outcome) in entries("outcomesByToolCallId") {
        facts.outcomes_by_tool_call_id.insert(
            id,
            outcome
                .get("failed")
                .and_then(JsValue::as_bool)
                .unwrap_or(false),
        );
    }
    facts
}

fn meta_from(value: &JsValue) -> Option<ClaudeSubagentMeta> {
    let object = value.as_object()?;
    let text = |key: &str| object.get(key).and_then(JsValue::as_str).map(str::to_owned);
    Some(ClaudeSubagentMeta {
        agent_type: text("agentType"),
        description: text("description"),
        tool_use_id: text("toolUseId"),
        spawn_depth: object.get("spawnDepth").and_then(JsValue::as_f64),
    })
}

fn text_of(value: Option<&String>) -> JsValue {
    value.map_or(JsValue::Undefined, |text| JsValue::String(text.clone()))
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
        if let Some(contents) = test_case.get("parseMeta").and_then(JsValue::as_str) {
            lines.push(match parse_claude_subagent_meta(contents) {
                Some(meta) => {
                    let mut object = JsObject::new();
                    object.insert("agentType", text_of(meta.agent_type.as_ref()));
                    object.insert("description", text_of(meta.description.as_ref()));
                    object.insert("toolUseId", text_of(meta.tool_use_id.as_ref()));
                    object.insert(
                        "spawnDepth",
                        meta.spawn_depth.map_or(JsValue::Undefined, JsValue::Number),
                    );
                    result_line(&JsValue::Object(object))
                }
                None => result_line(&JsValue::Null),
            });
            continue;
        }
        let items: Vec<JsValue> = test_case
            .get("convert")
            .and_then(JsValue::as_array)
            .map(<[JsValue]>::to_vec)
            .unwrap_or_default();
        let subagents: Vec<ClaudeReplaySubagentInput> = test_case
            .get("subagents")
            .and_then(JsValue::as_array)
            .unwrap_or_default()
            .iter()
            .map(|subagent| ClaudeReplaySubagentInput {
                agent_id: subagent
                    .get("agentId")
                    .and_then(JsValue::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                meta: subagent.get("meta").and_then(meta_from),
                entries: subagent
                    .get("entries")
                    .and_then(JsValue::as_array)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|entry| entry.as_object().cloned())
                    .collect(),
                parent_facts: subagent
                    .get("parentFacts")
                    .map(|facts| facts_from(Some(facts))),
            })
            .collect();
        let parent = facts_from(test_case.get("parent"));
        let mut convert = |_: &JsObject| Ok(items.clone());
        let (observations, owners) =
            observe_replay_subagents(subagents, &parent, &mut convert).expect("replay");
        lines.push(result_line(&JsValue::Array(
            observations.iter().map(canon).collect(),
        )));
        lines.push(result_line(&JsValue::Array(fold_subagent_observations(
            &observations,
        ))));
        let sorted: std::collections::BTreeMap<&String, &String> = owners.iter().collect();
        lines.push(result_line(&JsValue::Array(
            sorted
                .into_iter()
                .map(|(tool, owner)| {
                    JsValue::Array(vec![
                        JsValue::String(tool.clone()),
                        JsValue::String(owner.clone()),
                    ])
                })
                .collect(),
        )));
    }
    lines
}

/// The child: prints the Rust side's result lines under the inherited `TZ`.
#[test]
fn child_prints_replay_source_results() {
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
fn replay_source_matches_the_pinned_build() {
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
            "child_prints_replay_source_results",
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
