//! Differential check of the model catalog, model id normalization, version
//! parsing, thinking capabilities, fast mode features, and `settings.json`
//! models against the pinned build.

mod support;

use std::path::{Path, PathBuf};

use spocky_contracts::js_value::{JsObject, JsValue, parse, stringify};
use spocky_provider_claude::model_manifest::{
    claude_manifest_model_supports_fast_mode, normalize_claude_manifest_model_id,
    normalize_claude_runtime_model_id, parse_claude_code_version,
    resolve_claude_disabled_thinking_for_model,
};
use spocky_provider_claude::models::{
    build_claude_features, find_claude_model, get_claude_models, get_claude_models_with_settings,
    resolve_configured_claude_model, resolve_observed_claude_model_id,
};

/// Model id inputs, from `models.test.ts` and edge cases; `null` is
/// written as the JSON value.
const MODEL_IDS: &[&str] = &[
    "null",
    r#""""#,
    r#""  ""#,
    r#""claude-opus-5""#,
    r#""claude-fable-5""#,
    r#""claude-fable-5[1m]""#,
    r#""claude-sonnet-5[1m]""#,
    r#""claude-opus-4-6[1m]""#,
    r#""claude-haiku-4-5""#,
    r#""claude-opus-5-20260724""#,
    r#""claude-fable-5-20260301[1m]""#,
    r#""claude-sonnet-5-20260101[1m]""#,
    r#""claude-haiku-4-5-20251001""#,
    r#""gpt-5""#,
    r#""random""#,
    r#""openrouter/anthropic/claude-opus-4-8""#,
    r#""us.anthropic.claude-opus-4-8[1m]""#,
    r#""us.anthropic.claude-opus-4-8-20260101""#,
    r#""anthropic/claude-opus-5-5""#,
    r#""us.anthropic.claude-opus-5-5-20260401-v1:0""#,
    r#""openrouter/anthropic/claude-fable-5-1""#,
    r#""us.anthropic.claude-opus-5-20260724-v1:0""#,
    r#""anthropic/claude-sonnet-5-5""#,
    r#""us.anthropic.claude-sonnet-5-5-20260928-v1:0""#,
    r#""us.anthropic.claude-sonnet-5-20260101-v1:0""#,
    r#""Opus 4.8""#,
    r#""CLAUDE_OPUS__4.7[1M]""#,
    r#""opus-5""#,
    r#""Fable-5-1-20260101""#,
    r#""claude-opus-4-8-2026010""#,
    r#""claude-opus-4-8-202601011""#,
    r#""claude opus 4 6 20260101 [1m]""#,
    r#""xclaude-opus-4-6""#,
    r#""claude-claude-opus-4-6""#,
    r#""claude-opus-4.6""#,
    r#""claude-sonnet-4-6[1m][1m]""#,
    r#""  claude-sonnet-4-6  ""#,
    r#""claude-opus-04-06""#,
    r#""<synthetic>""#,
    r#""glm-4.6""#,
    r#""claude-opus-5-5[1m]""#,
];

const VERSIONS: &[&str] = &[
    "wrapper 1.0.0\n2.1.219 (Claude Code)",
    "2.1.287 (Claude Code)",
    "2.1.287 (claude code)",
    "v2.1.0",
    "a2.1.0 2.0.9",
    "2.1.0x 3.0.0",
    "2.1",
    "01.002.0003\u{a0}(CLAUDE CODE)",
    "",
];

