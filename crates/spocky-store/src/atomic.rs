//! `writeFileAtomic` from pinned Paseo `atomic-file.ts`: create the parent
//! directory, write a hidden temp file beside the target, rename it over the
//! target, and on failure remove the temp file.
//!
//! Each step is the node call the baseline makes, and fails with node's
//! error text, so a failed write reads as it does in the baseline:
//! - `fs.mkdir(dir, { recursive: true })` runs node's `MKDirpAsync`, which
//!   reports the directory that failed, not the one requested.
//! - `fs.writeFile(temp, data)` opens (`open`, with the temp path) and
//!   writes (`write`, no path).
//! - `fs.rename(temp, target)` (`rename`, with both paths).
//! - The catch's `fs.rm(temp, { force: true })` (`lstat`, then `unlink`)
//!   replaces the original error when it fails other than with `ENOENT`.
//!
//! Messages follow node's `UVException`: `CODE: description, syscall
//! 'path' -> 'dest'`, with libuv's names and descriptions. The temp name
//! carries a v4-shaped random id like the baseline's `randomUUID()`, so it
//! has the same length.
//!
//! Not reproduced: a failing `close` of the temp file (`std` drops a file
//! without reporting it); node then rejects with the close error.

use std::collections::hash_map::RandomState;
use std::fmt::{Display, Formatter, Write as _};
use std::fs::{self, File};
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::StoreError;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

const EPERM: i32 = 1;
const ENOENT: i32 = 2;
const EACCES: i32 = 13;
const EEXIST: i32 = 17;
const ENOTDIR: i32 = 20;
const ENOSPC: i32 = 28;

/// A failed file system call, reported as node's `UVException` reports it.
#[derive(Debug)]
pub struct FsError {
    /// The node binding's syscall name: `mkdir`, `open`, `write`, `rename`,
    /// `lstat` or `unlink`.
    pub syscall: &'static str,
    pub path: Option<String>,
    pub dest: Option<String>,
    pub source: io::Error,
}

impl FsError {
    fn at(syscall: &'static str, source: io::Error, path: &Path) -> Self {
        Self {
            syscall,
            path: Some(path.to_string_lossy().into_owned()),
            dest: None,
            source,
        }
    }

    /// `error.code`: libuv's name for the errno (`uv_err_name`).
    #[must_use]
    pub fn code(&self) -> String {
        match self.source.raw_os_error() {
            Some(errno) => uv_name(errno).map_or_else(|| unknown(errno), str::to_owned),
            None => "UNKNOWN".to_owned(),
        }
    }

    /// libuv's description of the errno (`uv_strerror`).
    #[must_use]
    pub fn description(&self) -> String {
        match self.source.raw_os_error() {
            Some(errno) => uv_name(errno)
                .and_then(|name| {
                    DESCRIPTIONS
                        .iter()
                        .find(|(known, _)| *known == name)
                        .map(|(_, description)| (*description).to_owned())
                })
                .unwrap_or_else(|| unknown(errno)),
            None => "unknown error".to_owned(),
        }
    }
}

/// `uv_err_name` and `uv_strerror` of an errno libuv does not map.
fn unknown(errno: i32) -> String {
    format!("Unknown system error -{errno}")
}

fn uv_name(errno: i32) -> Option<&'static str> {
    ERRNO_NAMES
        .iter()
        .find(|(known, _)| *known == errno)
        .map(|(_, name)| *name)
}

impl Display for FsError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}: {}, {}",
            self.code(),
            self.description(),
            self.syscall
        )?;
        if let Some(path) = &self.path {
            write!(formatter, " '{path}'")?;
        }
        if let Some(dest) = &self.dest {
            write!(formatter, " -> '{dest}'")?;
        }
        Ok(())
    }
}

impl std::error::Error for FsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn errno(error: &io::Error) -> Option<i32> {
    error.raw_os_error()
}

/// `path.substr(0, path.find_last_of('/'))`, as `MKDirpAsync` takes the
/// parent: the whole path when it has no separator.
fn mkdirp_parent(path: &str) -> &str {
    path.rfind('/').map_or(path, |index| &path[..index])
}

