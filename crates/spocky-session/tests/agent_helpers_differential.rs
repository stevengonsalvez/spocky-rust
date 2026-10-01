//! Differential check of small agent helpers against the pinned build:
//! label helpers (`protocol/agent-labels`), `isSystemInjectedEnvelope`,
//! `stripInternalPaseoMcpServer` and `withRuntimePaseoMcpServer`, and
//! `commandMayHaveChangedExternalState`.
//!
//! Needs `SPOCKY_PINNED_NODE` and `SPOCKY_PASEO_DIST` like
//! `checkout_differential`; without them the test FAILS unless
//! `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::process::Command;

use spocky_session::agent_labels::{
    has_open_agent_tab, is_delegated_agent, parent_agent_id_from_labels,
};
use spocky_session::agent_prompt::is_system_injected_envelope;
use spocky_session::external_state::command_may_have_changed_external_state;
use spocky_session::runtime_mcp_config::{
    strip_internal_paseo_mcp_server, with_runtime_paseo_mcp_server,
};
use spocky_store::js_value::{JsObject, JsValue, parse, stringify};

const LABELS: &str = r#"[
  {"paseo.parent-agent-id":"  p1  "},
  {"paseo.parent-agent-id":"   "},
  {"paseo.parent-agent-id":5},
  null,
  {},
  {"paseo.open-agent-tab.c1":"true"},
  {"paseo.open-agent-tab.c1":"false","x":"true"},
  {"paseo.open-agent-tabx":"true","paseo.open-agent-tab.":"true"}
]"#;

const ENVELOPES: &str = r#"[
  "<paseo-system>\n\n</paseo-system>",
  "<paseo-system>\n</paseo-system>",
  "<paseo-system>\nhi\n</paseo-system>\n",
  "x<paseo-system>\na\n</paseo-system>",
  "<paseo-system>\r\na\n</paseo-system>",
  "<paseo-system>\na\nb\r\n\n</paseo-system>",
  ""
]"#;

/// `[config, agentId, mcpBaseUrl, mcpAuthToken]`.
const MCP: &str = r#"[
  [{"provider":"codex","cwd":"/w"}, "a1", "http://127.0.0.1:7/mcp/agents", "tok"],
  [{"provider":"codex","cwd":"/w"}, "a1", null, "tok"],
  [{"provider":"codex","cwd":"/w","mcpServers":{"paseo":{"type":"http","url":"http://h:1/mcp/agents?callerAgentId=x"},"z":{"type":"stdio","command":"c"},"1":{"type":"stdio","command":"n"}},"model":"m"}, "a2", "http://h:2/mcp/agents", null],
  [{"provider":"codex","cwd":"/w","mcpServers":{"paseo":{"type":"sse","url":"http://h/mcp/agents"}}}, "a3", "", null],
  [{"provider":"codex","cwd":"/w","mcpServers":{"paseo":{"type":"stdio","command":"c"}}}, "a4", "http://h/mcp/agents", "t"],
  [{"provider":"codex","cwd":"/w","mcpServers":{"paseo":{"type":"http","url":"http://h/other"}}}, "a5", "http://h/mcp/agents", ""],
  [{"provider":"codex","cwd":"/w","mcpServers":{"paseo":null,"q":{"type":"stdio","command":"c"}}}, "a6", "http://h/mcp/agents", "t"],
  [{"provider":"codex","cwd":"/w","mcpServers":{"paseo":{"type":"http","url":"not a url"}}}, "a7", null, null],
  [{"provider":"codex","cwd":"/w","mcpServers":{"paseo":{"type":"http","url":["http://h/mcp/agents"]}}}, "a8", null, null],
  [{"provider":"codex","cwd":"/w","mcpServers":{}}, "a9", "http://h/mcp/agents", null]
]"#;

const COMMANDS: &str = r#"[
  "gh pr merge 12", "GH  PR\tCREATE --fill", "gh pr view 1", "xgh pr merge", "gh pr merged",
  "git push origin main", "git  fetch", "git pushx", "cd x && git push", "agit push",
  "git push", "git_push", "echo 'git fetch'", "gh pr comment_x", "git\npush", ""
]"#;

