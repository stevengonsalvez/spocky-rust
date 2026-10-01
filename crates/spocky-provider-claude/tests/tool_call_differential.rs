//! Differential check of the tool call mapper, detail parser, and partial
//! JSON parser against the pinned build: every case must print the same
//! `JSON.stringify` text, or the same thrown error, in both.

mod support;

use spocky_contracts::js_value::{JsValue, parse, stringify};
use spocky_provider_claude::partial_json::parse_partial_json_object;
use spocky_provider_claude::tool_call_detail::derive_claude_tool_detail;
use spocky_provider_claude::tool_call_mapper::{
    MapperParams, map_canceled, map_completed, map_failed, map_running,
};
use spocky_session::agent_sdk::AgentError;

/// `(status, params)`: the mapper entry point and its argument object.
const MAPPER_CASES: &[(&str, &str)] = &[
    // tool-call-mapper.test.ts fixtures.
    (
        "running",
        r#"{"name":"ExitPlanMode","callId":"plan-tool-1","input":{"plan":"Ship it"}}"#,
    ),
    (
        "failed",
        r#"{"name":"ExitPlanMode","callId":"plan-tool-1","input":{"plan":"Ship it"},"error":"Denied by user"}"#,
    ),
    (
        "completed",
        r#"{"name":"ExitPlanMode","callId":"plan-tool-1","input":{"plan":"Ship it"}}"#,
    ),
    (
        "canceled",
        r#"{"name":"ExitPlanMode","callId":"plan-tool-1","input":{"plan":"Ship it"}}"#,
    ),
    (
        "running",
        r#"{"callId":"claude-call-1","name":"Bash","input":{"command":"pwd","cwd":"/tmp/repo"},"output":null}"#,
    ),
    (
        "running",
        r#"{"callId":"claude-call-partial-1","name":"Bash","input":{"command":"echo "},"output":null}"#,
    ),
    (
        "running",
        r#"{"callId":"claude-running-read","name":"read_file","input":{"file_path":"README.md"},"output":null}"#,
    ),
    (
        "running",
        r#"{"callId":"claude-running-write","name":"write_file","input":{"file_path":"src/new.ts"},"output":null}"#,
    ),
    (
        "running",
        r#"{"callId":"claude-running-edit","name":"apply_patch","input":{"file_path":"src/index.ts"},"output":null}"#,
    ),
    (
        "running",
        r#"{"callId":"claude-running-search","name":"web_search","input":{"query":"tool call mapping"},"output":null}"#,
    ),
    (
        "completed",
        r#"{"callId":"claude-call-2","name":"read_file","input":{"file_path":"README.md"},"output":{"content":"hello"}}"#,
    ),
    (
        "completed",
        r#"{"callId":"claude-read-array","name":"read_file","input":{"file_path":"README.md"},"output":{"content":[{"type":"output_text","text":"alpha"},{"type":"output_text","content":"beta"}]}}"#,
    ),
    (
        "completed",
        r#"{"callId":"claude-read-object","name":"read_file","input":{"file_path":"README.md"},"output":{"structured_content":{"content":{"type":"output_text","text":"gamma"}}}}"#,
    ),
    (
        "failed",
        r#"{"callId":"claude-call-3","name":"shell","input":{"command":"false"},"output":null,"error":{"message":"Command failed"}}"#,
    ),
    (
        "completed",
        r#"{"callId":"claude-write-1","name":"write_file","input":{"file_path":"src/new.ts","content":"export const x = 1;"},"output":null}"#,
    ),
    (
        "completed",
        r#"{"callId":"claude-edit-1","name":"apply_patch","input":{"file_path":"src/index.ts","patch":"@@\\n-old\\n+new\\n"},"output":null}"#,
    ),
    (
        "completed",
        r#"{"callId":"claude-search-1","name":"web_search","input":{"query":"tool call mapping"},"output":null}"#,
    ),
    (
        "completed",
        r#"{"callId":"claude-call-4","name":"my_custom_tool","input":{"foo":"bar"},"output":{"ok":true}}"#,
    ),
    (
        "completed",
        r#"{"callId":"claude-glob-1","name":"Glob","input":{"pattern":"**/.claude/commands/paseo*"},"output":{"durationMs":7,"numFiles":2,"filenames":["a.txt","b.txt"],"truncated":false}}"#,
    ),
    (
        "completed",
        r#"{"callId":"claude-grep-1","name":"Grep","input":{"pattern":"\\\\\\\"cli\\\\\\\"\"","path":"/workspaces/paseo/packages/desktop/src","output_mode":"content","-n":true},"output":{"mode":"content","numFiles":1,"filenames":["src/main.rs"],"content":"12:const cli = true;","numLines":1,"numMatches":1}}"#,
    ),
    (
        "completed",
        r#"{"callId":"claude-grep-string-1","name":"Grep","input":{"pattern":"MaskedView","output_mode":"files_with_matches"},"output":{"output":"Found 2 files\nsrc/foo.tsx\nsrc/bar.tsx"}}"#,
    ),
    (
        "completed",
        r#"{"callId":"claude-web-search-1","name":"WebSearch","input":{"query":"OpenAI latest news"},"output":{"query":"OpenAI latest news","results":["Top results:",{"tool_use_id":"toolu_123","content":[{"title":"OpenAI launches thing","url":"https://example.com/1"},{"title":"Another result","url":"https://example.com/2"}]}],"durationSeconds":1.5}}"#,
    ),
    (
        "completed",
        r#"{"callId":"claude-web-fetch-1","name":"WebFetch","input":{"url":"https://example.com/article","prompt":"Summarize this page"},"output":{"bytes":5120,"code":200,"codeText":"OK","result":"Summary text","durationMs":250,"url":"https://example.com/article"}}"#,
    ),
    (
        "completed",
        r#"{"callId":"claude-speak-1","name":"mcp__paseo__speak","input":{"text":"Voice response from Claude."},"output":{"ok":true}}"#,
    ),
    (
        "completed",
        r#"{"callId":"claude-speak-2","name":"mcp__paseo_voice__speak","input":{"text":"Hey! I can hear you."},"output":{"ok":true}}"#,
    ),
    (
        "completed",
        r#"{"callId":null,"name":"read_file","input":{"file_path":"README.md"},"output":{"content":"hello"}}"#,
    ),
    // Plans: metadata merge, a non-string plan, and a failed plan's error.
    (
        "completed",
        r#"{"name":"ExitPlanMode","callId":"p2","input":{"plan":"x"},"metadata":{"actionId":"implement","approved":"over"}}"#,
    ),
    (
        "failed",
        r#"{"name":" ExitPlanMode ","callId":"p3","input":{"plan":"x"},"metadata":{"actionId":"reject"},"error":"no"}"#,
    ),
    (
        "running",
        r#"{"name":"ExitPlanMode","callId":"p4","input":{"plan":5}}"#,
    ),
    (
        "running",
        r#"{"name":"ExitPlanMode","callId":"p5","input":["plan"]}"#,
    ),
    // Identity edge cases.
    (
        "running",
        r#"{"name":"Bash","callId":"   ","input":{"command":"ls"}}"#,
    ),
    (
        "running",
        r#"{"name":"","callId":"c","input":{"command":"ls"}}"#,
    ),
    (
        "running",
        r#"{"name":"  Bash  ","callId":" c ","input":{"command":"ls"}}"#,
    ),
    (
        "failed",
        r#"{"name":"Bash","callId":"c","input":{"command":"ls"},"error":null}"#,
    ),
    (
        "failed",
        r#"{"name":"Bash","callId":"c","input":{"command":"ls"},"error":0,"metadata":{"b":1,"2":0,"a":2}}"#,
    ),
    (
        "completed",
        r#"{"name":"unknown_tool","callId":"c","metadata":{}}"#,
    ),
    // Shell variants.
    (
        "completed",
        r#"{"name":"Bash","callId":"c","input":{"command":["  git ","","status "],"directory":"/d","cwd":""},"output":"Chunk ID: 1\nWall time: 2s\nProcess exited with code 0\nOutput:\nhello"}"#,
    ),
    (
        "completed",
        r#"{"name":"bash","callId":"c","input":{"command":5,"cmd":"ls"},"output":{"exitCode":null,"exit_code":3,"result":{"command":"r","text":"t"}}}"#,
    ),
    (
        "completed",
        r#"{"name":"shell","callId":"c","input":{"cmd":["a",1]},"output":{"metadata":{"exitCode":2},"structuredContent":{"text":"s"}}}"#,
    ),
    (
        "completed",
        r#"{"name":"exec_command","callId":"c","input":null,"output":{"command":"from-output","aggregatedOutput":"agg","exitCode":0}}"#,
    ),
    (
        "completed",
        r#"{"name":"Bash","callId":"c","input":{"command":"ls","cwd":null},"output":null}"#,
    ),
    (
        "completed",
        r#"{"name":"Bash","callId":"c","input":{"command":""},"output":{"metadata":[]}}"#,
    ),
    // Read variants.
    (
        "completed",
        r#"{"name":"Read","callId":"c","input":{"file_path":"/a.ts","offset":3,"limit":2},"output":"     3\tconst a = 1;\n     4\tconst b = 2;\n"}"#,
    ),
    (
        "completed",
        r#"{"name":"Read","callId":"c","input":{"file_path":"/a.ts"},"output":"  10\tx\n  12\ty"}"#,
    ),
    (
        "completed",
        r#"{"name":"Read","callId":"c","input":{"file_path":"/a.ts"},"output":"1\ta\nplain\nplain2\nplain3"}"#,
    ),
    (
        "completed",
        r#"{"name":"Read","callId":"c","input":{"filePath":"  "},"output":"<path>/x.md</path>\n<content>\nA &amp; B &#65;&#x42; &bogus; &lt;\n</content>"}"#,
    ),
    (
        "completed",
        r#"{"name":"Read","callId":"c","input":{"file_path":"/a"},"output":"<path>/x.md</path><content>&#99999999;</content>"}"#,
    ),
    (
        "completed",
        r#"{"name":"Read","callId":"c","input":{"file_path":"/a"},"output":"<path>/x.md</path><CONTENT>&#x110000;</CONTENT>"}"#,
    ),
    (
        "completed",
        r#"{"name":"Read","callId":"c","input":{"file_path":"/a"},"output":{"data":{"text":[{"output":"o"},{"text":""}]}}}"#,
    ),
    (
        "completed",
        r#"{"name":"Read","callId":"c","input":{"file_path":"/a","offset":"1"},"output":"x"}"#,
    ),
    (
        "completed",
        r#"{"name":"view_file","callId":"c","input":{"path":"/p"},"output":[{"content":"c1"},{"text":"t2","output":5}]}"#,
    ),
    // Write and edit, including the intersection throw.
    (
        "completed",
        r#"{"name":"Write","callId":"c","input":{"path":"a","filePath":"b"}}"#,
    ),
    (
        "completed",
        r#"{"name":"Bash","callId":"c","input":{"command":5,"path":"a","filePath":"b"}}"#,
    ),
    (
        "completed",
        r#"{"name":"mystery","callId":"c","input":{"file_path":"a","filePath":7}}"#,
    ),
    (
        "completed",
        r#"{"name":"Write","callId":"c","input":{"path":"a","filePath":"a","content":"","newContent":"n"},"output":{"file_path":"o","content":"oc"}}"#,
    ),
    (
        "completed",
        r#"{"name":"Write","callId":"c","input":null,"output":{"new_content":"x"}}"#,
    ),
    (
        "completed",
        r#"{"name":"Write","callId":"c","input":{"path":"a","content":5},"output":"ok"}"#,
    ),
    (
        "completed",
        r#"{"name":"Edit","callId":"c","input":{"file_path":"/f","old_string":"a","new_str":"b","diff":"d"},"output":{"files":[{"path":"/g","unified_diff":"u"}]}}"#,
    ),
    (
        "completed",
        r#"{"name":"MultiEdit","callId":"c","input":null,"output":{"files":[{"filePath":"/g","patch":"p"},{"bad":1}],"content":"c"}}"#,
    ),
    (
        "completed",
        r#"{"name":"str_replace_editor","callId":"c","input":{"path":"/f"},"output":{"path":"/f2","filePath":"/other"}}"#,
    ),
    // Search, fetch, skill, speak.
    (
        "completed",
        r#"{"name":"Grep","callId":"c","input":{"q":"x"},"output":{"numFiles":0,"filenames":[],"truncated":"yes"}}"#,
    ),
    (
        "completed",
        r#"{"name":"grep","callId":"c","input":{"pattern":"x"},"output":{"mode":"bogus","numFiles":1,"filenames":["a"]}}"#,
    ),
    (
        "completed",
        r#"{"name":"glob","callId":"c","input":{"pattern":""},"output":null}"#,
    ),
    (
        "completed",
        r#"{"name":"search","callId":"c","input":{"query":"q"},"output":{"anything":1}}"#,
    ),
    (
        "completed",
        r#"{"name":"WebSearch","callId":"c","input":{"query":"q"},"output":{"query":"q","results":[{"tool_use_id":"t","content":[{"url":"u","extra":1,"title":"T"}],"zzz":0}],"durationSeconds":2,"filenames":["f"]}}"#,
    ),
    (
        "completed",
        r#"{"name":"WebFetch","callId":"c","input":{"url":"","prompt":"p"},"output":{"bytes":1,"code":-1,"codeText":"","result":"","durationMs":0,"url":"https://o"}}"#,
    ),
    (
        "completed",
        r#"{"name":"webfetch","callId":"c","input":{"url":"u"},"output":null}"#,
    ),
    (
        "completed",
        r#"{"name":"Skill","callId":"c","input":{"skill":"brainstorm","args":"x"},"output":{"output":"ran"}}"#,
    ),
    (
        "completed",
        r#"{"name":"Skill","callId":"c","input":{"skill":""},"output":"x"}"#,
    ),
    (
        "completed",
        r#"{"name":"tools.SPEAK","callId":"c","input":"  hi  ","output":1}"#,
    ),
    (
        "completed",
        r#"{"name":"speak","callId":"c","input":{"text":"   "}}"#,
    ),
    ("completed", r#"{"name":"x_speak_","callId":"c","input":5}"#),
];

/// `(name, input, output)` for `deriveClaudeToolDetail` directly; `null`
/// stands for both null and absent, as `?? null` makes them equal.
const DETAIL_CASES: &[(&str, &str, &str)] = &[
    ("", r#"{"a":1}"#, "2"),
    ("Edit", r#"{"file_path":"/f","patch":"PATCH"}"#, "null"),
    (
        "apply_patch",
        r#"{"path":"/f"}"#,
        r#"{"path":"/f","filePath":"/x"}"#,
    ),
    (
        "read_file",
        r#"{"file_path":"/f"}"#,
        r#""<path>/f</path>\n<content>&#55357;&#56832;&#xD800;</content>""#,
    ),
];

/// Inputs for `parsePartialJsonObject`.
const PARTIAL_CASES: &[&str] = &[
    r#"{"command":"pwd","cwd":"/tmp/repo"}"#,
    r#"{"command":"echo "#,
    r#"{"file_path":"src/message.tsx","old_string":"before"#,
    r#"{"payload":{"path":"src/index.ts","content":"hello"#,
    r#""text""#,
    r#" {"a":[1,-0,2.5e3,true,null,"\ud800xé"],"b":1e} "#,
    r#"{"a":1,"__proto__":{"x":1},"b":"\/"}"#,
    r#"{"a":"\u12"#,
    r#"{"a":tru"#,
    "{\"a\u{2028}\":\u{3000}1\u{feff}}",
];

const NODE_SCRIPT: &str = r#"
const [dist, mapperJson, detailJson, partialJson] = process.argv.slice(1);
const base = `${dist}/server/agent/providers/claude`;
const mapper = await import(`${base}/tool-call-mapper.js`);
const { deriveClaudeToolDetail } = await import(`${base}/tool-call-detail-parser.js`);
const { parsePartialJsonObject } = await import(`${base}/partial-json.js`);
const fns = {
  running: mapper.mapClaudeRunningToolCall,
  completed: mapper.mapClaudeCompletedToolCall,
  failed: mapper.mapClaudeFailedToolCall,
  canceled: mapper.mapClaudeCanceledToolCall,
};
const out = [];
const run = (fn) => {
  try {
    const result = fn();
    out.push(result === undefined ? "undefined" : JSON.stringify(result));
  } catch (error) {
    out.push(`THROW ${error.name}: ${error.message}`);
  }
};
for (const [status, params] of JSON.parse(mapperJson)) run(() => fns[status](JSON.parse(params)));
for (const [name, input, output] of JSON.parse(detailJson))
  run(() => deriveClaudeToolDetail(name, JSON.parse(input), JSON.parse(output)));
for (const input of JSON.parse(partialJson)) run(() => parsePartialJsonObject(input));
process.stdout.write(out.join("\n") + "\n");
"#;

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/agent/providers/claude/tool-call-mapper.js",
        "ce7a4ce016271bc7637d8e226952b0ccee411352fcad9514efba5660339bfda1",
    ),
    (
        "server/agent/providers/claude/tool-call-detail-parser.js",
        "cef7f2fe967057dfac71c0b0012875e4496c1a4755c0ddf7c9618b6be6174534",
    ),
    (
        "server/agent/providers/tool-call-detail-primitives.js",
        "fde3bdc6089e3c4a98f0f641d066cd8302762b1b358fa2a702bc5fa8f0fd8762",
    ),
    (
        "server/agent/providers/tool-call-mapper-utils.js",
        "c2e99061f5fa571579f6384e48330f12384bd559aa0ce975daec4fddabcc4b08",
    ),
    (
        "server/agent/providers/claude/partial-json.js",
        "882aca46cc529af7b930be1869faf7146fb33e03ca4bbc0cc978fd3ced00291f",
    ),
];

fn line(result: Result<Option<JsValue>, AgentError>) -> String {
    match result {
        Ok(Some(value)) => stringify(&value),
        Ok(None) => "null".to_owned(),
        Err(error) => format!("THROW {}: {}", error.name, error.message),
    }
}

fn rust_output() -> String {
    let mut out = Vec::new();
    for (status, params) in MAPPER_CASES {
        let params = parse(params).expect("params JSON");
        let member = |key: &str| params.get(key);
        let mapper = MapperParams {
            call_id: member("callId").and_then(JsValue::as_str),
            name: member("name").and_then(JsValue::as_str).unwrap_or(""),
            input: member("input"),
            output: member("output"),
            metadata: member("metadata").and_then(JsValue::as_object),
        };
        out.push(line(match *status {
            "running" => map_running(&mapper),
            "completed" => map_completed(&mapper),
            "failed" => map_failed(&mapper, member("error")),
            _ => map_canceled(&mapper),
        }));
    }
    for (name, input, output) in DETAIL_CASES {
        let input = parse(input).expect("input JSON");
        let output = parse(output).expect("output JSON");
        out.push(line(
            derive_claude_tool_detail(name, Some(&input), Some(&output)).map(Some),
        ));
    }
    for input in PARTIAL_CASES {
        out.push(match parse_partial_json_object(input) {
            Some(parsed) => {
                let mut object = spocky_contracts::js_value::JsObject::new();
                object.insert("value", JsValue::Object(parsed.value));
                object.insert("complete", JsValue::Bool(parsed.complete));
                stringify(&JsValue::Object(object))
            }
            None => "null".to_owned(),
        });
    }
    out.join("\n") + "\n"
}

fn json_list(items: impl Iterator<Item = Vec<String>>) -> String {
    stringify(&JsValue::Array(
        items
            .map(|item| JsValue::Array(item.into_iter().map(JsValue::String).collect()))
            .collect(),
    ))
}

#[test]
fn tool_calls_match_the_pinned_mapper() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let mapper = json_list(
        MAPPER_CASES
            .iter()
            .map(|(status, params)| vec![(*status).to_owned(), (*params).to_owned()]),
    );
    let detail = json_list(DETAIL_CASES.iter().map(|(name, input, output)| {
        vec![
            (*name).to_owned(),
            (*input).to_owned(),
            (*output).to_owned(),
        ]
    }));
    let partial = stringify(&JsValue::Array(
        PARTIAL_CASES
            .iter()
            .map(|input| JsValue::String((*input).to_owned()))
            .collect(),
    ));
    let expected = support::run_node(&node, &dist, NODE_SCRIPT, &[mapper, detail, partial]);
    let actual = rust_output();
    for (index, (node_line, rust_line)) in expected.lines().zip(actual.lines()).enumerate() {
        assert_eq!(rust_line, node_line, "case {index} differs");
    }
    assert_eq!(actual, expected);
}