/// `fs.promises.mkdir(directory, { recursive: true })`: node's
/// `MKDirpAsync`. Missing parents are created first; a failure reports the
/// directory whose `mkdir` failed.
fn mkdirp(directory: &str) -> Result<(), FsError> {
    let mut stack = vec![directory.to_owned()];
    while let Some(path) = stack.pop() {
        let Err(mut error) = fs::create_dir(&path) else {
            continue;
        };
        loop {
            match errno(&error) {
                Some(EACCES | ENOSPC | ENOTDIR | EPERM) => {
                    return Err(FsError::at("mkdir", error, Path::new(&path)));
                }
                Some(ENOENT) => {
                    let parent = mkdirp_parent(&path).to_owned();
                    if parent != path {
                        stack.push(path.clone());
                        stack.push(parent);
                    } else if stack.is_empty() {
                        error = io::Error::from_raw_os_error(EEXIST);
                        continue;
                    }
                    break;
                }
                original => {
                    let stat = fs::metadata(&path);
                    if original == Some(EEXIST) && !stack.is_empty() {
                        if stat.as_ref().is_ok_and(fs::Metadata::is_dir) {
                            break;
                        }
                        let error = io::Error::from_raw_os_error(ENOTDIR);
                        return Err(FsError::at("mkdir", error, Path::new(&path)));
                    }
                    return match stat {
                        Ok(metadata) if metadata.is_dir() => Ok(()),
                        Ok(_) => Err(FsError::at(
                            "mkdir",
                            io::Error::from_raw_os_error(EEXIST),
                            Path::new(&path),
                        )),
                        Err(error) => Err(FsError::at("mkdir", error, Path::new(&path))),
                    };
                }
            }
        }
    }
    Ok(())
}

/// The catch's `fs.rm(temp, { force: true })`: a missing temp file is
/// fine; any other failure is returned.
fn remove_temporary(temporary: &Path) -> Result<(), FsError> {
    match fs::symlink_metadata(temporary) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(FsError::at("lstat", error, temporary)),
        Ok(_) => match fs::remove_file(temporary) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                Err(FsError::at("unlink", error, temporary))
            }
            _ => Ok(()),
        },
    }
}

