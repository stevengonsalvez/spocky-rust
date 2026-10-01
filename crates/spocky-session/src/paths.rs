//! Path helpers from node `path.posix` and pinned Paseo `utils/path.ts`.

// ponytail: POSIX semantics only. Windows-shaped paths (drive letters, UNC)
// need node `path.win32` ports before Windows hosts are qualified.

use std::path::Path;

use spocky_store::path_compare::posix_normalize;

/// node `path.posix.resolve(base, path)` for an absolute `base`.
#[must_use]
pub fn resolve(base: &str, path: &str) -> String {
    let joined = if path.starts_with('/') {
        path.to_owned()
    } else if path.is_empty() {
        base.to_owned()
    } else {
        format!("{base}/{path}")
    };
    strip_trailing_slash(posix_normalize(&joined))
}

/// node `path.posix.resolve(path)`, relative to the process working directory.
#[must_use]
pub fn resolve_from_cwd(path: &str) -> String {
    if path.starts_with('/') {
        return resolve("/", path);
    }
    let cwd = std::env::current_dir().map_or_else(
        |_| "/".to_owned(),
        |directory| directory.to_string_lossy().into_owned(),
    );
    resolve(&cwd, path)
}

fn strip_trailing_slash(mut value: String) -> String {
    while value.len() > 1 && value.ends_with('/') {
        value.pop();
    }
    value
}

/// node `path.posix.basename(path)`.
#[must_use]
pub fn basename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or_default().to_owned()
}

/// node `path.posix.dirname(path)`, including its non-collapsing slash rules.
#[must_use]
pub fn dirname(path: &str) -> String {
    let bytes = path.as_bytes();
    if bytes.is_empty() {
        return ".".to_owned();
    }
    let has_root = bytes[0] == b'/';
    let mut end = None;
    let mut matched_slash = true;
    for index in (1..bytes.len()).rev() {
        if bytes[index] == b'/' {
            if !matched_slash {
                end = Some(index);
                break;
            }
        } else {
            matched_slash = false;
        }
    }
    match end {
        None if has_root => "/".to_owned(),
        None => ".".to_owned(),
        Some(1) if has_root => "//".to_owned(),
        Some(index) => path[..index].to_owned(),
    }
}

/// `expandTilde`: only `~` and a leading `~/` expand, using `$HOME`.
#[must_use]
pub fn expand_tilde(path: &str, home: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        return format!("{home}/{rest}");
    }
    if path == "~" {
        return home.to_owned();
    }
    path.to_owned()
}

/// `realpathSync`; `None` where it throws.
#[must_use]
pub fn realpath(path: &str) -> Option<String> {
    std::fs::canonicalize(Path::new(path))
        .ok()
        .map(|resolved| resolved.to_string_lossy().into_owned())
}

/// `collectPathVariants`: the path, then its realpath when different.
#[must_use]
pub fn path_variants(path: &str) -> Vec<String> {
    let mut variants = vec![path.to_owned()];
    if let Some(real) = realpath(path)
        && real != path
    {
        variants.push(real);
    }
    variants
}

/// Normalized comparison form: `path.posix.normalize` without trailing separators.
fn comparable(path: &str) -> String {
    let normalized = posix_normalize(path);
    let root = usize::from(normalized.starts_with('/'));
    let mut result = normalized;
    while result.len() > root && result.ends_with('/') {
        result.pop();
    }
    result
}

/// node `path.posix.relative(from, to)` for normalized absolute paths.
fn relative(from: &str, to: &str) -> String {
    if from == to {
        return String::new();
    }
    let from_parts: Vec<&str> = from.split('/').filter(|part| !part.is_empty()).collect();
    let to_parts: Vec<&str> = to.split('/').filter(|part| !part.is_empty()).collect();
    let common = from_parts
        .iter()
        .zip(&to_parts)
        .take_while(|(left, right)| left == right)
        .count();
    let mut parts: Vec<&str> = vec![".."; from_parts.len() - common];
    parts.extend(&to_parts[common..]);
    parts.join("/")
}

/// `getRelativePathInsideRoot`: `Some("")` when equal, `None` when outside.
#[must_use]
pub fn relative_path_inside_root(root: &str, candidate: &str) -> Option<String> {
    let root = comparable(root);
    let candidate = comparable(candidate);
    let relative = relative(&root, &candidate);
    if !relative.is_empty() && (relative.starts_with("..") || relative.starts_with('/')) {
        return None;
    }
    Some(relative)
}

/// `getRealpathAwareRelativePath`: tries every realpath variant pair.
#[must_use]
pub fn realpath_aware_relative_path(root: &str, candidate: &str) -> Option<String> {
    for root_variant in path_variants(root) {
        for candidate_variant in path_variants(candidate) {
            if let Some(relative) = relative_path_inside_root(&root_variant, &candidate_variant) {
                return Some(relative);
            }
        }
    }
    None
}

/// `normalizePathForIdentity`: the realpath when one exists, normalized.
#[must_use]
pub fn normalize_path_for_identity(path: &str) -> String {
    comparable(&realpath(path).unwrap_or_else(|| path.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::{basename, dirname, expand_tilde, relative_path_inside_root, resolve};

    // Expected values printed by node v22 `path.posix`.
    #[test]
    fn posix_helpers_match_node() {
        assert_eq!(resolve("/a/b", "c/../d/"), "/a/b/d");
        assert_eq!(resolve("/a/b", "/x/./y/"), "/x/y");
        assert_eq!(resolve("/a/b", ""), "/a/b");
        assert_eq!(resolve("/", "."), "/");
        assert_eq!(basename("/tmp/project/"), "project");
        assert_eq!(basename("/"), "");
        assert_eq!(dirname("/tmp/project/.git"), "/tmp/project");
        assert_eq!(dirname("/x"), "/");
        assert_eq!(dirname("a"), ".");
        assert_eq!(dirname("//a"), "//");
        assert_eq!(dirname("/a//b"), "/a/");
        assert_eq!(expand_tilde("~/p", "/h"), "/h/p");
        assert_eq!(expand_tilde("~", "/h"), "/h");
        assert_eq!(expand_tilde("~x/p", "/h"), "~x/p");
    }

    #[test]
    fn relative_inside_root() {
        assert_eq!(
            relative_path_inside_root("/r", "/r/a/b/").as_deref(),
            Some("a/b")
        );
        assert_eq!(relative_path_inside_root("/r/", "/r").as_deref(), Some(""));
        assert_eq!(relative_path_inside_root("/r", "/rx"), None);
        assert_eq!(relative_path_inside_root("/r/a", "/r"), None);
    }
}
