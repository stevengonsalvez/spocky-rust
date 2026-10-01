//! Codex provider catalog: models, modes, and the default mode.
//!
//! Port of `CodexAppServerAgentClient.fetchCatalog` and
//! `fetchModelsFromAppServer` from pinned Paseo. The agent manager resolves a
//! session's default model from this catalog before the session exists
//! (`resolveDefaultModelId`); without it Codex 0.159.0 rejects `turn/start`
//! because the collaboration mode settings carry no `model`.

use serde_json::{Map, Value, json};

use crate::launch::CODEX_PROVIDER;
use crate::transport::{AppServerClient, DEFAULT_REQUEST_TIMEOUT, js_trim};

/// One entry of `CodexModelListResponseSchema.data`.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexModel {
    pub id: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub is_default: Option<bool>,
    pub model: Option<String>,
    pub default_reasoning_effort: Option<String>,
    pub supported_reasoning_efforts: Option<Vec<ReasoningEffortEntry>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningEffortEntry {
    pub reasoning_effort: Option<String>,
    pub description: Option<String>,
}

fn optional_string(record: &Map<String, Value>, key: &str) -> Result<Option<String>, ()> {
    match record.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(()),
    }
}

/// `CodexModelListResponseSchema.safeParse(response)`: any field of the
/// wrong type fails the whole list, which Paseo then treats as empty.
#[must_use]
pub fn parse_model_list(response: &Value) -> Vec<CodexModel> {
    parse_model_list_strict(response).unwrap_or_default()
}

fn parse_model_list_strict(response: &Value) -> Result<Vec<CodexModel>, ()> {
    let record = response.as_object().ok_or(())?;
    let data = match record.get("data") {
        None => return Ok(Vec::new()),
        Some(Value::Array(data)) => data,
        Some(_) => return Err(()),
    };
    let mut models = Vec::with_capacity(data.len());
    for entry in data {
        let entry = entry.as_object().ok_or(())?;
        let id = match entry.get("id") {
            Some(Value::String(id)) => id.clone(),
            _ => return Err(()),
        };
        let is_default = match entry.get("isDefault") {
            None => None,
            Some(Value::Bool(flag)) => Some(*flag),
            Some(_) => return Err(()),
        };
        let supported_reasoning_efforts = match entry.get("supportedReasoningEfforts") {
            None => None,
            Some(Value::Array(efforts)) => {
                let mut parsed = Vec::with_capacity(efforts.len());
                for effort in efforts {
                    let effort = effort.as_object().ok_or(())?;
                    parsed.push(ReasoningEffortEntry {
                        reasoning_effort: optional_string(effort, "reasoningEffort")?,
                        description: optional_string(effort, "description")?,
                    });
                }
                Some(parsed)
            }
            Some(_) => return Err(()),
        };
        models.push(CodexModel {
            id,
            display_name: optional_string(entry, "displayName")?,
            description: optional_string(entry, "description")?,
            is_default,
            model: optional_string(entry, "model")?,
            default_reasoning_effort: optional_string(entry, "defaultReasoningEffort")?,
            supported_reasoning_efforts,
        });
    }
    Ok(models)
}

/// Configured defaults read from Codex config (`readCodexConfiguredDefaults`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfiguredDefaults {
    pub model: Option<String>,
    pub thinking_option_id: Option<String>,
}

/// `normalizeCodexThinkingOptionId`.
#[must_use]
pub fn normalize_thinking(value: Option<&str>) -> Option<String> {
    let normalized = js_trim(value?);
    (!normalized.is_empty() && normalized != "default").then(|| normalized.to_owned())
}

/// `normalizeCodexModelId`.
#[must_use]
pub fn normalize_model(value: Option<&str>) -> Option<String> {
    let normalized = js_trim(value?);
    (!normalized.is_empty()).then(|| normalized.to_owned())
}

/// `readCodexConfiguredDefaults(client)`: `getUserSavedConfig`, then
/// `config/read` when either value is still missing.
#[must_use]
pub fn read_configured_defaults(client: &AppServerClient) -> ConfiguredDefaults {
    let read = |method: &str, effort_key: &str| -> ConfiguredDefaults {
        let Ok(response) = client.request(method, Some(json!({})), DEFAULT_REQUEST_TIMEOUT) else {
            return ConfiguredDefaults::default();
        };
        let config = response.get("config").and_then(Value::as_object);
        let field = |key: &str| {
            config
                .and_then(|config| config.get(key))
                .and_then(Value::as_str)
        };
        ConfiguredDefaults {
            model: normalize_model(field("model")),
            thinking_option_id: normalize_thinking(field(effort_key)),
        }
    };
    let saved = read("getUserSavedConfig", "modelReasoningEffort");
    if saved.model.is_some() && saved.thinking_option_id.is_some() {
        return saved;
    }
    let configured = read("config/read", "model_reasoning_effort");
    ConfiguredDefaults {
        model: saved.model.or(configured.model),
        thinking_option_id: saved.thinking_option_id.or(configured.thinking_option_id),
    }
}