/// A random id shaped like `randomUUID()` (version 4, RFC variant).
// ponytail: std's per-process SipHash keys, not a CSPRNG; the id only has to
// keep temp names apart beside the pid and millisecond.
fn random_uuid() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut bytes = [0_u8; 16];
    for (salt, half) in bytes.chunks_mut(8).enumerate() {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_usize(salt);
        hasher.write_u64(sequence);
        hasher.write_u128(nanos);
        half.copy_from_slice(&hasher.finish().to_be_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// `writeFileAtomic(path, contents)`.
///
/// # Errors
///
/// Returns node's error for the step that failed, or for the temp file
/// removal that followed it.
pub fn write_file_atomic(path: &Path, contents: &str) -> Result<(), FsError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    mkdirp(&parent.to_string_lossy())?;
    let base = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    let temporary: PathBuf = parent.join(format!(
        ".{base}.{}.{millis}.{}.tmp",
        std::process::id(),
        random_uuid()
    ));
    let written = File::create(&temporary)
        .map_err(|error| FsError::at("open", error, &temporary))
        .and_then(|mut file| {
            file.write_all(contents.as_bytes())
                .map_err(|source| FsError {
                    syscall: "write",
                    path: None,
                    dest: None,
                    source,
                })
        })
        .and_then(|()| {
            fs::rename(&temporary, path).map_err(|error| FsError {
                dest: Some(path.to_string_lossy().into_owned()),
                ..FsError::at("rename", error, &temporary)
            })
        });
    if let Err(error) = written {
        remove_temporary(&temporary)?;
        return Err(error);
    }
    Ok(())
}

/// `writeJsonFileAtomic`: `contents` is the caller's JSON text.
///
/// # Errors
///
/// Returns the [`FsError`] of [`write_file_atomic`].
pub fn write_json_atomic(path: &Path, contents: &str) -> Result<(), StoreError> {
    write_file_atomic(path, contents).map_err(StoreError::Fs)
}

/// libuv's names (`uv_err_name`) for the platform errnos it maps, from
/// `util.getSystemErrorName` on node 22.20.0 (macOS) and libuv's
/// `uv/errno.h` with the Linux errno values.
#[cfg(target_os = "macos")]
const ERRNO_NAMES: &[(i32, &str)] = &[
    (1, "EPERM"),
    (2, "ENOENT"),
    (3, "ESRCH"),
    (4, "EINTR"),
    (5, "EIO"),
    (6, "ENXIO"),
    (7, "E2BIG"),
    (8, "ENOEXEC"),
    (9, "EBADF"),
    (12, "ENOMEM"),
    (13, "EACCES"),
    (14, "EFAULT"),
    (16, "EBUSY"),
    (17, "EEXIST"),
    (18, "EXDEV"),
    (19, "ENODEV"),
    (20, "ENOTDIR"),
    (21, "EISDIR"),
    (22, "EINVAL"),
    (23, "ENFILE"),
    (24, "EMFILE"),
    (25, "ENOTTY"),
    (26, "ETXTBSY"),
    (27, "EFBIG"),
    (28, "ENOSPC"),
    (29, "ESPIPE"),
    (30, "EROFS"),
    (31, "EMLINK"),
    (32, "EPIPE"),
    (34, "ERANGE"),
    (35, "EAGAIN"),
    (37, "EALREADY"),
    (38, "ENOTSOCK"),
    (39, "EDESTADDRREQ"),
    (40, "EMSGSIZE"),
    (41, "EPROTOTYPE"),
    (42, "ENOPROTOOPT"),
    (43, "EPROTONOSUPPORT"),
    (44, "ESOCKTNOSUPPORT"),
    (45, "ENOTSUP"),
    (47, "EAFNOSUPPORT"),
    (48, "EADDRINUSE"),
    (49, "EADDRNOTAVAIL"),
    (50, "ENETDOWN"),
    (51, "ENETUNREACH"),
    (53, "ECONNABORTED"),
    (54, "ECONNRESET"),
    (55, "ENOBUFS"),
    (56, "EISCONN"),
    (57, "ENOTCONN"),
    (58, "ESHUTDOWN"),
    (60, "ETIMEDOUT"),
    (61, "ECONNREFUSED"),
    (62, "ELOOP"),
    (63, "ENAMETOOLONG"),
    (64, "EHOSTDOWN"),
    (65, "EHOSTUNREACH"),
    (66, "ENOTEMPTY"),
    (78, "ENOSYS"),
    (79, "EFTYPE"),
    (84, "EOVERFLOW"),
    (89, "ECANCELED"),
    (92, "EILSEQ"),
    (96, "ENODATA"),
    (100, "EPROTO"),
];

#[cfg(target_os = "linux")]
const ERRNO_NAMES: &[(i32, &str)] = &[
    (1, "EPERM"),
    (2, "ENOENT"),
    (3, "ESRCH"),
    (4, "EINTR"),
    (5, "EIO"),
    (6, "ENXIO"),
    (7, "E2BIG"),
    (8, "ENOEXEC"),
    (9, "EBADF"),
    (11, "EAGAIN"),
    (12, "ENOMEM"),
    (13, "EACCES"),
    (14, "EFAULT"),
    (16, "EBUSY"),
    (17, "EEXIST"),
    (18, "EXDEV"),
    (19, "ENODEV"),
    (20, "ENOTDIR"),
    (21, "EISDIR"),
    (22, "EINVAL"),
    (23, "ENFILE"),
    (24, "EMFILE"),
    (25, "ENOTTY"),
    (26, "ETXTBSY"),
    (27, "EFBIG"),
    (28, "ENOSPC"),
    (29, "ESPIPE"),
    (30, "EROFS"),
    (31, "EMLINK"),
    (32, "EPIPE"),
    (34, "ERANGE"),
    (36, "ENAMETOOLONG"),
    (38, "ENOSYS"),
    (39, "ENOTEMPTY"),
    (40, "ELOOP"),
    (49, "EUNATCH"),
    (61, "ENODATA"),
    (64, "ENONET"),
    (71, "EPROTO"),
    (75, "EOVERFLOW"),
    (84, "EILSEQ"),
    (88, "ENOTSOCK"),
    (89, "EDESTADDRREQ"),
    (90, "EMSGSIZE"),
    (91, "EPROTOTYPE"),
    (92, "ENOPROTOOPT"),
    (93, "EPROTONOSUPPORT"),
    (94, "ESOCKTNOSUPPORT"),
    (95, "ENOTSUP"),
    (97, "EAFNOSUPPORT"),
    (98, "EADDRINUSE"),
    (99, "EADDRNOTAVAIL"),
    (100, "ENETDOWN"),
    (101, "ENETUNREACH"),
    (103, "ECONNABORTED"),
    (104, "ECONNRESET"),
    (105, "ENOBUFS"),
    (106, "EISCONN"),
    (107, "ENOTCONN"),
    (108, "ESHUTDOWN"),
    (110, "ETIMEDOUT"),
    (111, "ECONNREFUSED"),
    (112, "EHOSTDOWN"),
    (113, "EHOSTUNREACH"),
    (114, "EALREADY"),
    (121, "EREMOTEIO"),
    (125, "ECANCELED"),
];

/// `UV_ERRNO_MAP` descriptions (`uv_strerror`) for the names above.
const DESCRIPTIONS: &[(&str, &str)] = &[
    ("E2BIG", "argument list too long"),
    ("EACCES", "permission denied"),
    ("EADDRINUSE", "address already in use"),
    ("EADDRNOTAVAIL", "address not available"),
    ("EAFNOSUPPORT", "address family not supported"),
    ("EAGAIN", "resource temporarily unavailable"),
    ("EALREADY", "connection already in progress"),
    ("EBADF", "bad file descriptor"),
    ("EBUSY", "resource busy or locked"),
    ("ECANCELED", "operation canceled"),
    ("ECONNABORTED", "software caused connection abort"),
    ("ECONNREFUSED", "connection refused"),
    ("ECONNRESET", "connection reset by peer"),
    ("EDESTADDRREQ", "destination address required"),
    ("EEXIST", "file already exists"),
    ("EFAULT", "bad address in system call argument"),
    ("EFBIG", "file too large"),
    ("EFTYPE", "inappropriate file type or format"),
    ("EHOSTDOWN", "host is down"),
    ("EHOSTUNREACH", "host is unreachable"),
    ("EILSEQ", "illegal byte sequence"),
    ("EINTR", "interrupted system call"),
    ("EINVAL", "invalid argument"),
    ("EIO", "i/o error"),
    ("EISCONN", "socket is already connected"),
    ("EISDIR", "illegal operation on a directory"),
    ("ELOOP", "too many symbolic links encountered"),
    ("EMFILE", "too many open files"),
    ("EMLINK", "too many links"),
    ("EMSGSIZE", "message too long"),
    ("ENAMETOOLONG", "name too long"),
    ("ENETDOWN", "network is down"),
    ("ENETUNREACH", "network is unreachable"),
    ("ENFILE", "file table overflow"),
    ("ENOBUFS", "no buffer space available"),
    ("ENODATA", "no data available"),
    ("ENODEV", "no such device"),
    ("ENOENT", "no such file or directory"),
    ("ENOEXEC", "exec format error"),
    ("ENOMEM", "not enough memory"),
    ("ENONET", "machine is not on the network"),
    ("ENOPROTOOPT", "protocol not available"),
    ("ENOSPC", "no space left on device"),
    ("ENOSYS", "function not implemented"),
    ("ENOTCONN", "socket is not connected"),
    ("ENOTDIR", "not a directory"),
    ("ENOTEMPTY", "directory not empty"),
    ("ENOTSOCK", "socket operation on non-socket"),
    ("ENOTSUP", "operation not supported on socket"),
    ("ENOTTY", "inappropriate ioctl for device"),
    ("ENXIO", "no such device or address"),
    ("EOVERFLOW", "value too large for defined data type"),
    ("EPERM", "operation not permitted"),
    ("EPIPE", "broken pipe"),
    ("EPROTO", "protocol error"),
    ("EPROTONOSUPPORT", "protocol not supported"),
    ("EPROTOTYPE", "protocol wrong type for socket"),
    ("ERANGE", "result too large"),
    ("EREMOTEIO", "remote I/O error"),
    ("EROFS", "read-only file system"),
    ("ESHUTDOWN", "cannot send after transport endpoint shutdown"),
    ("ESOCKTNOSUPPORT", "socket type not supported"),
    ("ESPIPE", "invalid seek"),
    ("ESRCH", "no such process"),
    ("ETIMEDOUT", "connection timed out"),
    ("ETXTBSY", "text file is busy"),
    ("EUNATCH", "protocol driver not attached"),
    ("EXDEV", "cross-device link not permitted"),
];

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::{Path, PathBuf};

    use super::{FsError, mkdirp, mkdirp_parent, random_uuid, write_file_atomic};

    struct Home(PathBuf);

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn home(name: &str) -> Home {
        let path =
            std::env::temp_dir().join(format!("spocky-atomic-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("home");
        Home(path)
    }

    #[test]
    fn messages_follow_uv_exception() {
        // node: UVException(-21, "rename", null, "/a/.t.tmp", "/a/t")
        let rename = FsError {
            syscall: "rename",
            path: Some("/a/.t.tmp".to_owned()),
            dest: Some("/a/t".to_owned()),
            source: io::Error::from_raw_os_error(21),
        };
        assert_eq!(
            rename.to_string(),
            "EISDIR: illegal operation on a directory, rename '/a/.t.tmp' -> '/a/t'"
        );
        let write = FsError {
            syscall: "write",
            path: None,
            dest: None,
            source: io::Error::from_raw_os_error(28),
        };
        assert_eq!(write.to_string(), "ENOSPC: no space left on device, write");
        // ECHILD has no libuv name on either platform.
        let unmapped = FsError {
            syscall: "open",
            path: Some("/x".to_owned()),
            dest: None,
            source: io::Error::from_raw_os_error(10),
        };
        assert_eq!(unmapped.code(), "Unknown system error -10");
        assert_eq!(
            unmapped.to_string(),
            "Unknown system error -10: Unknown system error -10, open '/x'"
        );
    }

    #[test]
    fn mkdirp_parent_cuts_at_the_last_separator() {
        assert_eq!(mkdirp_parent("/a/b"), "/a");
        assert_eq!(mkdirp_parent("/a"), "");
        assert_eq!(mkdirp_parent("a"), "a");
    }

    #[test]
    fn temp_ids_have_the_random_uuid_shape() {
        let first = random_uuid();
        assert_eq!(first.len(), 36);
        assert_eq!(&first[14..15], "4");
        assert!(matches!(&first[19..20], "8" | "9" | "a" | "b"));
        assert!(first.chars().enumerate().all(|(index, character)| {
            if [8, 13, 18, 23].contains(&index) {
                character == '-'
            } else {
                character.is_ascii_hexdigit() && !character.is_ascii_uppercase()
            }
        }));
        assert_ne!(first, random_uuid());
    }

    #[test]
    fn mkdirp_creates_missing_parents_and_reports_the_failing_one() {
        use std::os::unix::fs::PermissionsExt;
        let home = home("mkdirp");
        let nested = home.0.join("a/b/c");
        mkdirp(&nested.to_string_lossy()).expect("nested");
        assert!(nested.is_dir());
        let locked = home.0.join("ro");
        std::fs::create_dir(&locked).expect("ro");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).expect("chmod");
        let error = mkdirp(&locked.join("x/y").to_string_lossy()).expect_err("locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        assert_eq!(
            error.to_string(),
            format!(
                "EACCES: permission denied, mkdir '{}'",
                locked.join("x").display()
            )
        );
    }

    #[test]
    fn a_failed_rename_removes_the_temp_file() {
        let home = home("rename");
        let target = home.0.join("d/t.json");
        std::fs::create_dir_all(&target).expect("blocking directory");
        let error = write_file_atomic(&target, "{}").expect_err("rename onto a directory");
        assert_eq!(error.syscall, "rename");
        assert_eq!(error.code(), "EISDIR");
        assert_eq!(
            std::fs::read_dir(home.0.join("d")).expect("dir").count(),
            1,
            "only the blocking directory is left"
        );
        write_file_atomic(Path::new(&home.0.join("ok.json")), "{}").expect("write");
        assert_eq!(
            std::fs::read_to_string(home.0.join("ok.json")).expect("read"),
            "{}"
        );
    }
}
