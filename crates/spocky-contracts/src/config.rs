//! Persisted config validation as pinned `server/persisted-config.ts`
//! (Paseo `5de45e2`) performs it, and the error text the daemon refuses a
//! config file with.
//!
//! `parseConfigFile` strips a leading BOM, runs `JSON.parse`, drops removed
//! fields (`stripRemovedConfigFields`), and runs
//! `PersistedConfigSchema.safeParse`, whose schemas are generated into
//! [`crate::config_schema`]. The functions here are the hand-written parts
//! those schemas call: the `agents.providers` preprocess, the refinements,
//! and the regular expressions.

use crate::config_schema::{
    agent_provider_runtime_settings_map, persisted_config, provider_overrides,
};
use crate::js::{js_string, truthy};
use crate::js_value::{JsObject, JsValue, parse, stringify_pretty};
use crate::zod::{CustomIssue, Mapped, Verdict, verdict};

/// `BUILTIN_PROVIDER_IDS` in `persisted-config.ts` and `provider-config.ts`.
const BUILTIN_PROVIDER_IDS: [&str; 6] = ["claude", "codex", "copilot", "opencode", "pi", "omp"];

/// Why the daemon refuses a config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigRefusal {
    /// The thrown error's message, as `loadPersistedConfig` throws it.
    Message(String),
    /// Nested deeper than the zod port judges (DIV-001).
    TooDeep,
}

/// Validates config file text as `parseConfigFile(configPath, raw)` does
/// and returns the config it returns: `PersistedConfigSchema`'s output.
///
/// # Errors
///
/// Returns the refusal `loadPersistedConfig` throws: invalid JSON, invalid
/// config with every zod issue on its own line, or the `ZodError` that the
/// legacy provider migration throws.
pub fn check_config_text(config_path: &str, raw: &str) -> Result<JsValue, ConfigRefusal> {
    let raw = raw.strip_prefix('\u{FEFF}').unwrap_or(raw);
    let parsed = parse(raw).map_err(|error| {
        ConfigRefusal::Message(format!("[Config] Invalid JSON in {config_path}: {error}"))
    })?;
    match verdict(persisted_config(), &strip_removed_config_fields(&parsed)) {
        Verdict::Valid(config) => Ok(config),
        Verdict::Invalid(issues) => {
            let lines: Vec<String> = issues.iter().map(issue_line).collect();
            Err(ConfigRefusal::Message(format!(
                "[Config] Invalid config in {config_path}:\n{}",
                lines.join("\n")
            )))
        }
        Verdict::Throws(message) => Err(ConfigRefusal::Message(message)),
        Verdict::TooDeep | Verdict::Unmodeled => Err(ConfigRefusal::TooDeep),
    }
}

/// `` `  - ${i.path.join(".")}: ${i.message}` ``.
fn issue_line(issue: &JsValue) -> String {
    let path: Vec<String> = issue
        .get("path")
        .and_then(JsValue::as_array)
        .unwrap_or(&[])
        .iter()
        .map(|segment| js_string(Some(segment)))
        .collect();
    let message = issue.get("message").and_then(JsValue::as_str).unwrap_or("");
    format!("  - {}: {message}", path.join("."))
}

fn plain_object(value: &JsValue) -> Option<&JsObject> {
    value.as_object()
}

/// A copy of `object` without `key`, as `{ ...object }` then `delete`.
fn without(object: &JsObject, key: &str) -> JsObject {
    let mut copy = JsObject::new();
    for (name, value) in object.iter().filter(|(name, _)| *name != key) {
        copy.insert(name, value.clone());
    }
    copy
}

/// `stripRemovedConfigFields`: drops `providers.local.autoDownload` and
/// `providers.openai.voice`.
#[must_use]
pub fn strip_removed_config_fields(parsed: &JsValue) -> JsValue {
    let Some(root) = plain_object(parsed) else {
        return parsed.clone();
    };
    let mut root = root.clone();
    let Some(providers) = root.get("providers").and_then(plain_object) else {
        return JsValue::Object(root);
    };
    let mut providers = providers.clone();
    for (name, removed) in [("local", "autoDownload"), ("openai", "voice")] {
        if let Some(entry) = providers.get(name).and_then(plain_object) {
            let stripped = without(entry, removed);
            providers.insert(name, JsValue::Object(stripped));
        }
    }
    root.insert("providers", JsValue::Object(providers));
    JsValue::Object(root)
}

/// `isLegacyProviderEntry`: an object whose `command` is an object with a
/// string `mode`.
fn is_legacy_provider_entry(value: &JsValue) -> bool {
    value
        .get("command")
        .and_then(plain_object)
        .and_then(|command| command.get("mode"))
        .is_some_and(JsValue::is_string)
}