/// `normalizeCodexModelLabel`: `displayName.replace(/\bgpt\b/gi, "GPT")`.
#[must_use]
pub fn normalize_model_label(display_name: &str) -> String {
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let chars: Vec<char> = display_name.chars().collect();
    let mut output = String::with_capacity(display_name.len());
    let mut index = 0;
    while index < chars.len() {
        let candidate: String = chars[index..chars.len().min(index + 3)].iter().collect();
        let before_ok = index == 0 || !is_word(chars[index - 1]);
        let after_ok = chars.get(index + 3).is_none_or(|c| !is_word(*c));
        if candidate.eq_ignore_ascii_case("gpt") && before_ok && after_ok {
            output.push_str("GPT");
            index += 3;
        } else {
            output.push(chars[index]);
            index += 1;
        }
    }
    output
}

/// `buildCodexModelDefinition(model, ctx)`.
#[must_use]
pub fn model_definition(
    model: &CodexModel,
    defaults: &ConfiguredDefaults,
    has_configured_default: bool,
) -> Value {
    let default_effort = normalize_thinking(model.default_reasoning_effort.as_deref());
    let resolved_default = defaults.thinking_option_id.clone().or(default_effort);
    let mut options: Vec<(String, Option<String>)> = Vec::new();
    for entry in model.supported_reasoning_efforts.iter().flatten() {
        let Some(id) = normalize_thinking(entry.reasoning_effort.as_deref()) else {
            continue;
        };
        let description = entry
            .description
            .clone()
            .filter(|description| !js_trim(description).is_empty());
        match options.iter_mut().find(|(known, _)| *known == id) {
            Some(existing) => existing.1 = description,
            None => options.push((id, description)),
        }
    }
    if let Some(resolved) = &resolved_default
        && !options.iter().any(|(id, _)| id == resolved)
    {
        let description = if defaults.thinking_option_id.as_ref() == Some(resolved) {
            "Configured default reasoning effort"
        } else {
            "Model default reasoning effort"
        };
        options.push((resolved.clone(), Some(description.to_owned())));
    }
    let thinking_options: Vec<Value> = options
        .iter()
        .map(|(id, description)| {
            let mut option = Map::new();
            option.insert("id".to_owned(), json!(id));
            option.insert("label".to_owned(), json!(id));
            if let Some(description) = description {
                option.insert("description".to_owned(), json!(description));
            }
            option.insert(
                "isDefault".to_owned(),
                json!(resolved_default.as_ref() == Some(id)),
            );
            Value::Object(option)
        })
        .collect();
    let default_thinking_option_id = resolved_default.clone().or_else(|| {
        thinking_options
            .first()
            .and_then(|option| option["id"].as_str())
            .map(str::to_owned)
    });
    let is_default = if has_configured_default {
        Some(defaults.model.as_deref() == Some(model.id.as_str()))
    } else {
        model.is_default
    };
    let mut definition = Map::new();
    definition.insert("provider".to_owned(), json!(CODEX_PROVIDER));
    definition.insert("id".to_owned(), json!(model.id));
    definition.insert(
        "label".to_owned(),
        json!(normalize_model_label(
            model.display_name.as_deref().unwrap_or("")
        )),
    );
    if let Some(description) = &model.description {
        definition.insert("description".to_owned(), json!(description));
    }
    if let Some(is_default) = is_default {
        definition.insert("isDefault".to_owned(), json!(is_default));
    }
    if !thinking_options.is_empty() {
        definition.insert("thinkingOptions".to_owned(), Value::Array(thinking_options));
    }
    if let Some(default_id) = default_thinking_option_id {
        definition.insert("defaultThinkingOptionId".to_owned(), json!(default_id));
    }
    definition.insert("metadata".to_owned(), model_metadata(model));
    Value::Object(definition)
}

/// `metadata` of a model definition: the zod-parsed raw fields.
fn model_metadata(model: &CodexModel) -> Value {
    let mut metadata = Map::new();
    if let Some(inner) = &model.model {
        metadata.insert("model".to_owned(), json!(inner));
    }
    if let Some(effort) = &model.default_reasoning_effort {
        metadata.insert("defaultReasoningEffort".to_owned(), json!(effort));
    }
    if let Some(efforts) = &model.supported_reasoning_efforts {
        let efforts: Vec<Value> = efforts
            .iter()
            .map(|entry| {
                let mut effort = Map::new();
                if let Some(value) = &entry.reasoning_effort {
                    effort.insert("reasoningEffort".to_owned(), json!(value));
                }
                if let Some(value) = &entry.description {
                    effort.insert("description".to_owned(), json!(value));
                }
                Value::Object(effort)
            })
            .collect();
        metadata.insert(
            "supportedReasoningEfforts".to_owned(),
            Value::Array(efforts),
        );
    }
    Value::Object(metadata)
}