const NODE_SCRIPT: &str = r"
const [dist, labelsJson, envelopesJson, mcpJson, commandsJson] = process.argv.slice(1);
const labelsModule = await import(`${dist}/../../../protocol/dist/agent-labels.js`);
const { isSystemInjectedEnvelope } = await import(`${dist}/server/agent/agent-prompt.js`);
const { stripInternalPaseoMcpServer, withRuntimePaseoMcpServer } = await import(`${dist}/server/agent/runtime-mcp-config.js`);
const { commandMayHaveChangedExternalState } = await import(`${dist}/server/agent/agent-manager.js`);
const labels = JSON.parse(labelsJson).map((value) => [
  labelsModule.getParentAgentIdFromLabels(value),
  labelsModule.isDelegatedAgent({ labels: value }),
  labelsModule.hasOpenAgentTab(value),
]);
const envelopes = JSON.parse(envelopesJson).map(isSystemInjectedEnvelope);
const mcp = JSON.parse(mcpJson).map(([config, agentId, mcpBaseUrl, mcpAuthToken]) => [
  stripInternalPaseoMcpServer(config),
  withRuntimePaseoMcpServer({ config, agentId, mcpBaseUrl, mcpAuthToken }),
]);
const commands = JSON.parse(commandsJson).map(commandMayHaveChangedExternalState);
process.stdout.write(JSON.stringify({ labels, envelopes, mcp, commands }));
";

fn list(text: &str) -> Vec<JsValue> {
    parse(text)
        .expect("fixture")
        .as_array()
        .expect("array")
        .to_vec()
}

fn rust_output() -> String {
    let labels = list(LABELS)
        .iter()
        .map(|value| {
            let labels = Some(value);
            JsValue::Array(vec![
                parent_agent_id_from_labels(labels).map_or(JsValue::Null, JsValue::String),
                JsValue::Bool(is_delegated_agent(labels)),
                JsValue::Bool(has_open_agent_tab(labels)),
            ])
        })
        .collect();
    let envelopes = list(ENVELOPES)
        .iter()
        .map(|text| JsValue::Bool(is_system_injected_envelope(text.as_str().expect("text"))))
        .collect();
    let mcp = list(MCP)
        .iter()
        .map(|case| {
            let case = case.as_array().expect("case");
            JsValue::Array(vec![
                strip_internal_paseo_mcp_server(&case[0]),
                with_runtime_paseo_mcp_server(
                    &case[0],
                    case[1].as_str().expect("agent id"),
                    case[2].as_str(),
                    case[3].as_str(),
                ),
            ])
        })
        .collect();
    let commands = list(COMMANDS)
        .iter()
        .map(|command| {
            JsValue::Bool(command_may_have_changed_external_state(
                command.as_str().expect("command"),
            ))
        })
        .collect();
    let mut output = JsObject::new();
    output.insert("labels", JsValue::Array(labels));
    output.insert("envelopes", JsValue::Array(envelopes));
    output.insert("mcp", JsValue::Array(mcp));
    output.insert("commands", JsValue::Array(commands));
    stringify(&JsValue::Object(output))
}

/// The pinned dist modules this test runs, relative to `SPOCKY_PASEO_DIST`,
/// with their SHA-256: a different build fails instead of silently passing.
const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "../../../protocol/dist/agent-labels.js",
        "45eb1cdeeef92dbe391138dceb148b411a71ac22b4eac3ffc17207d36240fbdf",
    ),
    (
        "server/agent/agent-prompt.js",
        "a4d7a19d6f82cb4932d03db441a2bcf31fbe0b5fb506faf73e80b676e7a96f12",
    ),
    (
        "server/agent/runtime-mcp-config.js",
        "2d4ec30a925c3a247409b9acd584cb48437b1e355c1a75b9b50cf69983f68aef",
    ),
    (
        "server/agent/agent-manager.js",
        "09e1a170a75fc6b1f4eca29779feb7ada0c6d607bd33588f545e619174d7fa65",
    ),
];

fn assert_pinned_modules(dist: &std::ffi::OsStr) {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    for (path, expected) in PINNED_MODULES {
        let bytes = std::fs::read(std::path::Path::new(dist).join(path)).expect("pinned module");
        let actual = Sha256::digest(&bytes)
            .iter()
            .fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            });
        assert_eq!(&actual, expected, "{path} is not the pinned build");
    }
}

#[test]
fn helpers_match_pinned_modules() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: helper differential not run");
            return;
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    };
    assert_pinned_modules(&dist);
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
        .args([LABELS, ENVELOPES, MCP, COMMANDS])
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(rust_output(), String::from_utf8_lossy(&output.stdout));
}
