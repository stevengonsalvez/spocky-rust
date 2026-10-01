//! Stable daemon identifier scoped to a home directory.
//!
//! Source at Paseo `5de45e2`: `server-id.ts`.

use std::fs;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::js;
use crate::log::Logger;
use crate::private_files::{ensure_private_file, write_private_file_atomic};

const SERVER_ID_FILENAME: &str = "server-id";

/// `generateServerId`: `srv_` and 9 random bytes as 12 base64url characters.
fn generate_server_id() -> String {
    let mut bytes = [0_u8; 9];
    getrandom::fill(&mut bytes).expect("operating system randomness is available");
    format!("srv_{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// `getOrCreateServerId`. `env_override` is `PASEO_SERVER_ID`. A write or read
/// failure is not an error: the id then lives only in memory or is regenerated.
/// The file is read as bytes and decoded lossily, as Node decodes UTF-8.
#[must_use]
pub fn get_or_create_server_id(
    paseo_home: &Path,
    env_override: Option<&str>,
    logger: &dyn Logger,
) -> String {
    let server_id_path = paseo_home.join(SERVER_ID_FILENAME);

    if let Some(server_id) = env_override
        .map(js::trim)
        .filter(|server_id| !server_id.is_empty())
    {
        if server_id_path.exists() {
            ensure_private_file(&server_id_path);
        } else {
            match write_private_file_atomic(&server_id_path, format!("{server_id}\n").as_bytes()) {
                Ok(()) => logger.info(
                    &[("serverId", server_id)],
                    "Persisted PASEO_SERVER_ID override",
                ),
                Err(error) => logger.warn(
                    &[("error", &error.to_string())],
                    "Failed to persist PASEO_SERVER_ID override",
                ),
            }
        }
        return server_id.to_owned();
    }

    if server_id_path.exists() {
        ensure_private_file(&server_id_path);
        match fs::read(&server_id_path) {
            Ok(raw) => {
                let decoded = String::from_utf8_lossy(&raw);
                let parsed = js::trim(&decoded);
                if !parsed.is_empty() {
                    return parsed.to_owned();
                }
            }
            Err(error) => logger.warn(
                &[("error", &error.to_string())],
                "Failed to read server-id file, regenerating",
            ),
        }
    }

    let created = generate_server_id();
    if let Err(error) =
        write_private_file_atomic(&server_id_path, format!("{created}\n").as_bytes())
    {
        logger.warn(
            &[("error", &error.to_string())],
            "Failed to persist serverId (continuing with in-memory id)",
        );
    }
    created
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::NullLogger;
    use crate::log::testing::RecordingLogger;

    fn get(home: &Path, env_override: Option<&str>) -> String {
        get_or_create_server_id(home, env_override, &NullLogger)
    }

    #[test]
    fn creates_a_persisted_srv_id_then_returns_it_again() {
        let home = tempfile::tempdir().unwrap();
        let first = get(home.path(), None);
        assert!(first.starts_with("srv_"));
        assert_eq!(first.len(), 16);
        assert!(
            first[4..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        assert_eq!(
            fs::read_to_string(home.path().join("server-id")).unwrap(),
            format!("{first}\n")
        );
        assert_eq!(get(home.path(), None), first);
    }

    #[test]
    fn the_environment_override_is_trimmed_and_persisted_once() {
        let home = tempfile::tempdir().unwrap();
        let logger = RecordingLogger::default();
        assert_eq!(
            get_or_create_server_id(home.path(), Some("  srv_fixed \n"), &logger),
            "srv_fixed"
        );
        assert_eq!(
            logger.messages(),
            [("info", "Persisted PASEO_SERVER_ID override".to_owned())]
        );
        assert_eq!(
            fs::read_to_string(home.path().join("server-id")).unwrap(),
            "srv_fixed\n"
        );
        assert_eq!(get(home.path(), Some("srv_other")), "srv_other");
        assert_eq!(
            fs::read_to_string(home.path().join("server-id")).unwrap(),
            "srv_fixed\n"
        );
    }

    #[test]
    fn a_blank_override_is_ignored() {
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join("server-id"), "srv_disk\n").unwrap();
        assert_eq!(get(home.path(), Some("   ")), "srv_disk");
    }

    #[test]
    fn an_empty_file_is_regenerated() {
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join("server-id"), "  \n").unwrap();
        let id = get(home.path(), None);
        assert!(id.starts_with("srv_"));
        assert_eq!(
            fs::read_to_string(home.path().join("server-id")).unwrap(),
            format!("{id}\n")
        );
    }

    #[test]
    fn a_non_utf8_file_is_decoded_lossily_not_regenerated() {
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join("server-id"), b"srv_\xff\xfe_x\n").unwrap();
        assert_eq!(get(home.path(), None), "srv_\u{fffd}\u{fffd}_x");
        assert_eq!(
            fs::read(home.path().join("server-id")).unwrap(),
            b"srv_\xff\xfe_x\n"
        );
    }

    #[test]
    fn an_unreadable_path_still_yields_an_in_memory_id_and_warns() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join("server-id")).unwrap();
        let logger = RecordingLogger::default();
        let id = get_or_create_server_id(home.path(), None, &logger);
        assert!(id.starts_with("srv_"));
        assert_eq!(
            logger.messages(),
            [
                (
                    "warn",
                    "Failed to read server-id file, regenerating".to_owned()
                ),
                (
                    "warn",
                    "Failed to persist serverId (continuing with in-memory id)".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn a_failed_override_write_warns_and_still_returns_the_override() {
        let home = tempfile::tempdir().unwrap();
        let blocker = home.path().join("blocked");
        fs::write(&blocker, "file").unwrap();
        let logger = RecordingLogger::default();
        let id = get_or_create_server_id(&blocker, Some("srv_o"), &logger);
        assert_eq!(id, "srv_o");
        assert_eq!(
            logger.messages(),
            [(
                "warn",
                "Failed to persist PASEO_SERVER_ID override".to_owned()
            )]
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_group_readable_file_is_repaired_to_0600_on_read_and_on_override() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let home = tempfile::tempdir().unwrap();
        let file = home.path().join("server-id");
        fs::write(&file, "srv_disk\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(get(home.path(), None), "srv_disk");
        assert_eq!(mode(&file), 0o600);
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(get(home.path(), Some("srv_env")), "srv_env");
        assert_eq!(mode(&file), 0o600);
        assert_eq!(fs::read_to_string(&file).unwrap(), "srv_disk\n");
    }
}
