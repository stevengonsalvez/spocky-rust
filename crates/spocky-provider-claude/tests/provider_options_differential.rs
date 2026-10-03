//! Differential check of the Claude provider options schema against the
//! pinned build: every input must print the same parsed `JSON.stringify` text,
//! or the same `ZodError` message, in both.

// Every case is written the same way: raw, so the JSON needs no escapes.
#![allow(clippy::needless_raw_string_hashes)]

mod support;

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_provider_claude::provider_options::parse_claude_provider_options;

/// JSON text of each input; `undefined` is the one non-JSON input.
const CASES: &[&str] = &[
    r#"undefined"#,
    r#"null"#,
    r#"{}"#,
    r#""""#,
    r#""x""#,
    r#"1"#,
    r#"true"#,
    r#"[]"#,
    r#"[1]"#,
    r#"{"allowedTools":["Read","Bash"]}"#,
    r#"{"allowedTools":"Read"}"#,
    r#"{"allowedTools":[1]}"#,
    r#"{"allowedTools":[null]}"#,
    r#"{"allowedTools":[]}"#,
    r#"{"disallowedTools":["a"],"additionalDirectories":["/x"]}"#,
    r#"{"additionalDirectories":[["x"]]}"#,
    r#"{"extraArgs":{"a":"b","c":null}}"#,
    r#"{"extraArgs":{"a":1}}"#,
    r#"{"extraArgs":[]}"#,
    r#"{"extraArgs":null}"#,
    r#"{"extraArgs":{"__proto__":"x"}}"#,
    r#"{"extraArgs":{"__proto__":{"a":1}}}"#,
    r#"{"extraArgs":{"":"x"}}"#,
    r#"{"unknown":1}"#,
    r#"{"unknown":1,"other":2}"#,
    r#"{"allowedTools":[1],"unknown":1}"#,
    r#"{"sandbox":{"enabled":true}}"#,
    r#"{"sandbox":{"enabled":"yes"}}"#,
    r#"{"sandbox":{"extra":1}}"#,
    r#"{"sandbox":null}"#,
    r#"{"sandbox":{"network":{"httpProxyPort":8080}}}"#,
    r#"{"sandbox":{"network":{"httpProxyPort":0}}}"#,
    r#"{"sandbox":{"network":{"httpProxyPort":1.5}}}"#,
    r#"{"sandbox":{"network":{"socksProxyPort":-1}}}"#,
    r#"{"sandbox":{"network":{"tlsTerminate":{"caCertPath":"/a","caKeyPath":"/b"}}}}"#,
    r#"{"sandbox":{"network":{"tlsTerminate":{"x":1}}}}"#,
    r#"{"sandbox":{"network":{"allowedDomains":["a.com"],"deniedDomains":[2]}}}"#,
    r#"{"sandbox":{"filesystem":{"allowWrite":["/a"],"disabled":false}}}"#,
    r#"{"sandbox":{"filesystem":{"disabled":"no"}}}"#,
    r#"{"sandbox":{"ignoreViolations":{"a":["b"]}}}"#,
    r#"{"sandbox":{"ignoreViolations":{"a":"b"}}}"#,
    r#"{"sandbox":{"ignoreViolations":{"__proto__":["b"]}}}"#,
    r#"{"sandbox":{"ripgrep":{"command":"rg"}}}"#,
    r#"{"sandbox":{"ripgrep":{"command":"rg","args":["-n"]}}}"#,
    r#"{"sandbox":{"ripgrep":{"args":["-n"]}}}"#,
    r#"{"sandbox":{"ripgrep":{"command":1}}}"#,
    r#"{"sandbox":{"ripgrep":{"command":"rg","z":1}}}"#,
    r#"{"settings":{"permissions":{"allow":["Read"],"ask":["Bash"],"deny":["Write"]}}}"#,
    r#"{"settings":{"permissions":{"allow":"Read"}}}"#,
    r#"{"settings":{"permissions":{"other":[]}}}"#,
    r#"{"settings":{"sandbox":{"enabled":true,"network":{"httpProxyPort":9}}}}"#,
    r#"{"settings":{"sandbox":{"ripgrep":{"command":"rg"}}}}"#,
    r#"{"settings":{"other":1}}"#,
    r#"{"settings":"x"}"#,
    r#"{"settings":null}"#,
    r#"{"sandbox":{"enabled":1,"network":{"httpProxyPort":"a"}},"allowedTools":3}"#,
    r#"{"settings":{"permissions":{"allow":[1,2,3]}}}"#,
    r#"{"allowedTools":["a"],"disallowedTools":["b"],"additionalDirectories":["c"],"extraArgs":{"d":"e"},"sandbox":{"enabled":true},"settings":{"permissions":{"deny":["f"]}}}"#,
];

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/agent/providers/claude/options.js",
        "3f63e785c60e02b8e094487807e34143c4dd8c331b0c5d58b33e09dbf0ce1791",
    ),
    (
        "../../../../node_modules/zod/package.json",
        "c630bd10b52dcf71c112a2bf78dbf2734b9db58d62de663b8d86c2ec2c8cda2e",
    ),
];

const NODE_SCRIPT: &str = r#"
import { readFileSync } from "node:fs";
const [dist, file] = process.argv.slice(1);
const { ClaudeProviderOptionsSchema } = await import(`${dist}/server/agent/providers/claude/options.js`);
const cases = JSON.parse(readFileSync(file, "utf8"));
const lines = [];
for (const text of cases) {
  const input = text === "undefined" ? undefined : JSON.parse(text);
  try {
    lines.push(JSON.stringify({ ok: ClaudeProviderOptionsSchema.parse(input) }));
  } catch (error) {
    lines.push(JSON.stringify({ error: error instanceof Error ? error.message : String(error) }));
  }
}
process.stdout.write(lines.join("\n") + "\n");
"#;

fn rust_line(text: &str) -> String {
    let input = if text == "undefined" {
        JsValue::Undefined
    } else {
        parse(text).expect("case JSON")
    };
    let mut object = JsObject::new();
    match parse_claude_provider_options(&input) {
        Ok(value) => object.insert("ok", value),
        Err(message) => object.insert("error", JsValue::String(message)),
    }
    stringify(&JsValue::Object(object))
}

#[test]
fn provider_options_match_the_pinned_schema() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let zod = std::fs::read_to_string(dist.join("../../../../node_modules/zod/package.json"))
        .expect("pinned zod package.json");
    assert!(zod.contains(r#""version": "4.4.3""#), "zod is not 4.4.3");
    let file =
        std::env::temp_dir().join(format!("spocky-options-cases-{}.json", std::process::id()));
    let list = CASES
        .iter()
        .map(|case| stringify(&JsValue::String((*case).to_owned())))
        .collect::<Vec<_>>()
        .join(",");
    std::fs::write(&file, format!("[{list}]")).expect("cases file");
    let output = support::run_node(
        &node,
        &dist,
        NODE_SCRIPT,
        &[file.to_string_lossy().into_owned()],
    );
    let _ = std::fs::remove_file(&file);
    let expected: Vec<&str> = output.lines().collect();
    assert_eq!(
        expected.len(),
        CASES.len(),
        "node printed one line per case"
    );
    let mut failures = Vec::new();
    for (case, node_line) in CASES.iter().zip(&expected) {
        let rust = rust_line(case);
        if &rust != node_line {
            failures.push(format!("{case}\n  node: {node_line}\n  rust: {rust}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} cases differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
