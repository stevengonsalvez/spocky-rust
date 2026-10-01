//! Per-run credential that lets a local client prove it can read the home.
//!
//! Source at Paseo `5de45e2`: `local-credential.ts` (the daemon side:
//! `writeLocalCredential`, `deleteLocalCredential`, `readLocalCredential` and
//! `matchesLocalCredential`).

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::js;
use crate::private_files::PRIVATE_FILE_MODE;

const FILE_NAME: &str = "local-credential";
const TOKEN_LENGTH: usize = 43;

/// `writeLocalCredential`: 32 random bytes as base64url, written exclusively to
/// `local-credential.<uuid>.tmp` with mode 0600, then renamed into place.
///
/// # Errors
///
/// Returns the first error from creating, writing, or renaming the file.
pub fn write_local_credential(home: &Path) -> io::Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let temporary = home.join(format!("{FILE_NAME}.{}.tmp", Uuid::new_v4()));
    let result = write_exclusive(&temporary, format!("{token}\n").as_bytes())
        .and_then(|()| fs::rename(&temporary, home.join(FILE_NAME)));
    let cleanup = remove_if_present(&temporary);
    result?;
    cleanup?;
    Ok(token)
}

fn write_exclusive(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(PRIVATE_FILE_MODE);
        let mut file = options.open(path)?;
        file.write_all(data)?;
        file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
    }
    #[cfg(not(unix))]
    {
        let _ = PRIVATE_FILE_MODE;
        options.open(path)?.write_all(data)
    }
}

/// `unlink(...).catch` that ignores only `ENOENT`.
fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// `deleteLocalCredential`.
///
/// # Errors
///
/// Returns any removal error other than the file already being absent.
pub fn delete_local_credential(home: &Path) -> io::Result<()> {
    remove_if_present(&home.join(FILE_NAME))
}

/// `readLocalCredential`: the trimmed file content when it is 43 base64url
/// characters, otherwise `None`.
#[must_use]
pub fn read_local_credential(home: &Path) -> Option<String> {
    let raw = fs::read_to_string(home.join(FILE_NAME)).ok()?;
    let token = js::trim(&raw);
    (token.len() == TOKEN_LENGTH
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
    .then(|| token.to_owned())
}

/// `matchesLocalCredential`: equal length, then a constant-time comparison.
#[must_use]
pub fn matches_local_credential(expected: &str, candidate: &str) -> bool {
    let (expected, candidate) = (expected.as_bytes(), candidate.as_bytes());
    expected.len() == candidate.len() && bool::from(expected.ct_eq(candidate))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn writes_a_private_43_character_token_and_leaves_no_temporary_file() {
        let home = tempfile::tempdir().unwrap();
        let token = write_local_credential(home.path()).unwrap();
        assert_eq!(token.len(), 43);
        let file = home.path().join("local-credential");
        assert_eq!(fs::read_to_string(&file).unwrap(), format!("{token}\n"));
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(home.path()).unwrap().count(), 1);
        assert_eq!(read_local_credential(home.path()), Some(token));
    }

    #[test]
    fn a_second_write_replaces_the_token() {
        let home = tempfile::tempdir().unwrap();
        let first = write_local_credential(home.path()).unwrap();
        let second = write_local_credential(home.path()).unwrap();
        assert_ne!(first, second);
        assert_eq!(read_local_credential(home.path()), Some(second));
    }

    #[test]
    fn delete_removes_the_file_and_ignores_a_missing_one() {
        let home = tempfile::tempdir().unwrap();
        write_local_credential(home.path()).unwrap();
        delete_local_credential(home.path()).unwrap();
        assert_eq!(read_local_credential(home.path()), None);
        delete_local_credential(home.path()).unwrap();
    }

    #[test]
    fn rejects_files_that_are_not_a_43_character_token() {
        let home = tempfile::tempdir().unwrap();
        let file = home.path().join("local-credential");
        for bad in [
            "",
            "short",
            &"a".repeat(44),
            &format!("{}!", "a".repeat(42)),
        ] {
            fs::write(&file, bad).unwrap();
            assert_eq!(read_local_credential(home.path()), None, "{bad}");
        }
        let good = "A_-".repeat(14) + "ab";
        assert_eq!(good.len(), 44);
        let good = &good[..43];
        fs::write(&file, format!("  {good}\r\n")).unwrap();
        assert_eq!(read_local_credential(home.path()).as_deref(), Some(good));
    }

    #[test]
    fn matching_needs_equal_length_and_content() {
        assert!(matches_local_credential("abc", "abc"));
        assert!(!matches_local_credential("abc", "abd"));
        assert!(!matches_local_credential("abc", "abcd"));
        assert!(!matches_local_credential("abc", ""));
    }
}
