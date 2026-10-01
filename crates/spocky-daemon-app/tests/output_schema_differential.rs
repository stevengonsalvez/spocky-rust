//! Differential check of `normalize_codex_output_schema` against the pinned
//! build's `normalizeCodexOutputSchema` (`codex-app-server-agent.js`): the
//! normalized schema, key order included, or the thrown message.
//!
//! Needs `SPOCKY_PINNED_NODE` (Node v22.20.0, asserted) and
//! `SPOCKY_PASEO_DIST` (the pinned `packages/server/dist/server`), as the session lane's differentials do;
//! without them the test FAILS unless `SPOCKY_ALLOW_SKIP=1` (exactly).

use std::process::Command;

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_daemon_app::codex_agent::normalize_codex_output_schema;

const CASES: &str = r#"[
  {"type":"object","properties":{"findings":{"type":"array","items":{"type":"object","properties":{"severity":{"type":"string"},"summary":{"type":"string"}},"required":["severity"]}},"overall":{"type":"string"}},"required":["overall"]},
  {"type":"object","properties":{"properties":{"type":"string"}},"required":["properties"],"additionalProperties":false},
  {"type":"object","properties":{"a":{"const":{"type":"object","properties":{"x":1}}},"b":{"enum":[{"type":"object"}]}},"$defs":{"d":{"type":"object","properties":{"z":{"type":"number"}}}},"definitions":{"e":{"properties":{"q":{}}}}},
  {"type":"object","properties":{"u":{"anyOf":[{"type":"object","properties":{"k":{"type":"string"}}},{"type":"null"}]},"t":{"type":"array","prefixItems":[{"type":"object"},true]},"n":{"not":{"type":"object"}},"c":{"if":{"type":"object"},"then":{"properties":{"p":{}}},"else":{"type":"string"}}}},
  {"type":["object","null"],"patternProperties":{"^x":{"type":"object"}},"dependentSchemas":{"a":{"type":"object","properties":{"b":{}}}}},
  {"type":"object","properties":{"1":{},"0":{},"b":{},"a":{}},"required":["a",1,"a",null,"z"]},
  {"type":"object","properties":[{"type":"string"}]},
  {"type":"object","additionalProperties":true},
  {"type":"object","properties":{"a":{"type":"object","additionalProperties":{"type":"string"}}}},
  {"type":"object","properties":{"list":{"type":"array","items":[{"type":"object","additionalProperties":1}]}}},
  {"type":"string"},
  {"properties":{}},
  {"oneOf":[{"type":"object"}]},
  [],
  "schema",
  null,
  7
]"#;

const NODE_SCRIPT: &str = r"
if (process.version !== `v22.20.0`) {
  throw new Error(`pinned node is v22.20.0, got ${process.version}`);
}
const [dist, casesJson] = process.argv.slice(1);
const { normalizeCodexOutputSchema } = await import(`${dist}/server/agent/providers/codex-app-server-agent.js`);
const out = JSON.parse(casesJson).map((schema) => {
  try {
    return { ok: normalizeCodexOutputSchema(schema) };
  } catch (error) {
    return { error: error.message };
  }
});
process.stdout.write(JSON.stringify(out));
";

fn rust_output() -> String {
    let cases = parse(CASES).expect("cases");
    let results = cases
        .as_array()
        .expect("case list")
        .iter()
        .map(|schema| {
            let mut result = JsObject::new();
            match normalize_codex_output_schema(schema) {
                Ok(normalized) => result.insert("ok", normalized),
                Err(message) => result.insert("error", JsValue::String(message)),
            }
            JsValue::Object(result)
        })
        .collect();
    stringify(&JsValue::Array(results))
}

/// The pinned dist module this test runs, relative to `SPOCKY_PASEO_DIST`,
/// with its SHA-256: a different build fails instead of silently passing.
const PINNED_MODULE: (&str, &str) = (
    "server/agent/providers/codex-app-server-agent.js",
    "719fa69764903ea2073086e1f6a5af43b4962d3ffb08f5896132c840a6a75d2e",
);

fn assert_pinned_module(dist: &std::ffi::OsStr) {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let (path, expected) = PINNED_MODULE;
    let bytes = std::fs::read(std::path::Path::new(dist).join(path)).expect("pinned module");
    let actual = Sha256::digest(&bytes)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    assert_eq!(actual, expected, "{path} is not the pinned build");
}

#[test]
fn output_schema_normalization_matches_the_pinned_module() {
    let (node, dist) = match (
        std::env::var_os("SPOCKY_PINNED_NODE"),
        std::env::var_os("SPOCKY_PASEO_DIST"),
    ) {
        (Some(node), Some(dist)) => (node, dist),
        _ if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") => {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: outputSchema differential not run");
            return;
        }
        _ => panic!("set SPOCKY_PINNED_NODE and SPOCKY_PASEO_DIST (or SPOCKY_ALLOW_SKIP=1)"),
    };
    assert_pinned_module(&dist);
    let output = Command::new("gtimeout")
        .args(["--kill-after=5", "120"])
        .arg(&node)
        .args(["--input-type=module", "-e", NODE_SCRIPT])
        .arg(&dist)
        .arg(CASES)
        .output()
        .expect("run pinned node");
    assert!(
        output.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(rust_output(), String::from_utf8_lossy(&output.stdout));
}