/// `(name, settings.json bytes or absent)` scenarios for the catalog.
const SETTINGS: &[(&str, Option<&str>)] = &[
    ("missing", None),
    ("malformed", Some("{not json")),
    ("bom", Some("\u{feff}{\"model\":\"bom-model\"}")),
    ("array", Some("[1]")),
    (
        "full",
        Some(
            r#"{"model":"  my-model  ","env":{"ANTHROPIC_MODEL":"my-model","ANTHROPIC_DEFAULT_OPUS_MODEL":"claude-opus-4-8","ANTHROPIC_SMALL_FAST_MODEL":"","ANTHROPIC_DEFAULT_HAIKU_MODEL":7,"OTHER":"x"}}"#,
        ),
    ),
    ("env-not-object", Some(r#"{"model":"m","env":"x"}"#)),
    ("fable-alias", Some(r#"{"model":"claude-fable-5[1m]"}"#)),
];

/// `resolveConfiguredClaudeModel` inputs.
const CONFIGURED: &[&str] = &[
    r#"{"provider":"claude","id":"claude-opus-4-8","label":"x"}"#,
    r#"{"provider":"claude","id":"claude-haiku-4-5","label":"x"}"#,
    r#"{"provider":"claude","id":"custom","label":"x"}"#,
    r#"{"provider":"claude","id":"custom","label":"x","thinkingOptions":null}"#,
    r#"{"provider":"claude","id":"us.anthropic.claude-opus-4-8","label":"x"}"#,
    // A defined thinkingOptions returns the model untouched.
    r#"{"provider":"claude","id":"claude-opus-4-8","label":"x","thinkingOptions":[{"id":"low","label":"Low"}]}"#,
    r#"{"provider":"claude","id":"custom","label":"x","thinkingOptions":[]}"#,
    r#"{"provider":"claude","id":"claude-haiku-4-5","label":"x","thinkingOptions":[{"id":"off","label":"Off","isDefault":true}],"extra":1}"#,
];

const CLAUDE_CODE_VERSIONS: &[&str] = &[
    "null",
    r#""2.1.168""#,
    r#""2.1.219""#,
    r#""2.1.284""#,
    r#""bad""#,
];

const NODE_SCRIPT: &str = r#"
const [dist, idsJson, versionsJson, settingsJson, configuredJson, ccJson] = process.argv.slice(1);
const base = `${dist}/server/agent/providers/claude`;
const manifest = await import(`${base}/model-manifest.js`);
const models = await import(`${base}/models.js`);
const features = await import(`${base}/feature-definitions.js`);
const logger = { debug() {}, warn() {}, info() {}, error() {} };
const out = [];
const put = (value) => out.push(value === undefined ? "undefined" : JSON.stringify(value));
for (const id of JSON.parse(idsJson).map((text) => JSON.parse(text))) {
  put(manifest.normalizeClaudeManifestModelId(id));
  put(manifest.normalizeClaudeRuntimeModelId(id));
  put(manifest.resolveClaudeDisabledThinkingForModel(id));
  put(manifest.claudeManifestModelSupportsFastMode(id));
  put(models.findClaudeModel(id) ?? null);
  put(models.resolveObservedClaudeModelId(id));
  put(features.buildClaudeFeatures({ modelId: id, fastModeEnabled: true }));
  put(features.buildClaudeFeatures({ modelId: id, fastModeEnabled: false }));
}
for (const version of JSON.parse(versionsJson)) put(manifest.parseClaudeCodeVersion(version));
for (const version of JSON.parse(ccJson).map((text) => JSON.parse(text) ?? undefined)) {
  put(models.getClaudeModels(version));
}
for (const dir of JSON.parse(settingsJson)) {
  put(await models.getClaudeModelsWithSettings(logger, dir, undefined));
  put(await models.getClaudeModelsWithSettings(logger, dir, "2.1.200"));
}
for (const model of JSON.parse(configuredJson)) put(models.resolveConfiguredClaudeModel(JSON.parse(model)));
process.stdout.write(out.join("\n") + "\n");
"#;

const PINNED_MODULES: &[(&str, &str)] = &[
    (
        "server/agent/providers/claude/models.js",
        "b1b0c2017f73b016eb980ee6748dbb5df61ec9591cd484c1e7e22c2eebefac03",
    ),
    (
        "server/agent/providers/claude/model-manifest.js",
        "9c23d9112ad98b7ee3a789fd30ee524f9f3f54b06fe4c7a887b263ad2b1311d1",
    ),
    (
        "server/agent/providers/claude/feature-definitions.js",
        "f847c3459c078daacd9bd456ea6ee9eff1a2eca541274e464590acb0f6191baf",
    ),
];

fn put(out: &mut Vec<String>, value: &JsValue) {
    out.push(stringify(value));
}

fn string_or_null(value: Option<String>) -> JsValue {
    value.map_or(JsValue::Null, JsValue::String)
}

fn array(items: Vec<JsValue>) -> JsValue {
    JsValue::Array(items)
}

fn rust_output(settings_dirs: &[PathBuf]) -> String {
    let mut out = Vec::new();
    for id in MODEL_IDS {
        let id = parse(id).expect("id JSON");
        let id = id.as_str();
        put(
            &mut out,
            &string_or_null(normalize_claude_manifest_model_id(id)),
        );
        put(
            &mut out,
            &string_or_null(normalize_claude_runtime_model_id(id)),
        );
        let (supported, fallback) = resolve_claude_disabled_thinking_for_model(id);
        let mut resolution = JsObject::new();
        resolution.insert("supported", JsValue::Bool(supported));
        resolution.insert(
            "fallbackThinkingOptionId",
            fallback.map_or(JsValue::Undefined, |id| JsValue::String(id.to_owned())),
        );
        put(&mut out, &JsValue::Object(resolution));
        put(
            &mut out,
            &JsValue::Bool(claude_manifest_model_supports_fast_mode(id)),
        );
        put(&mut out, &find_claude_model(id).unwrap_or(JsValue::Null));
        put(
            &mut out,
            &string_or_null(resolve_observed_claude_model_id(id)),
        );
        put(&mut out, &array(build_claude_features(id, true)));
        put(&mut out, &array(build_claude_features(id, false)));
    }
    for version in VERSIONS {
        put(
            &mut out,
            &parse_claude_code_version(version).map_or(JsValue::Null, |parts| {
                array(parts.iter().map(|part| JsValue::Number(*part)).collect())
            }),
        );
    }
    for version in CLAUDE_CODE_VERSIONS {
        let version = parse(version).expect("version JSON");
        put(&mut out, &array(get_claude_models(version.as_str())));
    }
    for dir in settings_dirs {
        put(&mut out, &array(get_claude_models_with_settings(dir, None)));
        put(
            &mut out,
            &array(get_claude_models_with_settings(dir, Some("2.1.200"))),
        );
    }
    for model in CONFIGURED {
        put(
            &mut out,
            &resolve_configured_claude_model(&parse(model).expect("model JSON")),
        );
    }
    out.join("\n") + "\n"
}

fn write_settings(root: &Path) -> Vec<PathBuf> {
    SETTINGS
        .iter()
        .map(|(name, contents)| {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).expect("settings dir");
            if let Some(contents) = contents {
                std::fs::write(dir.join("settings.json"), contents).expect("settings.json");
            }
            dir
        })
        .collect()
}

fn json_strings(items: &[&str]) -> String {
    stringify(&array(
        items
            .iter()
            .map(|item| JsValue::String((*item).to_owned()))
            .collect(),
    ))
}

#[test]
fn models_match_the_pinned_catalog() {
    let Some((node, dist)) = support::pinned() else {
        return;
    };
    support::assert_pinned_modules(&dist, PINNED_MODULES);
    let root = std::env::temp_dir().join(format!(
        "spocky-claude-models-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let dirs = write_settings(&root);
    let dir_args: Vec<String> = dirs
        .iter()
        .map(|dir| dir.to_string_lossy().into_owned())
        .collect();
    let dir_list: Vec<&str> = dir_args.iter().map(String::as_str).collect();
    let expected = support::run_node(
        &node,
        &dist,
        NODE_SCRIPT,
        &[
            json_strings(MODEL_IDS),
            json_strings(VERSIONS),
            json_strings(&dir_list),
            json_strings(CONFIGURED),
            json_strings(CLAUDE_CODE_VERSIONS),
        ],
    );
    let actual = rust_output(&dirs);
    std::fs::remove_dir_all(&root).expect("remove disposable settings root");
    for (index, (node_line, rust_line)) in expected.lines().zip(actual.lines()).enumerate() {
        assert_eq!(rust_line, node_line, "line {index} differs");
    }
    assert_eq!(actual, expected);
}
