//! Stable daemon identifier scoped to a home directory.
//!
//! Source at Paseo `5de45e2`: `server-id.ts`.

use std::fs;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::js;
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
#[must_use]
pub fn get_or_create_server_id(paseo_home: &Path, env_override: Option<&str>) -> String {
    let server_id_path = paseo_home.join(SERVER_ID_FILENAME);

    if let Some(server_id) = env_override
        .map(js::trim)
        .filter(|server_id| !server_id.is_empty())
    {
        if server_id_path.exists() {
            ensure_private_file(&server_id_path);
        } else {
            let _ = write_private_file_atomic(&server_id_path, format!("{server_id}\n").as_bytes());
        }
        return server_id.to_owned();
    }

    if server_id_path.exists() {
        ensure_private_file(&server_id_path);
        if let Ok(raw) = fs::read_to_string(&server_id_path) {
            let parsed = js::trim(&raw);
            if !parsed.is_empty() {
                return parsed.to_owned();
            }
        }
    }

    let created = generate_server_id();
    let _ = write_private_file_atomic(&server_id_path, format!("{created}\n").as_bytes());
    created
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_a_persisted_srv_id_then_returns_it_again() {
        let home = tempfile::tempdir().unwrap();
        let first = get_or_create_server_id(home.path(), None);
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
        assert_eq!(get_or_create_server_id(home.path(), None), first);
    }

    #[test]
    fn the_environment_override_is_trimmed_and_persisted_once() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(
            get_or_create_server_id(home.path(), Some("  srv_fixed \n")),
            "srv_fixed"
        );
        assert_eq!(
            fs::read_to_string(home.path().join("server-id")).unwrap(),
            "srv_fixed\n"
        );
        assert_eq!(
            get_or_create_server_id(home.path(), Some("srv_other")),
            "srv_other"
        );
        assert_eq!(
            fs::read_to_string(home.path().join("server-id")).unwrap(),
            "srv_fixed\n"
        );
    }

    #[test]
    fn a_blank_override_is_ignored() {
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join("server-id"), "srv_disk\n").unwrap();
        assert_eq!(
            get_or_create_server_id(home.path(), Some("   ")),
            "srv_disk"
        );
    }

    #[test]
    fn an_empty_file_is_regenerated() {
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join("server-id"), "  \n").unwrap();
        let id = get_or_create_server_id(home.path(), None);
        assert!(id.starts_with("srv_"));
        assert_eq!(
            fs::read_to_string(home.path().join("server-id")).unwrap(),
            format!("{id}\n")
        );
    }

    #[test]
    fn an_unreadable_path_still_yields_an_in_memory_id() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join("server-id")).unwrap();
        let id = get_or_create_server_id(home.path(), None);
        assert!(id.starts_with("srv_"));
    }
}
