//! The built-in `codex` provider client the daemon creates at bootstrap:
//! `extractProviderOverrides` and `extractAgentProviderSettings`
//! (`config.ts`), then `mergeRuntimeSettings` with `toRuntimeSettings`
//! (`agent/provider-registry.ts`), handed to
//! [`crate::codex_agent::CodexAgentClient::new`].
//!
//! Only `command` and `env` reach the Codex client; `disallowedTools` is
//! carried by the baseline but never read by the Codex provider.

use serde_json::{Map, Value};
use spocky_provider_codex::{ProviderCommand, ProviderRuntimeSettings};

const CODEX: &str = "codex";

fn is_optional(value: Option<&Value>, check: impl Fn(&Value) -> bool) -> bool {
    value.is_none_or(check)
}

/// `z.array(z.string())`, with `.min(1)` items and `z.string().min(1)`
/// entries when `non_empty`.
fn is_string_array(value: &Value, non_empty: bool) -> bool {
    value.as_array().is_some_and(|items| {
        (!non_empty || !items.is_empty())
            && items
                .iter()
                .all(|item| item.as_str().is_some_and(|s| !non_empty || !s.is_empty()))
    })
}

fn is_string_record(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|entries| entries.values().all(Value::is_string))
}

fn is_non_empty_string(value: &Value) -> bool {
    value.as_str().is_some_and(|s| !s.is_empty())
}

/// `ProviderProfileThinkingOptionSchema`.
fn is_thinking_option(value: &Value) -> bool {
    value.as_object().is_some_and(|option| {
        option.get("id").is_some_and(Value::is_string)
            && option.get("label").is_some_and(Value::is_string)
            && is_optional(option.get("description"), Value::is_string)
            && is_optional(option.get("isDefault"), Value::is_boolean)
    })
}

/// `ProviderProfileModelSchema`.
fn is_profile_model(value: &Value) -> bool {
    value.as_object().is_some_and(|model| {
        model.get("id").is_some_and(is_non_empty_string)
            && model.get("label").is_some_and(is_non_empty_string)
            && is_optional(model.get("description"), Value::is_string)
            && is_optional(model.get("isDefault"), Value::is_boolean)
            && is_optional(model.get("thinkingOptions"), |options| {
                options
                    .as_array()
                    .is_some_and(|options| options.iter().all(is_thinking_option))
            })
    })
}

fn is_profile_models(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|models| models.iter().all(is_profile_model))
}

/// `ProviderOverrideSchema.safeParse(provider).success`. Unknown keys are
/// stripped by zod, so they never fail the parse; `null` fails every
/// optional field.
fn is_provider_override(value: &Value) -> bool {
    let Some(entry) = value.as_object() else {
        return false;
    };
    let field = |key: &str| entry.get(key);
    is_optional(field("extends"), Value::is_string)
        && is_optional(field("label"), Value::is_string)
        && is_optional(field("description"), Value::is_string)
        && is_optional(field("command"), |v| is_string_array(v, true))
        && is_optional(field("env"), is_string_record)
        && is_optional(field("params"), Value::is_object)
        && is_optional(field("models"), is_profile_models)
        && is_optional(field("additionalModels"), is_profile_models)
        && is_optional(field("disallowedTools"), |v| is_string_array(v, false))
        && is_optional(field("paseoTools"), |tools| {
            tools.as_object().is_some_and(|tools| {
                is_optional(tools.get("enabled"), Value::is_boolean)
                    && is_optional(tools.get("disabledTools"), |v| is_string_array(v, false))
            })
        })
        && is_optional(field("enabled"), Value::is_boolean)
        && is_optional(field("order"), Value::is_number)
}

