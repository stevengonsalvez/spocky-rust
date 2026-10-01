//! The `config.json` fields the transport reads, and the first-run file.
//!
//! Source at Paseo `5de45e2`: `loadPersistedConfig`, `DEFAULT_PERSISTED_CONFIG`
//! and `parseConfigFile` in `persisted-config.ts`.
//!
//! Only `daemon.listen`, `daemon.hostnames` (and its old name `allowedHosts`),
//! `daemon.cors.allowedOrigins` and `daemon.auth.password` are read.
//!
//! Unported: config refusal. The baseline parses the whole file with one strict
//! zod schema (`persisted-config.ts`) and refuses what it rejects: an
//! unrecognized key in any section (`Unrecognized key: "bogus"`), an invalid
//! `allowedHosts` entry beside valid ones, and every issue joined as zod joins
//! them, with the V8 `JSON.parse` text for a syntax error. This module checks
//! only the four fields above, so a config the baseline refuses can start the
//! daemon and the text of the errors it does report differs. Closing the gap
//! needs a persisted-config schema in the contracts zod port, after which the
//! check calls it with no second zod here. Tracked as an open gap, deferred
//! until after G1.

use std::fmt;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::hostnames::Hostnames;
use crate::js;
use crate::log::Logger;
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
}

/// The password hash is a credential: never print it.
impl fmt::Debug for PersistedDaemonConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PersistedDaemonConfig")
            .field("listen", &self.listen)
            .field("hostnames", &self.hostnames)
            .field("cors_allowed_origins", &self.cors_allowed_origins)
            .field(
                "auth_password",
                &self.auth_password.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// `BcryptHashSchema`: `/^\$2[aby]\$\d{2}\$[./A-Za-z0-9]{53}$/`. An empty or
/// malformed `daemon.auth.password` is a config error, as in the baseline. It is
/// never read as "no password", so a bad value cannot open the daemon.
fn is_bcrypt_hash(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 60
        && bytes.starts_with(b"$2")
        && matches!(bytes[2], b'a' | b'b' | b'y')
        && bytes[3] == b'$'
        && bytes[4].is_ascii_digit()
        && bytes[5].is_ascii_digit()
        && bytes[6] == b'$'
        && bytes[7..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'.' || *b == b'/')
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

fn parse_config_text(config_path: &Path, raw: &str) -> Result<PersistedDaemonConfig, ConfigError> {
    let text = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let parsed: Value = serde_json::from_str(text).map_err(|error| {
        ConfigError(format!(
            "[Config] Invalid JSON in {}: {error}",
            config_path.display()
        ))
    })?;
    extract(&parsed).map_err(|issue| {
        ConfigError(format!(
            "[Config] Invalid config in {}:\n  - {issue}",
            config_path.display()
        ))
    })
}

fn string_list(value: &Value, path: &str) -> Result<Vec<String>, String> {
    let Value::Array(items) = value else {
        return Err(format!(
            "{path}: Invalid input: expected array, received {}",
            kind(value)
        ));
    };
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            item.as_str().map(str::to_owned).ok_or_else(|| {
                format!(
                    "{path}.{index}: Invalid input: expected string, received {}",
                    kind(item)
                )
            })
        })
        .collect()
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn hostnames_field(value: &Value, path: &str) -> Result<Hostnames, String> {
    match value {
        Value::Bool(true) => Ok(Hostnames::Any),
        Value::Array(_) => string_list(value, path).map(Hostnames::Patterns),
        _ => Err(format!("{path}: Invalid input")),
    }
}

fn extract(root: &Value) -> Result<PersistedDaemonConfig, String> {
    let Value::Object(root) = root else {
        return Err(format!(
            ": Invalid input: expected object, received {}",
            kind(root)
        ));
    };
    let mut config = PersistedDaemonConfig::default();
    let Some(daemon) = root.get("daemon") else {
        return Ok(config);
    };
    let Value::Object(daemon) = daemon else {
        return Err(format!(
            "daemon: Invalid input: expected object, received {}",
            kind(daemon)
        ));
    };
    if let Some(listen) = daemon.get("listen") {
        config.listen = Some(
            listen
                .as_str()
                .ok_or_else(|| {
                    format!(
                        "daemon.listen: Invalid input: expected string, received {}",
                        kind(listen)
                    )
                })?
                .to_owned(),
        );
    }
    // `allowedHosts` is the old name; `hostnames` wins when both are present.
    if let Some(value) = daemon
        .get("hostnames")
        .or_else(|| daemon.get("allowedHosts"))
    {
        let path = if daemon.contains_key("hostnames") {
            "daemon.hostnames"
        } else {
            "daemon.allowedHosts"
        };
        config.hostnames = Some(hostnames_field(value, path)?);
    }
    if let Some(cors) = daemon.get("cors") {
        let Value::Object(cors) = cors else {
            return Err(format!(
                "daemon.cors: Invalid input: expected object, received {}",
                kind(cors)
            ));
        };
        if let Some(origins) = cors.get("allowedOrigins") {
            config.cors_allowed_origins = string_list(origins, "daemon.cors.allowedOrigins")?;
        }
    }
    if let Some(auth) = daemon.get("auth") {
        let Value::Object(auth) = auth else {
            return Err(format!(
                "daemon.auth: Invalid input: expected object, received {}",
                kind(auth)
            ));
        };
        if let Some(password) = auth.get("password") {
            let hash = password.as_str().ok_or_else(|| {
                format!(
                    "daemon.auth.password: Invalid input: expected string, received {}",
                    kind(password)
                )
            })?;
            if !is_bcrypt_hash(hash) {
                return Err("daemon.auth.password: Expected a bcrypt hash".to_owned());
            }
            config.auth_password = Some(hash.to_owned());
        }
    }
    Ok(config)
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
    fn the_bcrypt_pattern_matches_the_baseline_regex() {
        assert!(is_bcrypt_hash(HASH));
        assert!(is_bcrypt_hash(&HASH.replace("$2b$", "$2a$")));
        assert!(is_bcrypt_hash(&HASH.replace("$2b$", "$2y$")));
        assert!(is_bcrypt_hash(&format!(
            "$2b$12${}",
            "./".repeat(25) + "AB1"
        )));
        for bad in [
            "",
            "x",
            &HASH.replace("$2b$", "$2x$"),
            &HASH.replace("$12$", "$1$"),
            &HASH.replace("$12$", "$ab$"),
            &format!("{HASH}x"),
            &HASH[..59],
            &HASH.replace('Z', "-"),
            &HASH.replace('Z', "_"),
        ] {
            assert!(!is_bcrypt_hash(bad), "{bad:?}");
        }
    }

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
