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

const BUILTIN_PROVIDER_IDS: [&str; 6] = ["claude", "codex", "copilot", "opencode", "pi", "omp"];

/// `/^[a-z][a-z0-9-]*$/`.
fn is_provider_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `ProviderOverridesSchema`: every entry parses as a `ProviderOverride`,
/// then the `superRefine` rules. Returns every issue, in entry order.
fn provider_issues(providers: &Map<String, Value>) -> Vec<String> {
    let mut issues = Vec::new();
    for (id, entry) in providers {
        if !is_provider_override(entry) {
            issues.push(format!("{id}: not a valid provider override"));
            continue;
        }
        let field = |key: &str| entry.get(key).and_then(Value::as_str);
        if !is_provider_id(id) {
            issues.push(format!(
                "{id}: Provider ID \"{id}\" must match /^[a-z][a-z0-9-]*$/."
            ));
        }
        let builtin = BUILTIN_PROVIDER_IDS.contains(&id.as_str());
        if !builtin && field("extends").is_none_or(str::is_empty) {
            issues.push(format!(
                "{id}.extends: Custom provider \"{id}\" must declare extends."
            ));
        }
        if !builtin && field("label").is_none_or(str::is_empty) {
            issues.push(format!(
                "{id}.label: Custom provider \"{id}\" must declare label."
            ));
        }
        if let Some(extends) = field("extends").filter(|extends| !extends.is_empty()) {
            if extends != "acp" && !BUILTIN_PROVIDER_IDS.contains(&extends) {
                issues.push(format!(
                    "{id}.extends: Provider \"{id}\" extends unknown provider \"{extends}\"."
                ));
            }
            if extends == "acp" && entry.get("command").is_none() {
                issues.push(format!(
                    "{id}.command: Provider \"{id}\" extending \"acp\" must declare command."
                ));
            }
        }
    }
    issues
}