/// The merged runtime settings of the built-in `codex` provider, from the
/// persisted config's `agents.providers`. `None` when no valid `codex`
/// override sets `command` or `env`, as both baseline helpers return
/// `undefined` then.
#[must_use]
pub fn codex_runtime_settings(persisted: &Value) -> Option<ProviderRuntimeSettings> {
    let entry = persisted
        .get("agents")
        .and_then(|agents| agents.get("providers"))
        .and_then(Value::as_object)
        .and_then(|providers| providers.get(CODEX))
        .filter(|entry| is_provider_override(entry))?
        .as_object()?;
    let command =
        entry
            .get("command")
            .and_then(Value::as_array)
            .map(|argv| ProviderCommand::Replace {
                argv: argv
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
            });
    let env = entry
        .get("env")
        .and_then(Value::as_object)
        .map(string_record);
    // `mergeRuntimeSettings(settings, toRuntimeSettings(override))` merges the
    // override with settings built from the same override: the command and
    // every env key are unchanged by the spread.
    if command.is_none() && env.is_none() {
        return None;
    }
    Some(ProviderRuntimeSettings { command, env })
}

fn string_record(entries: &Map<String, Value>) -> std::collections::BTreeMap<String, String> {
    entries
        .iter()
        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use spocky_provider_codex::{ProviderCommand, ProviderRuntimeSettings};

    use super::codex_runtime_settings;

    #[test]
    fn harness_config_yields_env_only_settings() {
        // `config.json` written by the slice harness for both daemons.
        let persisted = json!({
            "daemon": {"listen": "127.0.0.1:50707", "relay": {"enabled": false}},
            "agents": {"providers": {"codex": {"env": {
                "CODEX_HOME": "/r/codex-home",
                "OPENAI_BASE_URL": "http://127.0.0.1:50706/v1",
                "OPENAI_API_KEY": "test-key"
            }}}}
        });
        let settings = codex_runtime_settings(&persisted).expect("settings");
        assert_eq!(settings.command, None);
        let env = settings.env.expect("env");
        assert_eq!(env.len(), 3);
        assert_eq!(env["CODEX_HOME"], "/r/codex-home");
        assert_eq!(env["OPENAI_API_KEY"], "test-key");
    }

    #[test]
    fn command_becomes_a_replace_command() {
        let persisted =
            json!({"agents": {"providers": {"codex": {"command": ["/bin/codex", "-x"]}}}});
        assert_eq!(
            codex_runtime_settings(&persisted),
            Some(ProviderRuntimeSettings {
                command: Some(ProviderCommand::Replace {
                    argv: vec!["/bin/codex".to_owned(), "-x".to_owned()]
                }),
                env: None,
            })
        );
    }

    #[test]
    fn absent_or_settingless_overrides_yield_none() {
        assert_eq!(codex_runtime_settings(&json!({})), None);
        assert_eq!(codex_runtime_settings(&json!({"agents": {}})), None);
        let other = json!({"agents": {"providers": {"claude": {"env": {"A": "b"}}}}});
        assert_eq!(codex_runtime_settings(&other), None);
        let label_only = json!({"agents": {"providers": {"codex": {"label": "Codex", "disallowedTools": ["x"]}}}});
        assert_eq!(codex_runtime_settings(&label_only), None);
    }

    #[test]
    fn an_override_failing_its_schema_is_dropped_whole() {
        // `extractProviderOverrides` drops entries whose safeParse fails, so
        // valid env beside an invalid field is ignored too.
        for invalid in [
            json!({"env": {"A": "b"}, "enabled": "yes"}),
            json!({"env": {"A": 1}}),
            json!({"env": {"A": "b"}, "command": []}),
            json!({"env": {"A": "b"}, "command": [""]}),
            json!({"env": {"A": "b"}, "label": null}),
            json!({"env": {"A": "b"}, "params": []}),
            json!({"env": {"A": "b"}, "models": [{"id": "", "label": "m"}]}),
            json!({"env": {"A": "b"}, "paseoTools": {"disabledTools": [1]}}),
            json!(["env"]),
        ] {
            let persisted = json!({"agents": {"providers": {"codex": invalid}}});
            assert_eq!(codex_runtime_settings(&persisted), None, "{invalid}");
        }
    }

    #[test]
    fn unknown_keys_do_not_fail_the_override() {
        let persisted = json!({"agents": {"providers": {"codex": {
            "env": {"A": "b"},
            "somethingNew": [1, 2],
            "models": [{"id": "m", "label": "M", "thinkingOptions": [{"id": "low", "label": "Low"}]}],
            "order": 2.5
        }}}});
        let settings = codex_runtime_settings(&persisted).expect("settings");
        assert_eq!(settings.env.expect("env")["A"], "b");
    }
}
