//! String-only path equivalence from pinned Paseo `utils/path.ts`
//! (`areEquivalentPaths`): normalize separators and dot segments, drop
//! trailing separators, and case-fold only when either side looks like a
//! Windows path. Symlinks are not resolved.

use spocky_contracts::text::js_to_lowercase;

/// `areEquivalentPaths`.
#[must_use]
pub fn are_equivalent_paths(left: &str, right: &str) -> bool {
    let windows = looks_like_definite_windows_path(left) || looks_like_definite_windows_path(right);
    normalize_for_comparison(left, windows) == normalize_for_comparison(right, windows)
}

/// `looksLikeDefiniteWindowsPath`: a drive letter, a `\\?\` device namespace, or UNC.
#[must_use]
pub fn looks_like_definite_windows_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    let is_separator = |byte: u8| matches!(byte, b'/' | b'\\');
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && is_separator(bytes[2]);
    let device = bytes.len() >= 4
        && is_separator(bytes[0])
        && is_separator(bytes[1])
        && bytes[2] == b'?'
        && is_separator(bytes[3]);
    drive || device || looks_like_unc(bytes)
}

/// `/^\\{2}[^/\\]+[/\\][^/\\]+/u`.
fn looks_like_unc(bytes: &[u8]) -> bool {
    if bytes.len() < 2 || bytes[0] != b'\\' || bytes[1] != b'\\' {
        return false;
    }
    let rest = &bytes[2..];
    let server = rest
        .iter()
        .take_while(|byte| !matches!(byte, b'/' | b'\\'))
        .count();
    server > 0 && rest.len() > server + 1 && !matches!(rest[server + 1], b'/' | b'\\')
}

fn normalize_for_comparison(value: &str, windows: bool) -> String {
    if windows {
        normalize_windows_for_comparison(value)
    } else {
        let normalized = posix_normalize(value);
        let root_length = usize::from(normalized.starts_with('/'));
        strip_trailing(&normalized, root_length, |character| character == '/')
    }
}

fn strip_trailing(value: &str, root_length: usize, is_separator: impl Fn(char) -> bool) -> String {
    let mut result = value;
    while result.len() > root_length && result.ends_with(&is_separator) {
        result = &result[..result.len() - 1];
    }
    result.to_owned()
}

// ponytail: Windows-shaped paths use a reduced form of node `path.win32.normalize`
// (namespace strip, separator unify, dot segments, case fold) without device-name
// edge cases; port node's full win32 routine when Windows hosts are qualified.
fn normalize_windows_for_comparison(value: &str) -> String {
    let stripped = strip_windows_namespace_prefix(value);
    let unified = stripped.replace('/', "\\");
    let (root, rest) = split_windows_root(&unified);
    let tail = normalize_segments(rest, '\\', root.is_empty());
    let joined = if root.is_empty() && tail.is_empty() {
        ".".to_owned()
    } else {
        format!("{root}{tail}")
    };
    js_to_lowercase(&strip_trailing(&joined, root.len(), |character| {
        character == '\\'
    }))
}

fn strip_windows_namespace_prefix(value: &str) -> String {
    let bytes = value.as_bytes();
    let is_separator = |byte: u8| matches!(byte, b'/' | b'\\');
    if bytes.len() >= 4
        && is_separator(bytes[0])
        && is_separator(bytes[1])
        && bytes[2] == b'?'
        && is_separator(bytes[3])
    {
        let rest = &value[4..];
        let rest_bytes = rest.as_bytes();
        if rest_bytes.len() >= 3
            && rest_bytes[0].is_ascii_alphabetic()
            && rest_bytes[1] == b':'
            && is_separator(rest_bytes[2])
        {
            return format!("{}\\{}", &rest[..2], &rest[3..]);
        }
        if rest.len() >= 4 && rest[..3].eq_ignore_ascii_case("UNC") && is_separator(rest_bytes[3]) {
            let unc = &rest[4..];
            let mut parts = unc.splitn(3, ['/', '\\']);
            if let (Some(server), Some(share)) = (parts.next(), parts.next())
                && !server.is_empty()
                && !share.is_empty()
            {
                return match parts.next() {
                    Some(tail) => format!("\\\\{server}\\{share}\\{tail}"),
                    None => format!("\\\\{server}\\{share}"),
                };
            }
        }
    }
    value.to_owned()
}

fn split_windows_root(value: &str) -> (String, &str) {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        if bytes.get(2) == Some(&b'\\') {
            return (value[..3].to_owned(), &value[3..]);
        }
        return (value[..2].to_owned(), &value[2..]);
    }
    if let Some(unc) = value.strip_prefix("\\\\") {
        let mut parts = unc.splitn(3, '\\');
        if let (Some(server), Some(share)) = (parts.next(), parts.next())
            && !server.is_empty()
            && !share.is_empty()
        {
            let root = format!("\\\\{server}\\{share}\\");
            let consumed = (2 + server.len() + 1 + share.len() + 1).min(value.len());
            return (root, &value[consumed..]);
        }
    }
    if let Some(rest) = value.strip_prefix('\\') {
        return ("\\".to_owned(), rest);
    }
    (String::new(), value)
}

