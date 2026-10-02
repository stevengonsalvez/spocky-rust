//! `providers/claude/project-dir.ts`: Claude Code's config directory and
//! its `projects/<encoded cwd>` transcript directory, plus POSIX
//! `path.join`.

use spocky_contracts::js_value::{JsObject, JsValue, js_text_utf16};
use unicode_normalization::UnicodeNormalization;

const PROJECT_DIR_LENGTH_CAP: usize = 200;

/// `path.posix.normalize(path)`.
#[must_use]
pub fn normalize_path(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let absolute = path.starts_with('/');
    let trailing = path.ends_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let mut joined = parts.join("/");
    if joined.is_empty() && !absolute {
        joined.push('.');
    }
    if trailing && !joined.is_empty() {
        joined.push('/');
    }
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// `path.join(left, right)`.
#[must_use]
pub fn join_path(left: &str, right: &str) -> String {
    let joined = [left, right]
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    normalize_path(&joined)
}

/// `claudeConfigDir(env)`: `CLAUDE_CONFIG_DIR`, else `~/.claude`.
#[must_use]
pub fn claude_config_dir(env: &JsObject) -> String {
    match env.get("CLAUDE_CONFIG_DIR") {
        Some(JsValue::String(dir)) => dir.clone(),
        _ => join_path(&home_dir(), ".claude"),
    }
}

/// `os.homedir()`: `$HOME` of the daemon.
#[must_use]
pub fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// `realpathSync.native(input)` with the input as fallback, NFC on macOS.
fn canonicalize(input: &str) -> String {
    let resolved = std::fs::canonicalize(input).map_or_else(
        |_| input.to_owned(),
        |path| path.to_string_lossy().into_owned(),
    );
    if cfg!(target_os = "macos") {
        resolved.nfc().collect()
    } else {
        resolved
    }
}

/// The SDK's 32-bit string hash, base 36.
fn hash_suffix(input: &str) -> String {
    let mut hash: i32 = 0;
    for unit in js_text_utf16(input) {
        hash = hash
            .wrapping_shl(5)
            .wrapping_sub(hash)
            .wrapping_add(i32::from(unit));
    }
    let mut value = u64::from(hash.unsigned_abs());
    if value == 0 {
        return "0".to_owned();
    }
    let mut digits = Vec::new();
    while value > 0 {
        let digit = u8::try_from(value % 36).unwrap_or(0);
        digits.push(char::from(if digit < 10 {
            b'0' + digit
        } else {
            b'a' + digit - 10
        }));
        value /= 36;
    }
    digits.iter().rev().collect()
}

/// `encode(input)`: non-alphanumerics become `-`, capped at 200 code
/// units plus a hash.
fn encode(input: &str) -> String {
    let replaced: Vec<u16> = js_text_utf16(input)
        .map(|unit| {
            if u8::try_from(unit).is_ok_and(|byte| byte.is_ascii_alphanumeric()) {
                unit
            } else {
                u16::from(b'-')
            }
        })
        .collect();
    let text: String = String::from_utf16_lossy(&replaced);
    if replaced.len() <= PROJECT_DIR_LENGTH_CAP {
        return text;
    }
    format!(
        "{}-{}",
        String::from_utf16_lossy(&replaced[..PROJECT_DIR_LENGTH_CAP]),
        hash_suffix(input)
    )
}

/// `claudeProjectDirSync(cwd, { configDir })`.
#[must_use]
pub fn claude_project_dir(cwd: &str, config_dir: &str) -> String {
    join_path(
        &join_path(config_dir, "projects"),
        &encode(&canonicalize(cwd)),
    )
}

#[cfg(test)]
mod tests {
    use super::{claude_project_dir, join_path, normalize_path};

    // node: path.join / path.normalize and the SDK project encoding.
    #[test]
    fn paths_follow_node() {
        assert_eq!(join_path("/a/b/", "../c"), "/a/c");
        assert_eq!(join_path("", "x"), "x");
        assert_eq!(normalize_path("a/./b//"), "a/b/");
        assert_eq!(normalize_path("/.."), "/");
        assert_eq!(normalize_path("../a/.."), "..");
        assert_eq!(
            claude_project_dir("/nonexistent/My Repo_1", "/cfg"),
            "/cfg/projects/-nonexistent-My-Repo-1"
        );
        let long = format!("/nonexistent/{}", "x".repeat(300));
        assert_eq!(
            claude_project_dir(&long, "/cfg"),
            format!(
                "/cfg/projects/-nonexistent-{}-{}",
                "x".repeat(187),
                "o0odul"
            )
        );
    }
}
