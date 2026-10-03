//! Differential check of the tool call detail primitives against the pinned
//! build: `tool-call-detail-primitives.js` (each schema pair through
//! `toolDetailBranchByName`, then its `to*ToolDetail` mapper) and the pure
//! helpers of `tool-call-mapper-utils.js`. Every case must print the same
//! `JSON.stringify` text, `REJECT` (zod reported issues), `undefined`, or the
//! same thrown error, in both.

#[path = "support/pinned_node.rs"]
mod support;

use spocky_contracts::js_value::{JsValue, parse, stringify};
use spocky_contracts::tool_detail::{
    Parse, Thrown, edit_detail, edit_input, edit_output, extract_codex_shell_output, fetch_detail,
    fetch_input, fetch_output, glob_output, infallible, non_empty_string, parse_pair, read_detail,
    read_input, read_output, search_detail, search_input, shell_detail, shell_input, shell_output,
    strip_read_line_number_gutter, truncate_diff_text, web_search_output, write_detail,
    write_input, write_output,
};

/// `(family, input, output)`: the schema pair and mapper under test.
const DETAIL_CASES: &[(&str, &str, &str)] = &[
    (
        "shell",
        r#"{"command":["  git ","","status "],"directory":"/d","cwd":""}"#,
        r#""Chunk ID: 1\nWall time: 2s\nProcess exited with code 0\nOutput:\nhello""#,
    ),
    (
        "shell",
        r#"{"command":5,"cmd":"ls"}"#,
        r#"{"exitCode":null,"exit_code":3,"result":{"command":"r","text":"t"}}"#,
    ),
    (
        "shell",
        r#"{"cmd":["a",1]}"#,
        r#"{"metadata":{"exitCode":2},"structuredContent":{"text":"s"}}"#,
    ),
    (
        "shell",
        r"null",
        r#"{"command":"from-output","aggregatedOutput":"agg","exitCode":0}"#,
    ),
    ("shell", r#"{"command":"ls","cwd":null}"#, r"null"),
    ("shell", r#"{"command":""}"#, r#"{"metadata":[]}"#),
    (
        "shell",
        r#"{"command":"ls"}"#,
        r#""Chunk ID: 1\nWall time: 2s\nProcess exited with code 0\nOriginal token count: 4\nbody""#,
    ),
    (
        "read",
        r#"{"file_path":"/a.ts","offset":3,"limit":2}"#,
        r#""     3\tconst a = 1;\n     4\tconst b = 2;\n""#,
    ),
    ("read", r#"{"file_path":"/a.ts"}"#, r#""  10\tx\n  12\ty""#),
    (
        "read",
        r#"{"file_path":"/a.ts"}"#,
        r#""1\ta\nplain\nplain2\nplain3""#,
    ),
    (
        "read",
        r#"{"filePath":"  "}"#,
        r#""<path>/x.md</path>\n<content>\nA &amp; B &#65;&#x42; &bogus; &lt;\n</content>""#,
    ),
    (
        "read",
        r#"{"file_path":"/a"}"#,
        r#""<path>/x.md</path><content>&#99999999;</content>""#,
    ),
    (
        "read",
        r#"{"file_path":"/a"}"#,
        r#""<path>/x.md</path><CONTENT>&#x110000;</CONTENT>""#,
    ),
    (
        "read",
        r#"{"file_path":"/a"}"#,
        r#"{"data":{"text":[{"output":"o"},{"text":""}]}}"#,
    ),
    ("read", r#"{"file_path":"/a","offset":"1"}"#, r#""x""#),
    (
        "read",
        r#"{"path":"/p"}"#,
        r#"[{"content":"c1"},{"text":"t2","output":5}]"#,
    ),
    (
        "read",
        r#"{"file_path":"/f"}"#,
        r#""<path>/f</path>\n<content>&#55357;&#56832;&#xD800;</content>""#,
    ),
    (
        "read",
        r#"{"file_path":"/f"}"#,
        r#"{"content":[{"type":"output_text","text":"alpha"},{"type":"output_text","content":"beta"}]}"#,
    ),
    (
        "read",
        r#"{"file_path":"/f"}"#,
        r#"{"structured_content":{"content":{"type":"output_text","text":"gamma"}}}"#,
    ),
    ("read", r"null", r"null"),
    ("write", r#"{"path":"a","filePath":"b"}"#, r"null"),
    (
        "write",
        r#"{"path":"a","filePath":"a","content":"","newContent":"n"}"#,
        r#"{"file_path":"o","content":"oc"}"#,
    ),
    ("write", r"null", r#"{"new_content":"x"}"#),
    ("write", r#"{"path":"a","content":5}"#, r#""ok""#),
    ("write", r#"{"file_path":"w","content":"hello"}"#, r"null"),
    (
        "edit",
        r#"{"file_path":"/f","old_string":"a","new_str":"b","diff":"d"}"#,
        r#"{"files":[{"path":"/g","unified_diff":"u"}]}"#,
    ),
    (
        "edit",
        r"null",
        r#"{"files":[{"filePath":"/g","patch":"p"},{"bad":1}],"content":"c"}"#,
    ),
    (
        "edit",
        r#"{"path":"/f"}"#,
        r#"{"path":"/f2","filePath":"/other"}"#,
    ),
    ("edit", r#"{"file_path":"/f","patch":"PATCH"}"#, r"null"),
    (
        "edit",
        r#"{"path":"/f"}"#,
        r#"{"path":"/f","filePath":"/x"}"#,
    ),
    (
        "edit",
        r#"{"path":"/f","old_content":"o","newContent":"n"}"#,
        r#"{"path":"/f","unifiedDiff":"U"}"#,
    ),
    (
        "web_search",
        r#"{"query":"q"}"#,
        r#"{"query":"q","durationSeconds":1,"results":[{"tool_use_id":"t","content":[{"title":"a","url":"u","__proto__":"pp"}],"__proto__":{"y":1}}]}"#,
    ),
    (
        "web_search",
        r#"{"query":"q"}"#,
        r#"{"query":"q","results":[{"tool_use_id":"t","content":[{"url":"u","extra":1,"title":"T"}],"zzz":0}],"durationSeconds":2,"filenames":["f"]}"#,
    ),
    ("web_search", r#"{"q":"x"}"#, r"null"),
    ("web_search", r#"{"query":"q"}"#, r#"{"results":"none"}"#),
    (
        "glob",
        r#"{"pattern":"*.rs"}"#,
        r#"{"durationMs":3,"numFiles":1,"filenames":["a.rs"],"truncated":false}"#,
    ),
    ("glob", r#"{"pattern":""}"#, r"null"),
    (
        "glob",
        r#"{"pattern":"p"}"#,
        r#"{"durationMs":3,"numFiles":-1,"filenames":["a.rs"],"truncated":false}"#,
    ),
    (
        "fetch",
        r#"{"url":"","prompt":"p"}"#,
        r#"{"bytes":1,"code":-1,"codeText":"","result":"","durationMs":0,"url":"https://o"}"#,
    ),
    ("fetch", r#"{"url":"u"}"#, r"null"),
    (
        "fetch",
        r#"{"url":"https://a","prompt":"p"}"#,
        r#"{"bytes":10,"code":200,"codeText":"OK","result":"body","durationMs":5,"url":"https://a"}"#,
    ),
];

/// Texts for the `tool-call-mapper-utils.ts` helpers, as JSON.
const TEXTS: &[&str] = &[
    r#""""#,
    r#""x""#,
    r#""  padded  ""#,
    r#""Chunk ID: 1\nWall time: 2s\nProcess exited with code 0\nOutput:\nhello""#,
    r#""Chunk ID: 1\r\nWall time: 2s\r\nProcess exited with code 0\r\nOutput:\r\n""#,
    r#""Chunk ID: 1\nWall time: 2s\nProcess exited with code 0\nOriginal token count: 4\nbody\nmore""#,
    r#""Chunk ID: 1\nWall time: 2s\nProcess exited with code 0\n""#,
    r#""chunk id: 1\nwall time: 2s\nplain""#,
    r#""Wall time: 2s\nOutput:\nx""#,
    r#""     3\tconst a = 1;\n     4\tconst b = 2;\n""#,
    r#""  10\tx\n  12\ty""#,
    r#""1\ta\nplain\nplain2\nplain3""#,
    r#""1\ta\n2\tb\n\n3\tc""#,
    r#""1\ta\n2\tb\nplain\nplain""#,
    r#""1\ta\n2\tb\nplain\nplain\nplain""#,
    r#""1\ta\r\n2\tb""#,
    r#""plain\n1\ta""#,
    r#""\n\n7\tx\n8\ty""#,
    r#""\u00a01\tx\n2\ty""#,
    r#""007\tx\n008\ty""#,
    r#""99999999999999999999\tx\n100000000000000000000\ty""#,
    r#""\ud800\n""#,
    r#""\ud83d\ude00 emoji""#,
];

const NODE_SCRIPT: &str = r#"
const [dist, detailJson, textJson, longJson] = process.argv.slice(1);
const base = `${dist}/server/agent/providers`;
const p = await import(`${base}/tool-call-detail-primitives.js`);
const utils = await import(`${base}/tool-call-mapper-utils.js`);
const branch = (inputSchema, outputSchema, mapper) =>
  p.toolDetailBranchByName("x", inputSchema, outputSchema, mapper);
const families = {
  shell: branch(p.ToolShellInputSchema, p.ToolShellOutputSchema, p.toShellToolDetail),
  write: branch(p.ToolWriteInputSchema, p.ToolWriteOutputSchema, p.toWriteToolDetail),
  edit: branch(p.ToolEditInputSchema, p.ToolEditOutputSchema, p.toEditToolDetail),
  web_search: branch(p.ToolSearchInputSchema, p.ToolWebSearchOutputSchema.nullable(), (input, output) =>
    p.toSearchToolDetail({ input, output, toolName: "web_search" })),
  glob: branch(p.ToolSearchInputSchema, p.ToolGlobOutputSchema.nullable(), (input, output) =>
    p.toSearchToolDetail({ input, output, toolName: "glob" })),
  fetch: branch(p.ToolWebFetchInputSchema, p.ToolWebFetchOutputSchema, p.toFetchToolDetail),
};
const fmt = (value) => (value === undefined ? "undefined" : JSON.stringify(value));
const run = (fn) => {
  try {
    return fn();
  } catch (error) {
    return `THROW ${error.name}: ${error.message}`;
  }
};
const detail = (family, input, output) => {
  if (family === "read") {
    // The read branch parses its output separately, as the Claude parser does.
    const parsedInput = p.ToolReadInputSchema.nullable().safeParse(input);
    if (!parsedInput.success) return "REJECT";
    const parsedOutput = p.ToolReadOutputSchema.safeParse(output);
    return fmt(p.toReadToolDetail(parsedInput.data, parsedOutput.success ? parsedOutput.data : null));
  }
  const result = families[family].safeParse({ name: "x", input, output });
  return result.success ? fmt(result.data) : "REJECT";
};
const out = [];
for (const [family, input, output] of JSON.parse(detailJson))
  out.push(run(() => detail(family, JSON.parse(input), JSON.parse(output))));
for (const text of [...JSON.parse(textJson), ...JSON.parse(longJson)]) {
  out.push(fmt(utils.nonEmptyString(text)));
  out.push(fmt(utils.extractCodexShellOutput(text)));
  out.push(fmt(utils.stripReadLineNumberGutter(text)));
  out.push(fmt(utils.truncateDiffText(text)));
}
process.stdout.write(out.join("\n") + "\n");
"#;

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/agent/providers/tool-call-detail-primitives.js",
        "fde3bdc6089e3c4a98f0f641d066cd8302762b1b358fa2a702bc5fa8f0fd8762",
    ),
    (
        "server/agent/providers/tool-call-mapper-utils.js",
        "c2e99061f5fa571579f6384e48330f12384bd559aa0ce975daec4fddabcc4b08",
    ),
];

/// Texts built at run time: `truncateDiffText` at its 12000-unit limit, with
/// a surrogate pair on the cut.
fn long_texts() -> Vec<String> {
    vec![
        "x".repeat(11_999),
        "x".repeat(12_000),
        "x".repeat(12_001),
        "x".repeat(30_000),
        format!("{}\u{1f600}{}", "x".repeat(11_999), "y".repeat(5)),
        format!("{}\u{1f600}{}", "x".repeat(11_998), "y".repeat(5)),
        format!("{}\u{20ac}", "\u{20ac}".repeat(12_000)),
    ]
}

fn texts() -> Vec<JsValue> {
    TEXTS
        .iter()
        .map(|text| parse(text).expect("text JSON"))
        .chain(long_texts().into_iter().map(JsValue::String))
        .collect()
}

fn undefined_or(value: Option<JsValue>) -> String {
    value.map_or_else(|| "undefined".to_owned(), |value| stringify(&value))
}

/// The branch the baseline builds with `toolDetailBranchByName`: both sides
/// are parsed even for a matching name; `None` is `REJECT`.
fn detail(family: &str, input: &JsValue, output: &JsValue) -> Parse<Option<JsValue>> {
    Ok(match family {
        "shell" => parse_pair(
            input,
            output,
            infallible(shell_input),
            infallible(shell_output),
        )?
        .map(|(input, output)| shell_detail(input, output)),
        "read" => match parse_pair(input, output, infallible(read_input), |_: &JsValue| {
            Ok(Some(()))
        })? {
            Some((parsed_input, _)) => {
                let parsed_output = if output.is_null() {
                    None
                } else {
                    read_output(output)?
                };
                Some(read_detail(parsed_input, parsed_output))
            }
            None => None,
        },
        "write" => parse_pair(input, output, write_input, write_output)?
            .map(|(input, output)| write_detail(input, output)),
        "edit" => parse_pair(input, output, edit_input, edit_output)?
            .map(|(input, output)| edit_detail(input, output.as_ref())),
        "web_search" => parse_pair(
            input,
            output,
            infallible(search_input),
            infallible(web_search_output),
        )?
        .map(|(query, output)| search_detail(query, output.as_ref(), "web_search")),
        "glob" => parse_pair(
            input,
            output,
            infallible(search_input),
            infallible(glob_output),
        )?
        .map(|(query, output)| search_detail(query, output.as_ref(), "glob")),
        "fetch" => parse_pair(
            input,
            output,
            infallible(fetch_input),
            infallible(fetch_output),
        )?
        .map(|(input, output)| fetch_detail(input.as_ref(), output.as_ref())),
        other => panic!("unknown family {other}"),
    })
}

fn rust_output() -> String {
    let mut out = Vec::new();
    for (family, input, output) in DETAIL_CASES {
        let input = parse(input).expect("input JSON");
        let output = parse(output).expect("output JSON");
        out.push(match detail(family, &input, &output) {
            Ok(Some(detail)) => undefined_or(detail),
            Ok(None) => "REJECT".to_owned(),
            Err(Thrown { name, message }) => format!("THROW {name}: {message}"),
        });
    }
    for text in texts() {
        let text = text.as_str().expect("text").to_owned();
        out.push(undefined_or(
            non_empty_string(Some(&text)).map(JsValue::String),
        ));
        out.push(undefined_or(
            extract_codex_shell_output(Some(&text)).map(JsValue::String),
        ));
        out.push(undefined_or(
            strip_read_line_number_gutter(Some(&text)).map(|(content, start_line)| {
                let mut stripped = spocky_contracts::js_value::JsObject::new();
                stripped.insert("content", JsValue::String(content));
                if let Some(start_line) = start_line {
                    stripped.insert("startLine", JsValue::Number(start_line));
                }
                JsValue::Object(stripped)
            }),
        ));
        out.push(undefined_or(
            truncate_diff_text(Some(text)).map(JsValue::String),
        ));
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
fn tool_detail_primitives_match_the_pinned_build() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let detail_json = json_list(DETAIL_CASES.iter().map(|(family, input, output)| {
        vec![
            (*family).to_owned(),
            (*input).to_owned(),
            (*output).to_owned(),
        ]
    }));
    let text_json = stringify(&JsValue::Array(
        TEXTS
            .iter()
            .map(|text| parse(text).expect("text JSON"))
            .collect(),
    ));
    let long_json = stringify(&JsValue::Array(
        long_texts().into_iter().map(JsValue::String).collect(),
    ));
    let expected = support::run_node(
        &node,
        &dist,
        NODE_SCRIPT,
        &[detail_json, text_json, long_json],
    );
    let actual = rust_output();
    for (index, (node_line, rust_line)) in expected.lines().zip(actual.lines()).enumerate() {
        assert_eq!(rust_line, node_line, "line {index} differs");
    }
    assert_eq!(actual, expected);
}
