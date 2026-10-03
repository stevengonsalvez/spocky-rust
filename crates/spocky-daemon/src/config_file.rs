//! The `config.json` fields the transport reads, and the first-run file.
//!
//! Source at Paseo `5de45e2`: `loadPersistedConfig`, `DEFAULT_PERSISTED_CONFIG`
//! and `parseConfigFile` in `persisted-config.ts`.
//!
//! The whole file is validated with `spocky_contracts::config::check_config_text`
//! (the pinned `PersistedConfigSchema`), so a config the baseline refuses is
//! refused here with the same text: an unrecognized key in any section, an
//! invalid `hostnames` entry, every issue on its own line, and the V8
//! `JSON.parse` text for a syntax error. Only `daemon.listen`,
//! `daemon.hostnames` (and its old name `allowedHosts`), `daemon.cors.allowedOrigins`
//! and `daemon.auth.password` are then read; the rest belongs to the session
//! layer.

use std::fmt;
use std::fs;
use std::path::Path;

use spocky_contracts::config::{ConfigRefusal, check_config_text};
use spocky_contracts::js_value::JsValue;

use crate::hostnames::Hostnames;
use crate::js;
use crate::log::{Level, Logger, resolve_level};
use crate::private_files::{ensure_private_file, write_private_file_atomic};

/// `JSON.stringify(DEFAULT_PERSISTED_CONFIG, null, 2) + "\n"`.
const DEFAULT_CONFIG_TEXT: &str = r#"{
  "version": 1,
  "daemon": {
    "listen": "127.0.0.1:6767",
    "cors": {
      "allowedOrigins": [
        "https://app.paseo.sh"
      ]
    },
    "relay": {
      "enabled": false
    }
  },
  "app": {
    "baseUrl": "https://app.paseo.sh"
  }
}
"#;

/// The subset of the persisted config the daemon transport needs.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct PersistedDaemonConfig {
    pub listen: Option<String>,
    pub hostnames: Option<Hostnames>,
    pub cors_allowed_origins: Vec<String>,
    /// bcrypt hash from `daemon.auth.password`.
    pub auth_password: Option<String>,
    /// `features.dictation.enabled`.
    pub dictation_enabled: Option<bool>,
    /// `features.voiceMode.enabled`.
    pub voice_mode_enabled: Option<bool>,
}

