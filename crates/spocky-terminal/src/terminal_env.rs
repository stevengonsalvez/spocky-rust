//! The terminal child environment, following pinned
//! `buildTerminalEnvironment` in `packages/server/src/terminal/terminal.ts`
//! and `createExternalProcessEnv` in `packages/server/src/server/paseo-env.ts`.
//!
//! Environments are [`JsObject`]s so key order follows JavaScript objects:
//! array-index keys first, then insertion order, and an assigned existing key
//! keeps its position. A value of [`JsValue::Undefined`] is an unset overlay.

use std::path::Path;

use spocky_contracts::js_value::{JsObject, JsValue};

/// Keys `createExternalProcessEnv` removes so runtime control never leaks
/// into user processes.
pub const RUNTIME_CONTROL_ENV_KEYS: [&str; 6] = [
    "PASEO_NODE_ENV",
    "PASEO_DESKTOP_MANAGED",
    "PASEO_SUPERVISED",
    "ELECTRON_RUN_AS_NODE",
    "ELECTRON_NO_ATTACH_CONSOLE",
    "ESBUILD_BINARY_PATH",
];

/// `createExternalProcessEnv(baseEnv, ...overlays)`.
#[must_use]
pub fn external_process_env(base: &JsObject, overlays: &[&JsObject]) -> JsObject {
    let mut merged = JsObject::new();
    for source in std::iter::once(base).chain(overlays.iter().copied()) {
        for (key, value) in source.iter() {
            merged.insert(key, value.clone());
        }
    }
    let mut sanitized = JsObject::new();
    for (key, value) in merged.iter() {
        if RUNTIME_CONTROL_ENV_KEYS.contains(&key) || matches!(value, JsValue::Undefined) {
            continue;
        }
        sanitized.insert(key, value.clone());
    }
    sanitized
}

/// Inputs of `buildTerminalEnvironment`. The CLI locations are resolved by
/// the caller (`resolvePaseoCliBinDir` and `resolvePaseoCliExecutablePath`);
/// `None` is the baseline's `null`.
#[derive(Debug, Clone, Copy)]
pub struct TerminalEnvironmentInput<'a> {
    pub shell: &'a str,
    /// `process.env` of the daemon, in its enumeration order.
    pub process_env: &'a JsObject,
    pub env: &'a JsObject,
    pub paseo_cli_bin_dir: Option<&'a str>,
    pub paseo_hook_cli_path: Option<&'a str>,
    /// The process working directory, for `path.resolve`.
    pub cwd: &'a str,
}

/// `buildTerminalEnvironment`. `prepare_zsh` runs only for a zsh shell and
/// returns the private runtime `ZDOTDIR`.
///
/// # Errors
///
/// The error of `prepare_zsh`.
pub fn build_terminal_environment(
    input: &TerminalEnvironmentInput<'_>,
    prepare_zsh: impl FnOnce() -> std::io::Result<String>,
) -> std::io::Result<JsObject> {
    let mut terminal = JsObject::new();
    terminal.insert("TERM", JsValue::String("xterm-256color".to_owned()));
    terminal.insert("TERM_PROGRAM", JsValue::String("kitty".to_owned()));
    let mut env = external_process_env(input.process_env, &[input.env, &terminal]);

    if let Some(bin_dir) = input.paseo_cli_bin_dir {
        let path_key = env
            .iter()
            .map(|(key, _)| key)
            .find(|key| key.eq_ignore_ascii_case("path"))
            .unwrap_or("PATH")
            .to_owned();
        let current = env.get(&path_key).and_then(JsValue::as_str).unwrap_or("");
        let prepended = prepend_path_entry(current, bin_dir);
        env.insert(path_key, JsValue::String(prepended));
    }
    if let Some(cli_path) = input.paseo_hook_cli_path {
        env.insert(
            "PASEO_HOOK_CLI",
            JsValue::String(resolve_posix(input.cwd, &external_process_path(cli_path))),
        );
    }
    if basename(input.shell) != "zsh" {
        return Ok(env);
    }
    let original_zdotdir = match env.get("ZDOTDIR") {
        Some(JsValue::String(value)) => value.clone(),
        _ => String::new(),
    };
    env.insert("PASEO_ZSH_ZDOTDIR", JsValue::String(original_zdotdir));
    env.insert("ZDOTDIR", JsValue::String(prepare_zsh()?));
    Ok(env)
}

/// `prependPathEntry`: the entry first, then the other non-empty entries.
fn prepend_path_entry(current: &str, entry: &str) -> String {
    std::iter::once(entry)
        .chain(
            current
                .split(':')
                .filter(|value| !value.is_empty() && *value != entry),
        )
        .collect::<Vec<_>>()
        .join(":")
}

/// `resolveExternalProcessPath`: the first `.asar` path segment becomes
/// `.asar.unpacked`.
#[must_use]
pub fn external_process_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut search = 0;
    while let Some(offset) = path[search..].find(".asar") {
        let end = search + offset + ".asar".len();
        if matches!(bytes.get(end), None | Some(b'/' | b'\\')) {
            return format!(
                "{}.asar.unpacked{}",
                &path[..end - ".asar".len()],
                &path[end..]
            );
        }
        search = search + offset + 1;
    }
    path.to_owned()
}

/// Node `path.basename` for a POSIX path without a suffix argument.
fn basename(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}

/// Node `path.posix.resolve(cwd, path)`.
#[must_use]
pub fn resolve_posix(cwd: &str, path: &str) -> String {
    let joined = if path.starts_with('/') {
        path.to_owned()
    } else if path.is_empty() {
        cwd.to_owned()
    } else {
        format!("{cwd}/{path}")
    };
    let mut segments: Vec<&str> = Vec::new();
    for segment in joined.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    format!("/{}", segments.join("/"))
}

/// `prepareZshShellIntegrationRuntimeDir`: copies `.zshenv` and
/// `paseo-integration.zsh` from `source_dir` into
/// `<tmpdir>/<username>-paseo-zsh-<pid>` (mode 0700, files 0600) and returns
/// that directory.
///
/// # Errors
///
/// Any file system error.
pub fn prepare_zsh_runtime_dir(
    source_dir: &Path,
    tmpdir: &Path,
    username: &str,
    pid: u32,
) -> std::io::Result<String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let source_dir = external_process_path(&source_dir.to_string_lossy());
    let runtime_dir = tmpdir.join(format!("{username}-paseo-zsh-{pid}"));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&runtime_dir)?;
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))?;
    for name in [".zshenv", "paseo-integration.zsh"] {
        let contents = std::fs::read(Path::new(&source_dir).join(name))?;
        write_private_file_atomic(&runtime_dir.join(name), &contents)?;
    }
    Ok(runtime_dir.to_string_lossy().into_owned())
}

/// `writePrivateFileAtomicSync`: ensures the private parent, writes a 0600
/// `.<name>.<pid>.<uuid>` sibling, renames it over the target, and removes
/// the sibling when either step fails. `chmod` failures are ignored, as the
/// baseline does for non-portable permissions.
fn write_private_file_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

    let parent = path.parent().unwrap_or(Path::new("/"));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .and_then(|mut file| std::io::Write::write_all(&mut file, contents))
        .and_then(|()| std::fs::rename(&temporary, path));
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(())
}
