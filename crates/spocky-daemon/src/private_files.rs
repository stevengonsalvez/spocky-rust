//! Owner-only files and directories under the daemon home.
//!
//! Source at Paseo `5de45e2`: `private-files.ts`.

use std::fs;
use std::io;
use std::path::Path;

use uuid::Uuid;

pub const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
pub const PRIVATE_FILE_MODE: u32 = 0o600;

/// `applyPrivateMode`: best effort, because not every filesystem keeps modes.
fn apply_private_mode(target: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(target, fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (target, mode);
    }
}

/// `ensurePrivateDirectory`: create with the private mode, then apply it to
/// the leaf. Parents created on the way keep the process umask default.
///
/// # Errors
///
/// Returns the error from creating the directory.
pub fn ensure_private_directory(directory: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(PRIVATE_DIRECTORY_MODE);
    }
    builder.create(directory)?;
    apply_private_mode(directory, PRIVATE_DIRECTORY_MODE);
    Ok(())
}

/// `ensurePrivateFile`.
pub fn ensure_private_file(file: &Path) {
    apply_private_mode(file, PRIVATE_FILE_MODE);
}

/// `path.dirname(file)`: `"."` for a bare name, where `Path::parent` gives an
/// empty path.
fn directory_of(file: &Path) -> &Path {
    file.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// `writePrivateFileAtomicSync`: write a sibling `.<name>.<pid>.<uuid>` with the
/// private mode, rename it over the target, and remove it if anything fails.
///
/// # Errors
///
/// Returns the first error from creating the directory, writing, or renaming.
pub fn write_private_file_atomic(file: &Path, data: &[u8]) -> io::Result<()> {
    let parent = directory_of(file);
    ensure_private_directory(parent)?;
    let name = file
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let temporary = parent.join(format!(".{name}.{}.{}", std::process::id(), Uuid::new_v4()));
    let result = write_new_private(&temporary, data)
        .and_then(|()| fs::rename(&temporary, file))
        .map(|()| ensure_private_file(file));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// `writeFileSync(temporary, data, { mode })`: create or truncate, with the mode
/// applied only when the file is created.
fn write_new_private(path: &Path, data: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(PRIVATE_FILE_MODE);
    }
    options.open(path)?.write_all(data)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn creates_a_private_directory_and_tightens_an_existing_one() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("a").join("home");
        ensure_private_directory(&home).unwrap();
        assert_eq!(mode(&home), 0o700);
        fs::set_permissions(&home, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_directory(&home).unwrap();
        assert_eq!(mode(&home), 0o700);
    }

    #[test]
    fn writes_atomically_with_the_private_mode_and_no_leftover() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("home").join("server-id");
        write_private_file_atomic(&file, b"srv_x\n").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"srv_x\n");
        assert_eq!(mode(&file), 0o600);
        write_private_file_atomic(&file, b"srv_y\n").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"srv_y\n");
        let names: Vec<_> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["server-id"]);
    }

    #[test]
    fn a_failed_rename_removes_the_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        fs::create_dir_all(home.join("target")).unwrap();
        assert!(write_private_file_atomic(&home.join("target"), b"x").is_err());
        let names: Vec<_> = fs::read_dir(&home)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["target"]);
    }

    #[test]
    fn tightens_a_group_readable_file() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("f");
        fs::write(&file, "x").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        ensure_private_file(&file);
        assert_eq!(mode(&file), 0o600);
    }

    #[test]
    fn a_bare_file_name_has_the_current_directory_as_its_directory() {
        assert_eq!(directory_of(Path::new("bare-name")), Path::new("."));
        assert_eq!(directory_of(Path::new("dir/name")), Path::new("dir"));
        assert_eq!(directory_of(Path::new("/name")), Path::new("/"));
    }
}
