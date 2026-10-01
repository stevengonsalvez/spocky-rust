//! `models.ts` and `feature-definitions.ts`: the Claude model catalog with
//! `settings.json` models, configured-model resolution, and the fast mode
//! feature.

use std::path::Path;

use spocky_contracts::js_value::{JsObject, JsValue, parse};
use spocky_contracts::text::js_trim;

use crate::model_manifest::{
    claude_manifest_model_supports_fast_mode, get_claude_custom_model_thinking_options,
    get_claude_manifest_models, normalize_claude_manifest_model_id,
    normalize_claude_runtime_model_id,
};

const CLAUDE_SETTINGS_MODEL_ENV_KEYS: [&str; 6] = [
    "ANTHROPIC_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "ANTHROPIC_DEFAULT_FABLE_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
];

/// `getClaudeModels(claudeCodeVersion)`.
#[must_use]
pub fn get_claude_models(claude_code_version: Option<&str>) -> Vec<JsValue> {
    get_claude_manifest_models(claude_code_version)
}

fn with_thinking_options(model: &JsObject, options: JsValue) -> JsValue {
    let mut resolved = model.clone();
    resolved.insert("thinkingOptions", options);
    JsValue::Object(resolved)
}

/// `resolveConfiguredClaudeModel(model)` over an `AgentModelDefinition`.
#[must_use]
pub fn resolve_configured_claude_model(model: &JsValue) -> JsValue {
    let Some(record) = model.as_object() else {
        return model.clone();
    };
    if record
        .get("thinkingOptions")
        .is_some_and(|options| !matches!(options, JsValue::Undefined))
    {
        return model.clone();
    }
    let manifest_id =
        normalize_claude_manifest_model_id(record.get("id").and_then(JsValue::as_str));
    let manifest_model = manifest_id.and_then(|id| {
        get_claude_models(None)
            .into_iter()
            .find(|candidate| candidate.get("id").and_then(JsValue::as_str) == Some(&id))
    });
    if let Some(manifest_model) = manifest_model {
        return match manifest_model.get("thinkingOptions") {
            Some(options) => with_thinking_options(record, options.clone()),
            None => model.clone(),
        };
    }
    with_thinking_options(
        record,
        JsValue::Array(get_claude_custom_model_thinking_options()),
    )
}

/// `findClaudeModel(modelId)`.
#[must_use]
pub fn find_claude_model(model_id: Option<&str>) -> Option<JsValue> {
    let normalized = normalize_claude_runtime_model_id(model_id)?;
    get_claude_models(None)
        .into_iter()
        .find(|model| model.get("id").and_then(JsValue::as_str) == Some(&normalized))
}

/// The `contextWindowMaxTokens` of [`find_claude_model`].
#[must_use]
pub fn find_claude_model_context_window(model_id: Option<&str>) -> Option<f64> {
    find_claude_model(model_id)?
        .get("contextWindowMaxTokens")
        .and_then(JsValue::as_f64)
}

/// `getClaudeModelsWithSettings(logger, configDir, claudeCodeVersion)`.
#[must_use]
pub fn get_claude_models_with_settings(
    config_dir: &Path,
    claude_code_version: Option<&str>,
) -> Vec<JsValue> {
    let mut models = get_claude_models(claude_code_version);
    for model in read_claude_settings_models(config_dir) {
        let id = model.get("id").and_then(JsValue::as_str);
        let existing = models
            .iter()
            .position(|candidate| candidate.get("id").and_then(JsValue::as_str) == id);
        match existing {
            Some(index) => {
                let existing = &models[index];
                if existing.get("isSelectable") == Some(&JsValue::Bool(false)) {
                    let mut merged = existing.as_object().cloned().unwrap_or_default();
                    for (key, value) in model.as_object().into_iter().flat_map(JsObject::iter) {
                        merged.insert(key, value.clone());
                    }
                    merged.insert("isSelectable", JsValue::Bool(true));
                    models[index] = JsValue::Object(merged);
                }
            }
            None => models.push(model),
        }
    }
    models
}

fn read_claude_settings_models(config_dir: &Path) -> Vec<JsValue> {
    let settings_path = config_dir.join("settings.json");
    let Ok(bytes) = std::fs::read(settings_path) else {
        return Vec::new();
    };
    // `fs.readFile(path, "utf8")` replaces invalid UTF-8 and keeps a BOM,
    // which `JSON.parse` then rejects.
    let Ok(parsed) = parse(&String::from_utf8_lossy(&bytes)) else {
        return Vec::new();
    };
    let Some(settings) = parsed.as_object() else {
        return Vec::new();
    };
    let mut models = Vec::new();
    add_settings_model(&mut models, settings.get("model"), "model");
    let Some(env) = settings.get("env") else {
        return models;
    };
    let Some(env) = env.as_object() else {
        return models;
    };
    for key in CLAUDE_SETTINGS_MODEL_ENV_KEYS {
        add_settings_model(&mut models, env.get(key), &format!("env.{key}"));
    }
    models
}

fn add_settings_model(models: &mut Vec<JsValue>, value: Option<&JsValue>, settings_key: &str) {
    let Some(value) = value.and_then(JsValue::as_str) else {
        return;
    };
    let id = js_trim(value);
    if id.is_empty()
        || models
            .iter()
            .any(|model| model.get("id").and_then(JsValue::as_str) == Some(id))
    {
        return;
    }
    let mut model = JsObject::new();
    model.insert("provider", JsValue::String("claude".to_owned()));
    model.insert("id", JsValue::String(id.to_owned()));
    model.insert("label", JsValue::String(id.to_owned()));
    model.insert(
        "description",
        JsValue::String(format!("From Claude settings.json {settings_key}")),
    );
    models.push(JsValue::Object(model));
}

/// `resolveObservedClaudeModelId(value)`.
#[must_use]
pub fn resolve_observed_claude_model_id(value: Option<&str>) -> Option<String> {
    let trimmed = js_trim(value.unwrap_or_default());
    if trimmed.is_empty() || trimmed == "<synthetic>" {
        return None;
    }
    Some(normalize_claude_runtime_model_id(Some(trimmed)).unwrap_or_else(|| trimmed.to_owned()))
}

/// `claudeModelSupportsFastMode(modelId)`.
#[must_use]
pub fn claude_model_supports_fast_mode(model_id: Option<&str>) -> bool {
    claude_manifest_model_supports_fast_mode(model_id)
}

/// `buildClaudeFeatures({ modelId, fastModeEnabled })`: `AgentFeature[]`.
#[must_use]
pub fn build_claude_features(model_id: Option<&str>, fast_mode_enabled: bool) -> Vec<JsValue> {
    if !claude_model_supports_fast_mode(model_id) {
        return Vec::new();
    }
    let mut feature = JsObject::new();
    for (key, value) in [
        ("type", "toggle"),
        ("id", "fast_mode"),
        ("label", "Fast"),
        (
            "description",
            "Lower latency Opus responses at higher token cost",
        ),
        ("tooltip", "Toggle fast mode"),
        ("icon", "zap"),
    ] {
        feature.insert(key, JsValue::String(value.to_owned()));
    }
    feature.insert("value", JsValue::Bool(fast_mode_enabled));
    vec![JsValue::Object(feature)]
}