/// `normalizeAgentProviders`, the `agents.providers` preprocess: legacy
/// `{ command: { mode, ... } }` entries are migrated by
/// `migrateProviderSettings` and placed after the other entries.
///
/// An own `__proto__` key is skipped: assigning it to the plain objects the
/// baseline builds sets their prototype instead of adding an entry.
#[must_use]
pub fn normalize_agent_providers(value: &JsValue) -> Mapped {
    let Some(raw) = plain_object(value) else {
        return Mapped::Same;
    };
    if !raw.iter().any(|(_, entry)| is_legacy_provider_entry(entry)) {
        return Mapped::Same;
    }
    let mut legacy = JsObject::new();
    let mut normalized = JsObject::new();
    for (provider_id, entry) in raw.iter().filter(|(id, _)| *id != "__proto__") {
        if is_legacy_provider_entry(entry) {
            legacy.insert(provider_id, entry.clone());
        } else {
            normalized.insert(provider_id, entry.clone());
        }
    }
    let Verdict::Valid(legacy) = verdict(
        agent_provider_runtime_settings_map(),
        &JsValue::Object(legacy),
    ) else {
        return Mapped::Same;
    };
    let migrated = match migrate_provider_settings(&legacy) {
        Ok(migrated) => migrated,
        Err(message) => return Mapped::Throws(message),
    };
    for (provider_id, entry) in migrated.iter() {
        normalized.insert(provider_id, entry.clone());
    }
    Mapped::Value(JsValue::Object(normalized))
}

/// `migrateProviderSettings(parsedLegacyEntries.data, BUILTIN_PROVIDER_IDS)`
/// over the parsed legacy entries. Every one fails `ProviderOverrideSchema`
/// (its `command` is an object), so each takes the legacy path: `append` is
/// dropped, `replace` keeps `argv` as `command`, and `env` is kept.
///
/// # Errors
///
/// The `ZodError` message `ProviderOverridesSchema.parse(migrated)` throws.
fn migrate_provider_settings(legacy: &JsValue) -> Result<JsObject, String> {
    let mut migrated = JsObject::new();
    for (provider_id, entry) in legacy.as_object().map(JsObject::iter).into_iter().flatten() {
        let command = entry.get("command");
        let mode = command
            .and_then(|command| command.get("mode"))
            .and_then(JsValue::as_str);
        if mode == Some("append") {
            continue;
        }
        let mut next = JsObject::new();
        if mode == Some("replace")
            && let Some(argv) = command.and_then(|command| command.get("argv"))
        {
            next.insert("command", argv.clone());
        }
        if let Some(env) = entry.get("env").filter(|env| truthy(Some(env))) {
            next.insert("env", env.clone());
        }
        migrated.insert(provider_id, JsValue::Object(next));
    }
    match verdict(provider_overrides(), &JsValue::Object(migrated)) {
        Verdict::Valid(parsed) => Ok(parsed.as_object().cloned().unwrap_or_default()),
        Verdict::Invalid(issues) => Err(stringify_pretty(&JsValue::Array(issues))),
        _ => Ok(JsObject::new()),
    }
}

/// The `PersistedConfigSchema.daemon` transform:
/// `({ allowedHosts, ...daemon }) => hostnames === undefined ? daemon :
/// { ...daemon, hostnames }`, with `hostnames = daemon.hostnames ??
/// allowedHosts`.
#[must_use]
pub fn daemon_hostnames(value: &JsValue) -> Mapped {
    let Some(input) = plain_object(value) else {
        return Mapped::Same;
    };
    let mut daemon = without(input, "allowedHosts");
    let nullish = |value: Option<&JsValue>| {
        value.is_none_or(|value| matches!(value, JsValue::Undefined | JsValue::Null))
    };
    let hostnames = if nullish(daemon.get("hostnames")) {
        input.get("allowedHosts")
    } else {
        daemon.get("hostnames")
    };
    if let Some(hostnames) = hostnames.filter(|hostnames| !matches!(hostnames, JsValue::Undefined))
    {
        daemon.insert("hostnames", hostnames.clone());
    }
    Mapped::Value(JsValue::Object(daemon))
}

/// `PROVIDER_ID_PATTERN`, `/^[a-z][a-z0-9-]*$/`; also `PluginIdSchema`.
#[must_use]
pub fn is_provider_id(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes.next().is_some_and(|first| first.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// `/^\$2[aby]\$\d{2}\$[./A-Za-z0-9]{53}$/`, `BcryptHashSchema`.
#[must_use]
pub fn is_bcrypt_hash(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 60
        && bytes.starts_with(b"$2")
        && matches!(bytes[2], b'a' | b'b' | b'y')
        && bytes[3] == b'$'
        && bytes[4..6].iter().all(u8::is_ascii_digit)
        && bytes[6] == b'$'
        && bytes[7..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'/'))
}

/// `TCP_PORT_RANGE_PATTERN`, `/^(\d{1,5})-(\d{1,5})$/`, as its two numbers.
fn tcp_port_range(value: &str) -> Option<(u32, u32)> {
    let (start, end) = value.split_once('-')?;
    let number = |part: &str| {
        (1..=5).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_digit())
    };
    if !number(start) || !number(end) {
        return None;
    }
    Some((start.parse().ok()?, end.parse().ok()?))
}