/// The merged runtime settings of the built-in `codex` provider, from the
/// persisted config's `agents.providers`. `Ok(None)` when no `codex`
/// override sets `command` or `env`, as both baseline helpers return
/// `undefined` then.
///
/// # Errors
///
/// Any provider entry that fails `ProviderOverridesSchema`, or a
/// non-object `agents` or `agents.providers`. The baseline refuses to start
/// on such a config (`[Config] Invalid config in <path>`, from
/// `persisted-config.ts`); the config loader owns that message and runs
/// first. This check is a fail-closed backstop so codex never launches from
/// a config the baseline would reject. Legacy `{ command: { mode, argv } }`
/// entries must be normalized by the loader first; here they are rejected.
pub fn codex_runtime_settings(
    persisted: &Value,
) -> Result<Option<ProviderRuntimeSettings>, String> {
    let Some(agents) = persisted.get("agents") else {
        return Ok(None);
    };
    let agents = agents
        .as_object()
        .ok_or("[Config] Invalid config: agents must be an object")?;
    let Some(providers) = agents.get("providers") else {
        return Ok(None);
    };
    let providers = providers
        .as_object()
        .ok_or("[Config] Invalid config: agents.providers must be an object")?;
    let issues = provider_issues(providers);
    if !issues.is_empty() {
        let lines: Vec<String> = issues
            .iter()
            .map(|issue| format!("  - agents.providers.{issue}"))
            .collect();
        return Err(format!("[Config] Invalid config:\n{}", lines.join("\n")));
    }
    let Some(entry) = providers.get(CODEX).and_then(Value::as_object) else {
        return Ok(None);
    };
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
        return Ok(None);
    }
    Ok(Some(ProviderRuntimeSettings { command, env }))
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

    fn providers(entries: &serde_json::Value) -> serde_json::Value {
        json!({"agents": {"providers": entries}})
    }

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
        let settings = codex_runtime_settings(&persisted)
            .expect("valid")
            .expect("settings");
        assert_eq!(settings.command, None);
        let env = settings.env.expect("env");
        assert_eq!(env.len(), 3);
        assert_eq!(env["CODEX_HOME"], "/r/codex-home");
        assert_eq!(env["OPENAI_API_KEY"], "test-key");
    }

    #[test]
    fn command_becomes_a_replace_command() {
        let persisted = providers(&json!({"codex": {"command": ["/bin/codex", "-x"]}}));
        assert_eq!(
            codex_runtime_settings(&persisted),
            Ok(Some(ProviderRuntimeSettings {
                command: Some(ProviderCommand::Replace {
                    argv: vec!["/bin/codex".to_owned(), "-x".to_owned()]
                }),
                env: None,
            }))
        );
    }

    #[test]
    fn absent_or_settingless_overrides_yield_none() {
        assert_eq!(codex_runtime_settings(&json!({})), Ok(None));
        assert_eq!(codex_runtime_settings(&json!({"agents": {}})), Ok(None));
        let other = providers(&json!({"claude": {"env": {"A": "b"}}}));
        assert_eq!(codex_runtime_settings(&other), Ok(None));
        let label_only = providers(&json!({"codex": {"label": "Codex", "disallowedTools": ["x"]}}));
        assert_eq!(codex_runtime_settings(&label_only), Ok(None));
    }

    #[test]
    fn an_invalid_codex_entry_refuses_the_config() {
        // The baseline refuses to start rather than launch codex without
        // its configured CODEX_HOME and model endpoint.
        for invalid in [
            json!({"env": {"CODEX_HOME": "/r"}, "enabled": "yes"}),
            json!({"env": {"CODEX_HOME": 1}}),
            json!({"env": {"CODEX_HOME": "/r"}, "command": []}),
            json!({"env": {"CODEX_HOME": "/r"}, "command": [""]}),
            json!({"env": {"CODEX_HOME": "/r"}, "label": null}),
            json!({"env": {"CODEX_HOME": "/r"}, "params": []}),
            json!({"env": {"CODEX_HOME": "/r"}, "models": [{"id": "", "label": "m"}]}),
            json!({"env": {"CODEX_HOME": "/r"}, "paseoTools": {"disabledTools": [1]}}),
            json!({"env": {"CODEX_HOME": "/r"}, "extends": "nope"}),
            json!({"command": {"mode": "replace", "argv": ["/bin/codex"]}}),
            json!(["env"]),
        ] {
            let error = codex_runtime_settings(&providers(&json!({"codex": invalid})))
                .expect_err(&invalid.to_string());
            assert!(
                error.starts_with("[Config] Invalid config:\n  - agents.providers.codex"),
                "{error}"
            );
        }
    }

    #[test]
    fn any_invalid_provider_entry_refuses_the_config() {
        let valid_codex = json!({"env": {"CODEX_HOME": "/r"}});
        for (id, entry, issue) in [
            (
                "claude",
                json!({"enabled": 1}),
                "claude: not a valid provider override",
            ),
            (
                "My",
                json!({"extends": "codex", "label": "M"}),
                "My: Provider ID \"My\" must match",
            ),
            (
                "mine",
                json!({"label": "Mine"}),
                "mine.extends: Custom provider \"mine\" must declare extends.",
            ),
            (
                "mine",
                json!({"extends": "codex"}),
                "mine.label: Custom provider \"mine\" must declare label.",
            ),
            (
                "tool",
                json!({"extends": "acp", "label": "T"}),
                "tool.command: Provider \"tool\" extending \"acp\" must declare command.",
            ),
        ] {
            let persisted = providers(&json!({"codex": valid_codex, id: entry}));
            let error = codex_runtime_settings(&persisted).expect_err(id);
            assert!(
                error.contains(&format!("  - agents.providers.{issue}")),
                "{error}"
            );
        }
        assert!(codex_runtime_settings(&json!({"agents": []})).is_err());
        assert!(codex_runtime_settings(&json!({"agents": {"providers": "x"}})).is_err());
    }

    #[test]
    fn unknown_keys_and_valid_custom_providers_pass() {
        let persisted = providers(&json!({
            "codex": {
                "env": {"A": "b"},
                "somethingNew": [1, 2],
                "models": [{"id": "m", "label": "M", "thinkingOptions": [{"id": "low", "label": "Low"}]}],
                "order": 2.5
            },
            "codex-stub": {"extends": "codex", "label": "Codex Stub"},
            "tool": {"extends": "acp", "label": "Tool", "command": ["tool"]}
        }));
        let settings = codex_runtime_settings(&persisted)
            .expect("valid")
            .expect("settings");
        assert_eq!(settings.env.expect("env")["A"], "b");
    }
}