/// The password hash is a credential: never print it.
impl fmt::Debug for PersistedDaemonConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PersistedDaemonConfig")
            .field("listen", &self.listen)
            .field("hostnames", &self.hostnames)
            .field("cors_allowed_origins", &self.cors_allowed_origins)
            .field("dictation_enabled", &self.dictation_enabled)
            .field("voice_mode_enabled", &self.voice_mode_enabled)
            .field(
                "auth_password",
                &self.auth_password.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// An error with the baseline's `[Config]` message prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

/// `loadPersistedConfig`: create the default file when absent, then parse it.
///
/// # Errors
///
/// A `[Config]` message when the file cannot be created, read or parsed, or
/// when a field the transport reads has the wrong type.
pub fn load_persisted_config(
    paseo_home: &Path,
    logger: &dyn Logger,
) -> Result<PersistedDaemonConfig, ConfigError> {
    let config_path = paseo_home.join("config.json");
    if !config_path.exists() {
        write_private_file_atomic(&config_path, DEFAULT_CONFIG_TEXT.as_bytes()).map_err(
            |error| {
                ConfigError(format!(
                    "[Config] Failed to initialize {}: {error}",
                    config_path.display()
                ))
            },
        )?;
        logger.info(
            &[],
            &format!("Initialized config file at {}", config_path.display()),
        );
    }
    ensure_private_file(&config_path);
    let raw = fs::read(&config_path).map_err(|error| {
        ConfigError(format!(
            "[Config] Failed to read {}: {error}",
            config_path.display()
        ))
    })?;
    let config = parse_config_text(&config_path, &String::from_utf8_lossy(&raw))?;
    logger.info(&[], &format!("Loaded from {}", config_path.display()));
    Ok(config)
}

/// The level the root logger is created with, from the `log` section of
/// `config.json` (`createRootLogger({ log: config.log }, { paseoHome, file })`).
/// The worker passes `files_allowed` false. A missing or refused config gives
/// the default, `info`: the start reports the refusal itself.
#[must_use]
pub fn configured_log_level(paseo_home: &Path, files_allowed: bool) -> Level {
    let config_path = paseo_home.join("config.json");
    let Ok(raw) = fs::read(&config_path) else {
        return Level::Info;
    };
    let Ok(config) = check_config_text(
        &config_path.display().to_string(),
        &String::from_utf8_lossy(&raw),
    ) else {
        return Level::Info;
    };
    let level = |section: Option<&JsValue>| {
        section
            .and_then(|section| section.get("level"))
            .and_then(JsValue::as_str)
            .and_then(Level::from_name)
    };
    let log = config.get("log");
    let file = log.and_then(|log| log.get("file"));
    resolve_level(
        level(log),
        level(log.and_then(|log| log.get("console"))),
        file.map(|file| level(Some(file))),
        files_allowed,
    )
}

/// `parseConfigFile`: `check_config_text` in spocky-contracts runs the pinned
/// schema, so an unrecognized key, an invalid entry or a syntax error is
/// refused with the baseline's text; the daemon fields are then read from the
/// validated config.
fn parse_config_text(config_path: &Path, raw: &str) -> Result<PersistedDaemonConfig, ConfigError> {
    let config =
        check_config_text(&config_path.display().to_string(), raw).map_err(
            |refusal| match refusal {
                ConfigRefusal::Message(message) => ConfigError(message),
                // Nested past where the pinned zod throws a `RangeError`, which
                // `loadPersistedConfig` lets through unwrapped.
                ConfigRefusal::TooDeep => {
                    ConfigError("Maximum call stack size exceeded".to_owned())
                }
            },
        )?;
    Ok(daemon_fields(&config))
}

/// The fields the transport reads, from a config `PersistedConfigSchema`
/// accepted (`allowedHosts` is already folded into `hostnames`).
fn daemon_fields(config: &JsValue) -> PersistedDaemonConfig {
    let daemon = config.get("daemon");
    let field = |name: &str| daemon.and_then(|daemon| daemon.get(name));
    let strings = |value: Option<&JsValue>| -> Vec<String> {
        value
            .and_then(JsValue::as_array)
            .unwrap_or_default()
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect()
    };
    PersistedDaemonConfig {
        listen: field("listen").and_then(JsValue::as_str).map(str::to_owned),
        hostnames: field("hostnames").and_then(|value| match value {
            JsValue::Bool(true) => Some(Hostnames::Any),
            JsValue::Array(_) => Some(Hostnames::Patterns(strings(Some(value)))),
            _ => None,
        }),
        cors_allowed_origins: strings(field("cors").and_then(|cors| cors.get("allowedOrigins"))),
        auth_password: field("auth")
            .and_then(|auth| auth.get("password"))
            .and_then(JsValue::as_str)
            .map(str::to_owned),
        dictation_enabled: feature_enabled(config, "dictation"),
        voice_mode_enabled: feature_enabled(config, "voiceMode"),
    }
}

/// `features.<name>.enabled`.
fn feature_enabled(config: &JsValue, name: &str) -> Option<bool> {
    config
        .get("features")
        .and_then(|features| features.get(name))
        .and_then(|feature| feature.get("enabled"))
        .and_then(JsValue::as_bool)
}

/// `env.X ?? env.Y` for a string environment value that is present.
#[must_use]
pub fn first_present<'a>(values: &[Option<&'a str>]) -> Option<&'a str> {
    values.iter().find_map(|value| *value)
}