/// `TCP_PORT_RANGE_PATTERN` as a test.
#[must_use]
pub fn is_tcp_port_range(value: &str) -> bool {
    tcp_port_range(value).is_some()
}

/// `PaseoServicePortAllocationSchema`'s first refine: `range` or
/// `portScript` is set. Optional keys of the parsed value are present
/// exactly when the input has them.
#[must_use]
pub fn has_range_or_port_script(value: &JsValue) -> bool {
    value.get("range").is_some() || value.get("portScript").is_some()
}

/// `PaseoServicePortAllocationSchema`'s second refine: a `range` is an
/// inclusive range within 1-65535. The parsed `range` is the trimmed input.
#[must_use]
pub fn is_inclusive_port_range(value: &JsValue) -> bool {
    let Some(range) = value.get("range").and_then(JsValue::as_str) else {
        return true;
    };
    let range = crate::text::js_trim(range);
    if range.is_empty() {
        return true;
    }
    tcp_port_range(range).is_some_and(|(start, end)| start >= 1 && end <= 65_535 && start <= end)
}

/// `ProviderOverridesSchema.superRefine`, over the parsed record.
#[must_use]
pub fn provider_overrides_issues(providers: &JsValue) -> Vec<CustomIssue> {
    let issue = |path: &[&str], message: String| CustomIssue {
        path: path.iter().map(|segment| (*segment).to_owned()).collect(),
        message,
    };
    let mut issues = Vec::new();
    let entries = providers
        .as_object()
        .map(JsObject::iter)
        .into_iter()
        .flatten();
    for (id, provider) in entries.filter(|(id, _)| *id != "__proto__") {
        if !is_provider_id(id) {
            issues.push(issue(
                &[id],
                format!("Provider ID \"{id}\" must match /^[a-z][a-z0-9-]*$/."),
            ));
        }
        let builtin = BUILTIN_PROVIDER_IDS.contains(&id);
        let extends = provider.get("extends");
        if !builtin && !truthy(extends) {
            issues.push(issue(
                &[id, "extends"],
                format!("Custom provider \"{id}\" must declare extends."),
            ));
        }
        if !builtin && !truthy(provider.get("label")) {
            issues.push(issue(
                &[id, "label"],
                format!("Custom provider \"{id}\" must declare label."),
            ));
        }
        let extends = extends
            .and_then(JsValue::as_str)
            .filter(|extends| !extends.is_empty());
        if let Some(extends) = extends
            && extends != "acp"
            && !BUILTIN_PROVIDER_IDS.contains(&extends)
        {
            issues.push(issue(
                &[id, "extends"],
                format!("Provider \"{id}\" extends unknown provider \"{extends}\"."),
            ));
        }
        if extends == Some("acp") && !truthy(provider.get("command")) {
            issues.push(issue(
                &[id, "command"],
                format!("Provider \"{id}\" extending \"acp\" must declare command."),
            ));
        }
    }
    issues
}

/// `AgentProviderRuntimeSettingsMapSchema.superRefine`: every key passes
/// `AgentProviderSchema`, which is `z.string()`, so it adds no issue.
#[must_use]
pub fn runtime_settings_map_issues(_providers: &JsValue) -> Vec<CustomIssue> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::{is_bcrypt_hash, is_inclusive_port_range, is_provider_id, is_tcp_port_range};
    use crate::js_value::parse;

    // Each pattern checked with RegExp.prototype.test in node v22.20.0.
    #[test]
    fn patterns_match_their_regular_expressions() {
        assert!(is_provider_id("a1-b"));
        assert!(!is_provider_id("1a"));
        assert!(!is_provider_id("aB"));
        assert!(!is_provider_id(""));
        let hash = format!("$2b$10${}", "a".repeat(53));
        assert!(is_bcrypt_hash(&hash));
        assert!(!is_bcrypt_hash(&hash.replace("$2b", "$2c")));
        assert!(!is_bcrypt_hash(&format!("{hash}a")));
        assert!(is_tcp_port_range("1-65535"));
        assert!(!is_tcp_port_range("1-655350"));
        assert!(!is_tcp_port_range("-1"));
    }

    #[test]
    fn port_range_refine_reads_the_trimmed_range() {
        let check = |text: &str| is_inclusive_port_range(&parse(text).unwrap());
        assert!(check(r#"{"range":" 1-2 "}"#));
        assert!(!check(r#"{"range":"0-2"}"#));
        assert!(!check(r#"{"range":"3-2"}"#));
        assert!(!check(r#"{"range":"1-65536"}"#));
        assert!(check(r#"{"portScript":"x"}"#));
    }
}
