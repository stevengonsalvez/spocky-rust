//! The daemon's long-lived relay key pair, `daemon-keypair.json`.
//!
//! Source at Paseo `5de45e2`: `daemon-keypair.ts`. The relay is disabled in the
//! vertical slice, but the file is part of the home layout the baseline
//! creates on every start, so it is created here too.

use std::fs;
use std::path::Path;

use serde_json::Value;
use spocky_crypto::{
    KeyPair, export_public_key, export_secret_key, generate_key_pair, import_public_key,
    import_secret_key,
};

use crate::log::Logger;
use crate::private_files::{ensure_private_file, write_private_file_atomic};

const KEYPAIR_FILENAME: &str = "daemon-keypair.json";

/// `DaemonKeyPairBundle`.
#[derive(Clone)]
pub struct DaemonKeyPair {
    pub key_pair: KeyPair,
    pub public_key_b64: String,
}

/// `KeyPairSchema`: `{ v: 2, publicKeyB64, secretKeyB64 }`, extra keys ignored.
fn read_stored(raw: &str) -> Result<DaemonKeyPair, String> {
    let value: Value = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let field = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| format!("{name} is missing or not a non-empty string"))
    };
    // `z.literal(2)` compares numbers, so `2.0` and `2e0` parse as 2 and are valid.
    if value.get("v").and_then(Value::as_f64) != Some(2.0) {
        return Err("v must be 2".to_owned());
    }
    let public_key =
        import_public_key(field("publicKeyB64")?).map_err(|error| error.to_string())?;
    let secret_key =
        import_secret_key(field("secretKeyB64")?).map_err(|error| error.to_string())?;
    let public_key_b64 = export_public_key(&public_key).map_err(|error| error.to_string())?;
    Ok(DaemonKeyPair {
        key_pair: KeyPair {
            public_key,
            secret_key,
        },
        public_key_b64,
    })
}

/// `loadOrCreateDaemonKeyPair`: reuse a valid file, otherwise generate a new
/// pair and write it as two-space indented JSON.
///
/// # Errors
///
/// Returns the write error when a new pair cannot be saved.
pub fn load_or_create_daemon_key_pair(
    paseo_home: &Path,
    logger: &dyn Logger,
) -> std::io::Result<DaemonKeyPair> {
    let file_path = paseo_home.join(KEYPAIR_FILENAME);
    let path_text = file_path.display().to_string();

    if file_path.exists() {
        ensure_private_file(&file_path);
        let loaded = fs::read(&file_path)
            .map_err(|error| error.to_string())
            .and_then(|raw| read_stored(&String::from_utf8_lossy(&raw)));
        match loaded {
            Ok(bundle) => {
                logger.info(&[("filePath", &path_text)], "Loaded daemon keypair");
                return Ok(bundle);
            }
            Err(error) => logger.warn(
                &[("err", &error), ("filePath", &path_text)],
                "Failed to load daemon keypair, regenerating",
            ),
        }
    }

    let key_pair = generate_key_pair();
    let encode = |result: Result<String, spocky_crypto::CryptoError>| {
        result.map_err(|error| std::io::Error::other(error.to_string()))
    };
    let public_key_b64 = encode(export_public_key(&key_pair.public_key))?;
    let secret_key_b64 = encode(export_secret_key(&key_pair.secret_key))?;
    let payload = format!(
        "{{\n  \"v\": 2,\n  \"publicKeyB64\": {},\n  \"secretKeyB64\": {}\n}}\n",
        Value::from(public_key_b64.as_str()),
        Value::from(secret_key_b64.as_str())
    );
    write_private_file_atomic(&file_path, payload.as_bytes())?;
    logger.info(&[("filePath", &path_text)], "Saved daemon keypair");
    Ok(DaemonKeyPair {
        key_pair,
        public_key_b64,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::log::NullLogger;
    use crate::log::testing::RecordingLogger;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn creates_a_private_file_in_the_pinned_text_form_and_reloads_it() {
        let home = tempfile::tempdir().unwrap();
        let created = load_or_create_daemon_key_pair(home.path(), &NullLogger).unwrap();
        let path = home.path().join("daemon-keypair.json");
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            format!(
                "{{\n  \"v\": 2,\n  \"publicKeyB64\": \"{}\",\n  \"secretKeyB64\": \"{}\"\n}}\n",
                created.public_key_b64,
                export_secret_key(&created.key_pair.secret_key).unwrap()
            )
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(created.public_key_b64.len(), 44);
        let again = load_or_create_daemon_key_pair(home.path(), &NullLogger).unwrap();
        assert!(again.key_pair == created.key_pair);
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn a_version_written_as_2_point_0_is_still_version_2() {
        let home = tempfile::tempdir().unwrap();
        let created = load_or_create_daemon_key_pair(home.path(), &NullLogger).unwrap();
        let path = home.path().join("daemon-keypair.json");
        let secret = export_secret_key(&created.key_pair.secret_key).unwrap();
        for version in ["2.0", "2e0", "2.00"] {
            fs::write(
                &path,
                format!(
                    "{{\"v\":{version},\"publicKeyB64\":\"{}\",\"secretKeyB64\":\"{secret}\"}}",
                    created.public_key_b64
                ),
            )
            .unwrap();
            let logger = RecordingLogger::default();
            let loaded = load_or_create_daemon_key_pair(home.path(), &logger).unwrap();
            assert!(loaded.key_pair == created.key_pair, "v {version}");
            assert_eq!(
                logger.messages(),
                [("info", "Loaded daemon keypair".to_owned())]
            );
        }
        fs::write(&path, r#"{"v":2.5,"publicKeyB64":"a","secretKeyB64":"b"}"#).unwrap();
        let logger = RecordingLogger::default();
        load_or_create_daemon_key_pair(home.path(), &logger).unwrap();
        assert_eq!(logger.messages()[0].0, "warn");
    }

    #[test]
    fn the_stored_secret_matches_the_public_key() {
        let home = tempfile::tempdir().unwrap();
        let created = load_or_create_daemon_key_pair(home.path(), &NullLogger).unwrap();
        let derived = spocky_crypto::key_pair_from_secret(created.key_pair.secret_key);
        assert_eq!(derived.public_key, created.key_pair.public_key);
    }

    #[test]
    fn an_invalid_file_is_replaced_with_a_warning() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("daemon-keypair.json");
        for bad in [
            "not json",
            r#"{"v":1,"publicKeyB64":"a","secretKeyB64":"b"}"#,
            r#"{"v":2,"publicKeyB64":"","secretKeyB64":"b"}"#,
            r#"{"v":2,"publicKeyB64":"AAAA","secretKeyB64":"AAAA"}"#,
        ] {
            fs::write(&path, bad).unwrap();
            let logger = RecordingLogger::default();
            let bundle = load_or_create_daemon_key_pair(home.path(), &logger).unwrap();
            assert_eq!(
                logger.messages(),
                [
                    (
                        "warn",
                        "Failed to load daemon keypair, regenerating".to_owned()
                    ),
                    ("info", "Saved daemon keypair".to_owned())
                ],
                "{bad}"
            );
            assert!(
                fs::read_to_string(&path)
                    .unwrap()
                    .contains(&bundle.public_key_b64)
            );
        }
    }

    #[test]
    fn a_group_readable_file_is_tightened_when_loaded() {
        let home = tempfile::tempdir().unwrap();
        load_or_create_daemon_key_pair(home.path(), &NullLogger).unwrap();
        let path = home.path().join("daemon-keypair.json");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        load_or_create_daemon_key_pair(home.path(), &NullLogger).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