/// `nonEmptyEnv`-style trim used when a value must not be blank.
#[must_use]
pub fn trimmed_non_empty(value: Option<&str>) -> Option<&str> {
    value.map(js::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::NullLogger;

    fn load(
        text: Option<&str>,
    ) -> (
        tempfile::TempDir,
        Result<PersistedDaemonConfig, ConfigError>,
    ) {
        let home = tempfile::tempdir().unwrap();
        if let Some(text) = text {
            fs::write(home.path().join("config.json"), text).unwrap();
        }
        let result = load_persisted_config(home.path(), &NullLogger);
        (home, result)
    }

    #[test]
    fn a_missing_file_is_created_with_the_pinned_defaults() {
        let (home, result) = load(None);
        assert_eq!(
            fs::read_to_string(home.path().join("config.json")).unwrap(),
            DEFAULT_CONFIG_TEXT
        );
        assert_eq!(
            result.unwrap(),
            PersistedDaemonConfig {
                listen: Some("127.0.0.1:6767".to_owned()),
                hostnames: None,
                cors_allowed_origins: vec!["https://app.paseo.sh".to_owned()],
                auth_password: None,
                dictation_enabled: None,
                voice_mode_enabled: None,
            }
        );
    }

    #[test]
    fn reads_the_daemon_fields() {
        let (_home, result) = load(Some(
            r#"{"daemon":{"listen":"127.0.0.1:9","hostnames":[".a.com"],"cors":{"allowedOrigins":["x"]},"auth":{"password":"$2a$12$abcdefghijklmnopqrstuuABCDEFGHIJKLMNOPQRSTUVWXYZ01234"}}}"#,
        ));
        let config = result.unwrap();
        assert_eq!(config.listen.as_deref(), Some("127.0.0.1:9"));
        assert_eq!(
            config.hostnames,
            Some(Hostnames::Patterns(vec![".a.com".to_owned()]))
        );
        assert_eq!(config.cors_allowed_origins, ["x"]);
        assert_eq!(
            config.auth_password.as_deref(),
            Some("$2a$12$abcdefghijklmnopqrstuuABCDEFGHIJKLMNOPQRSTUVWXYZ01234")
        );
    }

    #[test]
    fn allowed_hosts_is_the_old_name_and_hostnames_wins() {
        let (_h, old) = load(Some(r#"{"daemon":{"allowedHosts":true}}"#));
        assert_eq!(old.unwrap().hostnames, Some(Hostnames::Any));
        let (_h, both) = load(Some(
            r#"{"daemon":{"allowedHosts":true,"hostnames":["h"]}}"#,
        ));
        assert_eq!(
            both.unwrap().hostnames,
            Some(Hostnames::Patterns(vec!["h".to_owned()]))
        );
    }

    #[test]
    fn a_byte_order_mark_is_ignored_and_a_config_without_daemon_is_empty() {
        let (_home, result) = load(Some("\u{feff}{}"));
        assert_eq!(result.unwrap(), PersistedDaemonConfig::default());
    }

    #[test]
    fn bad_json_and_bad_types_carry_the_config_prefix() {
        let (home, result) = load(Some("{nope"));
        let message = result.unwrap_err().0;
        assert!(message.starts_with(&format!(
            "[Config] Invalid JSON in {}: ",
            home.path().join("config.json").display()
        )));
        for bad in [
            r#"{"daemon":{"listen":5}}"#,
            r#"{"daemon":{"hostnames":false}}"#,
            r#"{"daemon":{"cors":{"allowedOrigins":[1]}}}"#,
            r#"{"daemon":{"auth":{"password":null}}}"#,
            "[]",
        ] {
            let (_home, result) = load(Some(bad));
            assert!(
                result
                    .unwrap_err()
                    .0
                    .starts_with("[Config] Invalid config in "),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_bad_hostnames_value_reports_zod_invalid_input_without_a_received_suffix() {
        let (home, result) = load(Some(r#"{"daemon":{"hostnames":false}}"#));
        assert_eq!(
            result.unwrap_err().0,
            format!(
                "[Config] Invalid config in {}:\n  - daemon.hostnames: Invalid input",
                home.path().join("config.json").display()
            )
        );
    }

    #[test]
    fn a_group_readable_config_is_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let (home, result) = load(Some("{}"));
        result.unwrap();
        let path = home.path().join("config.json");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        load_persisted_config(home.path(), &NullLogger).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn env_helpers_follow_nullish_coalescing_and_trimming() {
        assert_eq!(first_present(&[None, Some(""), Some("b")]), Some(""));
        assert_eq!(first_present(&[None, None]), None);
        assert_eq!(trimmed_non_empty(Some("  x ")), Some("x"));
        assert_eq!(trimmed_non_empty(Some("  ")), None);
    }

    const HASH: &str = "$2b$12$abcdefghijklmnopqrstuuABCDEFGHIJKLMNOPQRSTUVWXYZ01234";

    #[test]
    fn an_empty_or_malformed_password_is_a_config_error_not_no_password() {
        for bad in ["", "plain", "$2b$12$short"] {
            let text = format!(r#"{{"daemon":{{"auth":{{"password":"{bad}"}}}}}}"#);
            let (_home, result) = load(Some(&text));
            let message = result.unwrap_err().0;
            assert!(
                message.ends_with("daemon.auth.password: Expected a bcrypt hash"),
                "{bad:?}: {message}"
            );
        }
        let (_home, result) = load(Some(&format!(
            r#"{{"daemon":{{"auth":{{"password":"{HASH}"}}}}}}"#
        )));
        assert_eq!(result.unwrap().auth_password.as_deref(), Some(HASH));
    }

    #[test]
    fn debug_output_never_contains_the_password_hash() {
        let (_home, result) = load(Some(&format!(
            r#"{{"daemon":{{"auth":{{"password":"{HASH}"}}}}}}"#
        )));
        let shown = format!("{:?}", result.unwrap());
        assert!(!shown.contains(HASH) && !shown.contains("$2b$"), "{shown}");
        assert!(shown.contains("<redacted>"));
    }
}