/// node `path.posix.normalize`.
#[must_use]
pub fn posix_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let is_absolute = path.starts_with('/');
    let trailing_separator = path.ends_with('/');
    let normalized = normalize_segments(path, '/', !is_absolute);
    if normalized.is_empty() {
        if is_absolute {
            return "/".to_owned();
        }
        return if trailing_separator { "./" } else { "." }.to_owned();
    }
    let mut result = String::with_capacity(normalized.len() + 2);
    if is_absolute {
        result.push('/');
    }
    result.push_str(&normalized);
    if trailing_separator {
        result.push('/');
    }
    result
}

/// node `normalizeString`: resolves `.` and `..` segments and collapses separators.
fn normalize_segments(path: &str, separator: char, allow_above_root: bool) -> String {
    let mut result = String::new();
    let mut last_segment_length = 0_usize;
    let mut last_slash: isize = -1;
    let mut dots: i32 = 0;
    let bytes = path.as_bytes();
    let separator_byte = u8::try_from(u32::from(separator)).unwrap_or(b'/');
    let length = isize::try_from(bytes.len()).unwrap_or(isize::MAX);
    let mut code = 0_u8;
    let mut index: isize = 0;
    while index <= length {
        if index < length {
            code = bytes[index.unsigned_abs()];
        } else if code == separator_byte {
            break;
        } else {
            code = separator_byte;
        }
        if code == separator_byte {
            if last_slash == index - 1 || dots == 1 {
                // A repeated separator or a `.` segment adds nothing.
            } else if dots == 2 {
                let ends_with_parent =
                    result.len() >= 2 && last_segment_length == 2 && result.ends_with("..");
                if !ends_with_parent {
                    if result.len() > 2 {
                        match result.rfind(separator) {
                            None => {
                                result.clear();
                                last_segment_length = 0;
                            }
                            Some(position) => {
                                result.truncate(position);
                                last_segment_length = match result.rfind(separator) {
                                    Some(found) => result.len() - 1 - found,
                                    None => result.len(),
                                };
                            }
                        }
                        last_slash = index;
                        dots = 0;
                        index += 1;
                        continue;
                    } else if !result.is_empty() {
                        result.clear();
                        last_segment_length = 0;
                        last_slash = index;
                        dots = 0;
                        index += 1;
                        continue;
                    }
                }
                if allow_above_root {
                    if !result.is_empty() {
                        result.push(separator);
                    }
                    result.push_str("..");
                    last_segment_length = 2;
                }
            } else {
                let start = (last_slash + 1).unsigned_abs();
                let segment = &path[start..index.unsigned_abs()];
                if !result.is_empty() {
                    result.push(separator);
                }
                result.push_str(segment);
                last_segment_length = (index - last_slash - 1).unsigned_abs();
            }
            last_slash = index;
            dots = 0;
        } else if code == b'.' && dots != -1 {
            dots += 1;
        } else {
            dots = -1;
        }
        index += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{are_equivalent_paths, posix_normalize};

    #[test]
    fn posix_normalize_matches_node() {
        for (input, expected) in [
            ("", "."),
            ("/", "/"),
            ("//", "/"),
            ("/a/b/../c/./d", "/a/c/d"),
            ("/a//b/", "/a/b/"),
            ("a/../..", ".."),
            ("a/b/../../..", ".."),
            ("../a/..", ".."),
            ("./", "./"),
            (".", "."),
            ("/..", "/"),
            ("/a/..", "/"),
            ("a/..", "."),
            ("/foo/bar//baz/asdf/quux/..", "/foo/bar/baz/asdf"),
            ("../../x", "../../x"),
            ("/a/b/c/../../..", "/"),
            ("ab/..cd/..", "ab"),
            ("/a/.../b", "/a/.../b"),
        ] {
            assert_eq!(posix_normalize(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn posix_equivalence_ignores_trailing_and_dot_segments_but_keeps_case() {
        assert!(are_equivalent_paths("/tmp/project/", "/tmp/project"));
        assert!(are_equivalent_paths("/tmp/./project", "/tmp/x/../project"));
        assert!(are_equivalent_paths("/", "//"));
        assert!(!are_equivalent_paths("/tmp/Project", "/tmp/project"));
        assert!(!are_equivalent_paths("/tmp/project", "tmp/project"));
    }

    #[test]
    fn windows_equivalence_folds_case_and_separators() {
        assert!(are_equivalent_paths("C:\\Repo\\App\\", "c:/repo/app"));
        assert!(are_equivalent_paths("\\\\?\\C:\\Repo", "C:\\repo"));
        assert!(are_equivalent_paths(
            "\\\\server\\share\\x",
            "//server/share/X"
        ));
        assert!(!are_equivalent_paths("C:\\repo", "D:\\repo"));
    }

    /// `areEquivalentPaths` ends in `toLowerCase`, which follows node's
    /// Unicode 16 tables: node v22.20.0 printed `false`, `true`, `true`,
    /// `true`, `true` for these pairs. U+A7CE has no lowercase there, and
    /// `str::to_lowercase` (a newer Unicode) maps it to U+A7CF.
    #[test]
    fn windows_case_fold_follows_nodes_unicode_tables() {
        assert!(!are_equivalent_paths("C:\\\u{a7ce}", "C:\\\u{a7cf}"));
        assert!(are_equivalent_paths("C:\\\u{130}", "c:\\i\u{307}"));
        assert!(are_equivalent_paths("C:\\\u{1e9e}", "c:\\\u{df}"));
        assert!(are_equivalent_paths("C:\\\u{3a3}", "c:\\\u{3c3}"));
        assert!(are_equivalent_paths("C:\\a\u{3a3}", "c:\\a\u{3c2}"));
    }
}
