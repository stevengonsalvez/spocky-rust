//! The built-in `codex` provider client the daemon creates at bootstrap:
//! `extractProviderOverrides` and `extractAgentProviderSettings`
//! (`config.ts`), then `mergeRuntimeSettings` with `toRuntimeSettings`
//! (`agent/provider-registry.ts`), handed to
//! [`crate::codex_agent::CodexAgentClient::new`].
//!
//! Only `command` and `env` reach the Codex client; `disallowedTools` is
//! carried by the baseline but never read by the Codex provider.

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use spocky_provider_codex::{ProviderCommand, ProviderRuntimeSettings};

const CODEX: &str = "codex";

/// The merged runtime settings of the built-in `codex` provider, from the
/// persisted config's `agents.providers`. `None` when no `codex` override
/// sets `command` or `env`, as both baseline helpers return `undefined`
/// then.
///
/// Precondition: `persisted` is the output of the daemon config loader,
/// which validates the whole file against `PersistedConfigSchema`
/// (including `ProviderOverridesSchema` and its legacy
/// `{ command: { mode, argv } }` migration) and refuses to start on an
/// invalid file, as `persisted-config.ts` does. This function reads that
/// validated shape and does not re-validate it.
#[must_use]
pub fn codex_runtime_settings(persisted: &Value) -> Option<ProviderRuntimeSettings> {
    let entry = persisted
        .get("agents")?
        .get("providers")?
        .get(CODEX)?
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

fn string_record(entries: &Map<String, Value>) -> BTreeMap<String, String> {
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
        let settings = codex_runtime_settings(&persisted).expect("settings");
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
        let other = providers(&json!({"claude": {"env": {"A": "b"}}}));
        assert_eq!(codex_runtime_settings(&other), None);
        let label_only = providers(&json!({"codex": {"label": "Codex", "disallowedTools": ["x"]}}));
        assert_eq!(codex_runtime_settings(&label_only), None);
    }
}
