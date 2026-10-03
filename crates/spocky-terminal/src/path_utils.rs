//! The path checks the terminal manager and controller use, following pinned
//! `packages/server/src/server/path-utils.ts`.

/// `assertAbsolutePath` error text.
pub const NOT_ABSOLUTE_ERROR: &str = "cwd must be absolute path";

/// `path.win32.isAbsolute`: a leading separator, or a drive letter, a colon
/// and a separator.
fn is_win32_absolute(path: &str) -> bool {
    let mut units = path.encode_utf16();
    let (Some(first), second, third) = (units.next(), units.next(), units.next()) else {
        return false;
    };
    let separator = |unit: u16| unit == 0x2F || unit == 0x5C;
    if separator(first) {
        return true;
    }
    let drive = (0x41..=0x5A).contains(&first) || (0x61..=0x7A).contains(&first);
    drive && second == Some(0x3A) && third.is_some_and(separator)
}

/// `assertAbsolutePath(cwd)`: absolute in either POSIX or Windows terms.
///
/// # Errors
///
/// [`NOT_ABSOLUTE_ERROR`] for a relative path.
pub fn assert_absolute_path(cwd: &str) -> Result<(), &'static str> {
    if cwd.starts_with('/') || is_win32_absolute(cwd) {
        Ok(())
    } else {
        Err(NOT_ABSOLUTE_ERROR)
    }
}

/// `/^[a-zA-Z]:\//` on a path with forward slashes.
fn has_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

/// `.replace(/\\/g, "/").replace(/\/$/, "")`.
fn normalize(path: &str) -> String {
    let replaced = path.replace('\\', "/");
    replaced.strip_suffix('/').unwrap_or(&replaced).to_owned()
}

/// `isSameOrDescendantPath(basePath, candidatePath)`: Windows drive paths
/// compare case-insensitively.
#[must_use]
pub fn is_same_or_descendant_path(base: &str, candidate: &str) -> bool {
    let mut base = normalize(base);
    let mut candidate = normalize(candidate);
    if has_drive_prefix(&base) || has_drive_prefix(&candidate) {
        base = base.to_lowercase();
        candidate = candidate.to_lowercase();
    }
    candidate == base || candidate.starts_with(&format!("{base}/"))
}