/// `fetchModelsFromAppServer` over an initialized catalog client.
///
/// # Errors
/// Returns the `model/list` request failure.
pub fn models_from_app_server(client: &AppServerClient) -> Result<Vec<Value>, String> {
    let response = client
        .request("model/list", Some(json!({})), DEFAULT_REQUEST_TIMEOUT)
        .map_err(|error| error.message)?;
    let models = parse_model_list(&response);
    let defaults = read_configured_defaults(client);
    let has_configured_default = defaults
        .model
        .as_ref()
        .is_some_and(|wanted| models.iter().any(|model| &model.id == wanted));
    Ok(models
        .iter()
        .map(|model| model_definition(model, &defaults, has_configured_default))
        .collect())
}

/// `{ models, defaultModeId, modes }`.
#[must_use]
pub fn catalog(models: Vec<Value>, auto_review_enabled: bool) -> Value {
    json!({
        "models": Value::Array(models),
        "defaultModeId": if auto_review_enabled { "auto-review" } else { "auto" },
        "modes": crate::session::available_modes(auto_review_enabled),
    })
}

/// `catalog.models.find(isDefault) ?? catalog.models[0]` id, as the agent
/// manager's `resolveDefaultModelId` picks it.
#[must_use]
pub fn default_model_id(catalog: &Value) -> Option<String> {
    let models = catalog["models"].as_array()?;
    models
        .iter()
        .find(|model| model["isDefault"] == json!(true))
        .or_else(|| models.first())
        .and_then(|model| model["id"].as_str())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_labels_capitalize_whole_word_gpt() {
        assert_eq!(normalize_model_label("gpt-5.5"), "GPT-5.5");
        assert_eq!(normalize_model_label("Gpt Codex gpts"), "GPT Codex gpts");
        assert_eq!(normalize_model_label("chatgpt"), "chatgpt");
    }

    #[test]
    fn one_bad_field_empties_the_whole_model_list() {
        let good = json!({"data": [{"id": "a", "isDefault": true}]});
        assert_eq!(parse_model_list(&good).len(), 1);
        let bad = json!({"data": [{"id": "a"}, {"id": "b", "description": null}]});
        assert!(parse_model_list(&bad).is_empty());
    }

    #[test]
    fn model_definition_matches_paseo_shape() {
        let model = parse_model_list(&json!({"data": [{
            "id": "gpt-5.5", "model": "gpt-5.5", "displayName": "gpt-5.5",
            "description": "Fast", "isDefault": true, "hidden": false,
            "defaultReasoningEffort": "medium",
            "supportedReasoningEfforts": [
                {"reasoningEffort": "low", "description": "Fast responses"},
                {"reasoningEffort": "medium", "description": " "}
            ]
        }]}))
        .remove(0);
        let definition = model_definition(&model, &ConfiguredDefaults::default(), false);
        assert_eq!(
            serde_json::to_string(&definition).unwrap(),
            concat!(
                r#"{"provider":"codex","id":"gpt-5.5","label":"GPT-5.5","description":"Fast","isDefault":true,"#,
                r#""thinkingOptions":[{"id":"low","label":"low","description":"Fast responses","isDefault":false},"#,
                r#"{"id":"medium","label":"medium","isDefault":true}],"defaultThinkingOptionId":"medium","#,
                r#""metadata":{"model":"gpt-5.5","defaultReasoningEffort":"medium","supportedReasoningEfforts":"#,
                r#"[{"reasoningEffort":"low","description":"Fast responses"},{"reasoningEffort":"medium","description":" "}]}}"#
            )
        );
    }

    #[test]
    fn configured_default_model_overrides_is_default() {
        let models = parse_model_list(&json!({"data": [
            {"id": "a", "isDefault": true},
            {"id": "b", "isDefault": false}
        ]}));
        let defaults = ConfiguredDefaults {
            model: Some("b".to_owned()),
            thinking_option_id: Some("high".to_owned()),
        };
        let a = model_definition(&models[0], &defaults, true);
        let b = model_definition(&models[1], &defaults, true);
        assert_eq!(a["isDefault"], json!(false));
        assert_eq!(b["isDefault"], json!(true));
        assert_eq!(
            b["thinkingOptions"],
            json!([{"id": "high", "label": "high", "description": "Configured default reasoning effort", "isDefault": true}])
        );
        assert_eq!(
            default_model_id(&catalog(vec![a, b], true)),
            Some("b".to_owned())
        );
    }
}
