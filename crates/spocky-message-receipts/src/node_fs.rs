//! The node 22.20.0 filesystem behavior the receipt store depends on:
//! `path.posix.join`, `fs.promises.readFile`, `fs.promises.mkdir` with
//! `{ recursive: true }`, and `writeFileAtomic` from pinned Paseo
//! `atomic-file.ts`, each failing with node's `UVException` fields and text.
//!
//! Errors carry node's negative `errno`, the libuv code name, the syscall,
//! and the paths node reports. The libuv name and description table covers
//! POSIX errno values; on other platforms every error reports as an unknown
//! system error.

use std::fmt::{self, Display, Formatter};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::time::{SystemTime, UNIX_EPOCH};

/// A node filesystem error: `CODE: description, syscall 'path' -> 'dest'`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsError {
    /// Node's `errno`: the negated POSIX error number.
    pub errno: i32,
    pub syscall: &'static str,
    pub path: Option<String>,
    pub dest: Option<String>,
}

impl FsError {
    fn new(errno: i32, syscall: &'static str, path: Option<&str>) -> Self {
        Self {
            errno,
            syscall,
            path: path.map(str::to_owned),
            dest: None,
        }
    }

    fn from_io(error: &io::Error, syscall: &'static str, path: Option<&str>) -> Self {
        // ponytail: a std error without an OS code (only a NUL byte in a
        // path) reports as EINVAL; node rejects that path before any syscall.
        let errno = -error.raw_os_error().unwrap_or(EINVAL);
        Self::new(errno, syscall, path)
    }

    /// Node's `code`: the libuv error name.
    #[must_use]
    pub fn code(&self) -> String {
        uv_error(self.errno).map_or_else(
            || format!("Unknown system error {}", self.errno),
            |(name, _)| name.to_owned(),
        )
    }

    fn description(&self) -> String {
        uv_error(self.errno).map_or_else(
            || format!("Unknown system error {}", self.errno),
            |(_, description)| description.to_owned(),
        )
    }

    fn is(&self, errno: i32) -> bool {
        self.errno == -errno
    }

    /// `error.code === "ENOENT"`.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        self.is(ENOENT)
    }
}

impl Display for FsError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
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

impl std::error::Error for FsError {}

#[cfg(unix)]
use libc::{
    E2BIG, EACCES, EADDRINUSE, EADDRNOTAVAIL, EAFNOSUPPORT, EAGAIN, EALREADY, EBADF, EBUSY,
    ECANCELED, ECONNABORTED, ECONNREFUSED, ECONNRESET, EDESTADDRREQ, EEXIST, EFAULT, EFBIG,
    EHOSTDOWN, EHOSTUNREACH, EILSEQ, EINTR, EINVAL, EIO, EISCONN, EISDIR, ELOOP, EMFILE, EMLINK,
    EMSGSIZE, ENAMETOOLONG, ENETDOWN, ENETUNREACH, ENFILE, ENOBUFS, ENODATA, ENODEV, ENOENT,
    ENOEXEC, ENOMEM, ENOPROTOOPT, ENOSPC, ENOSYS, ENOTCONN, ENOTDIR, ENOTEMPTY, ENOTSOCK, ENOTSUP,
    ENOTTY, ENXIO, EOVERFLOW, EPERM, EPIPE, EPROTO, EPROTONOSUPPORT, EPROTOTYPE, ERANGE, EROFS,
    ESHUTDOWN, ESOCKTNOSUPPORT, ESPIPE, ESRCH, ETIMEDOUT, ETXTBSY, EXDEV,
};

#[cfg(not(unix))]
const ENOENT: i32 = 2;
#[cfg(not(unix))]
const EPERM: i32 = 1;
#[cfg(not(unix))]
const EACCES: i32 = 13;
#[cfg(not(unix))]
const EEXIST: i32 = 17;
#[cfg(not(unix))]
const ENOTDIR: i32 = 20;
#[cfg(not(unix))]
const EINVAL: i32 = 22;
#[cfg(not(unix))]
const ENOSPC: i32 = 28;

/// libuv `uv_err_name` and `uv_strerror` for node's negative `errno`, read
/// from node 22.20.0 `util.getSystemErrorName` and `getSystemErrorMessage`.
#[cfg(unix)]
fn uv_error(errno: i32) -> Option<(&'static str, &'static str)> {
    let table: &[(i32, &str, &str)] = &[
        (EPERM, "EPERM", "operation not permitted"),
        (ENOENT, "ENOENT", "no such file or directory"),
        (ESRCH, "ESRCH", "no such process"),
        (EINTR, "EINTR", "interrupted system call"),
        (EIO, "EIO", "i/o error"),
        (ENXIO, "ENXIO", "no such device or address"),
        (E2BIG, "E2BIG", "argument list too long"),
        (ENOEXEC, "ENOEXEC", "exec format error"),
        (EBADF, "EBADF", "bad file descriptor"),
        (ENOMEM, "ENOMEM", "not enough memory"),
        (EACCES, "EACCES", "permission denied"),
        (EFAULT, "EFAULT", "bad address in system call argument"),
        (EBUSY, "EBUSY", "resource busy or locked"),
        (EEXIST, "EEXIST", "file already exists"),
        (EXDEV, "EXDEV", "cross-device link not permitted"),
        (ENODEV, "ENODEV", "no such device"),
        (ENOTDIR, "ENOTDIR", "not a directory"),
        (EISDIR, "EISDIR", "illegal operation on a directory"),
        (EINVAL, "EINVAL", "invalid argument"),
        (ENFILE, "ENFILE", "file table overflow"),
        (EMFILE, "EMFILE", "too many open files"),
        (ENOTTY, "ENOTTY", "inappropriate ioctl for device"),
        (ETXTBSY, "ETXTBSY", "text file is busy"),
        (EFBIG, "EFBIG", "file too large"),
        (ENOSPC, "ENOSPC", "no space left on device"),
        (ESPIPE, "ESPIPE", "invalid seek"),
        (EROFS, "EROFS", "read-only file system"),
        (EMLINK, "EMLINK", "too many links"),
        (EPIPE, "EPIPE", "broken pipe"),
        (ERANGE, "ERANGE", "result too large"),
        (EAGAIN, "EAGAIN", "resource temporarily unavailable"),
        (EALREADY, "EALREADY", "connection already in progress"),
        (ENOTSOCK, "ENOTSOCK", "socket operation on non-socket"),
        (EDESTADDRREQ, "EDESTADDRREQ", "destination address required"),
        (EMSGSIZE, "EMSGSIZE", "message too long"),
        (EPROTOTYPE, "EPROTOTYPE", "protocol wrong type for socket"),
        (ENOPROTOOPT, "ENOPROTOOPT", "protocol not available"),
        (EPROTONOSUPPORT, "EPROTONOSUPPORT", "protocol not supported"),
        (
            ESOCKTNOSUPPORT,
            "ESOCKTNOSUPPORT",
            "socket type not supported",
        ),
        (ENOTSUP, "ENOTSUP", "operation not supported on socket"),
        (EAFNOSUPPORT, "EAFNOSUPPORT", "address family not supported"),
        (EADDRINUSE, "EADDRINUSE", "address already in use"),
        (EADDRNOTAVAIL, "EADDRNOTAVAIL", "address not available"),
        (ENETDOWN, "ENETDOWN", "network is down"),
        (ENETUNREACH, "ENETUNREACH", "network is unreachable"),
        (
            ECONNABORTED,
            "ECONNABORTED",
            "software caused connection abort",
        ),
        (ECONNRESET, "ECONNRESET", "connection reset by peer"),
        (ENOBUFS, "ENOBUFS", "no buffer space available"),
        (EISCONN, "EISCONN", "socket is already connected"),
        (ENOTCONN, "ENOTCONN", "socket is not connected"),
        (
            ESHUTDOWN,
            "ESHUTDOWN",
            "cannot send after transport endpoint shutdown",
        ),
        (ETIMEDOUT, "ETIMEDOUT", "connection timed out"),
        (ECONNREFUSED, "ECONNREFUSED", "connection refused"),
        (ELOOP, "ELOOP", "too many symbolic links encountered"),
        (ENAMETOOLONG, "ENAMETOOLONG", "name too long"),
        (EHOSTDOWN, "EHOSTDOWN", "host is down"),
        (EHOSTUNREACH, "EHOSTUNREACH", "host is unreachable"),
        (ENOTEMPTY, "ENOTEMPTY", "directory not empty"),
        (ENOSYS, "ENOSYS", "function not implemented"),
        #[cfg(target_os = "macos")]
        (libc::EFTYPE, "EFTYPE", "inappropriate file type or format"),
        (
            EOVERFLOW,
            "EOVERFLOW",
            "value too large for defined data type",
        ),
        (ECANCELED, "ECANCELED", "operation canceled"),
        (EILSEQ, "EILSEQ", "illegal byte sequence"),
        (ENODATA, "ENODATA", "no data available"),
        (EPROTO, "EPROTO", "protocol error"),
        #[cfg(target_os = "linux")]
        (libc::EREMOTEIO, "EREMOTEIO", "remote I/O error"),
        #[cfg(target_os = "linux")]
        (libc::EUNATCH, "EUNATCH", "protocol driver not attached"),
    ];
    table
        .iter()
        .find(|(number, _, _)| -*number == errno)
        .map(|(_, name, description)| (*name, *description))
}

#[cfg(not(unix))]
fn uv_error(_errno: i32) -> Option<(&'static str, &'static str)> {
    None
}

/// `path.posix.normalize`.
fn normalize(path: &str) -> String {
    let absolute = path.starts_with('/');
    let trailing = path.ends_with('/');
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.last().is_some_and(|last| *last != "..") {
                    segments.pop();
                } else if !absolute {
                    segments.push("..");
                }
            }
            _ => segments.push(segment),
        }
    }
    let mut out = segments.join("/");
    if out.is_empty() && !absolute {
        out.push('.');
    }
    if !out.is_empty() && trailing {
        out.push('/');
    }
    if absolute { format!("/{out}") } else { out }
}

/// `path.posix.join(directory, name)` for a non-empty `name`.
#[must_use]
pub fn join(directory: &str, name: &str) -> String {
    if directory.is_empty() {
        normalize(name)
    } else {
        normalize(&format!("{directory}/{name}"))
    }
}

/// `path.posix.dirname` of a normalized path without a trailing slash.
fn dirname(path: &str) -> &str {
    match path.rfind('/') {
        None => ".",
        Some(0) => "/",
        Some(index) => &path[..index],
    }
}

/// `path.posix.basename` of a normalized path without a trailing slash.
fn basename(path: &str) -> &str {
    path.rfind('/').map_or(path, |index| &path[index + 1..])
}

/// `fs.promises.readFile(path)`: open, then read to the end.
///
/// # Errors
///
/// Returns the failing `open` (with the path) or `read` (without it).
pub fn read_file(path: &str) -> Result<Vec<u8>, FsError> {
    let mut file =
        File::open(path).map_err(|error| FsError::from_io(&error, "open", Some(path)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| FsError::from_io(&error, "read", None))?;
    Ok(bytes)
}

fn os_errno(error: &io::Error) -> i32 {
    error.raw_os_error().unwrap_or(EINVAL)
}

fn is_directory(path: &str) -> Option<bool> {
    fs::metadata(path).ok().map(|metadata| metadata.is_dir())
}

/// `fs.promises.mkdir(path, { recursive: true })`, following node's
/// `MKDirpAsync`: missing parents are created first, and the error names
/// the directory whose `mkdir` failed.
///
/// # Errors
///
/// Returns the failing `mkdir`, or `EEXIST`/`ENOTDIR` when a non-directory
/// is in the way.
pub fn mkdir_recursive(path: &str) -> Result<(), FsError> {
    let mut pending: Vec<String> = Vec::new();
    let mut current = path.to_owned();
    loop {
        let errno = match fs::create_dir(&current) {
            Ok(()) => match pending.pop() {
                None => return Ok(()),
                Some(next) => {
                    current = next;
                    continue;
                }
            },
            Err(error) => os_errno(&error),
        };
        if [EACCES, ENOSPC, ENOTDIR, EPERM].contains(&errno) {
            return Err(FsError::new(-errno, "mkdir", Some(&current)));
        }
        if errno == ENOENT {
            // `path.substr(0, path.find_last_of('/'))`.
            let parent = current
                .rfind('/')
                .map_or_else(|| current.clone(), |index| current[..index].to_owned());
            if parent != current {
                pending.push(std::mem::replace(&mut current, parent));
                continue;
            }
            if pending.is_empty() {
                return Err(FsError::new(-EEXIST, "mkdir", Some(&current)));
            }
            // ponytail: node retries this mkdir forever here (a missing
            // root); report the ENOENT instead.
            return Err(FsError::new(-ENOENT, "mkdir", Some(&current)));
        }
        let directory = is_directory(&current);
        if errno == EEXIST && !pending.is_empty() {
            if directory == Some(true) {
                current = pending.pop().unwrap_or_default();
                continue;
            }
            return Err(FsError::new(-ENOTDIR, "mkdir", Some(&current)));
        }
        return match fs::metadata(&current) {
            Ok(metadata) if metadata.is_dir() => Ok(()),
            Ok(_) => Err(FsError::new(-EEXIST, "mkdir", Some(&current))),
            // The stat failure reports through the mkdir request.
            Err(error) => Err(FsError::from_io(&error, "mkdir", Some(&current))),
        };
    }
}

/// `fs.promises.writeFile(path, data, "utf8")`: open with `"w"` (mode
/// 0o666), then write.
fn write_file(path: &str, data: &str) -> Result<(), FsError> {
    let mut file =
        File::create(path).map_err(|error| FsError::from_io(&error, "open", Some(path)))?;
    file.write_all(data.as_bytes())
        .map_err(|error| FsError::from_io(&error, "write", None))
}

/// `fs.promises.rm(path, { force: true })` of a file: `lstat`, then
/// `unlink`; a missing path is not an error.
fn remove_forced(path: &str) -> Result<(), FsError> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if os_errno(&error) == ENOENT => return Ok(()),
        Err(error) => return Err(FsError::from_io(&error, "lstat", Some(path))),
    }
    match fs::remove_file(path) {
        Err(error) if os_errno(&error) != ENOENT => {
            Err(FsError::from_io(&error, "unlink", Some(path)))
        }
        _ => Ok(()),
    }
}

/// The hidden temp file `writeFileAtomic` writes beside `file`:
/// `.<basename>.<pid>.<Date.now()>.<randomUUID()>.tmp`.
fn temp_path(file: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    join(
        dirname(file),
        &format!(
            ".{}.{}.{millis}.{}.tmp",
            basename(file),
            std::process::id(),
            uuid::Uuid::new_v4()
        ),
    )
}

/// `writeFileAtomic(file, data)` from pinned Paseo `atomic-file.ts`: create
/// the parent directory, write a hidden temp file beside the target, rename
/// it over the target, and on failure remove the temp file and rethrow.
///
/// # Errors
///
/// Returns the failing `mkdir`, `open`, `write`, or `rename`, or the temp
/// file removal error when that cleanup itself fails.
pub fn write_file_atomic(file: &str, data: &str) -> Result<(), FsError> {
    mkdir_recursive(dirname(file))?;
    let temp = temp_path(file);
    let written = write_file(&temp, data).and_then(|()| {
        fs::rename(&temp, file).map_err(|error| FsError {
            dest: Some(file.to_owned()),
            ..FsError::from_io(&error, "rename", Some(&temp))
        })
    });
    if let Err(error) = written {
        remove_forced(&temp)?;
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FsError, basename, dirname, join, normalize};

    #[test]
    fn join_matches_node_path_posix() {
        // node -e 'console.log(path.posix.join(a, b))'
        let cases = [
            ("/a/b", "k.json", "/a/b/k.json"),
            ("/a//b/", "k.json", "/a/b/k.json"),
            ("/a/./b/../c", "k.json", "/a/c/k.json"),
            ("a/..", "k.json", "k.json"),
            ("../x", "k.json", "../x/k.json"),
            ("/..", "k.json", "/k.json"),
            ("", "k.json", "k.json"),
            ("/", "k.json", "/k.json"),
        ];
        for (directory, name, expected) in cases {
            assert_eq!(join(directory, name), expected, "{directory:?}");
        }
        assert_eq!(normalize(""), ".");
        assert_eq!(normalize("a/"), "a/");
        assert_eq!(dirname("/k.json"), "/");
        assert_eq!(dirname("k.json"), ".");
        assert_eq!(dirname("/a/k.json"), "/a");
        assert_eq!(basename("/a/k.json"), "k.json");
    }

    #[cfg(unix)]
    #[test]
    fn error_text_matches_node_uv_exception() {
        let error = FsError {
            errno: -libc::EACCES,
            syscall: "rename",
            path: Some("/t/.k.tmp".to_owned()),
            dest: Some("/t/k".to_owned()),
        };
        assert_eq!(
            error.to_string(),
            "EACCES: permission denied, rename '/t/.k.tmp' -> '/t/k'"
        );
        let read = FsError::new(-libc::EISDIR, "read", None);
        assert_eq!(
            read.to_string(),
            "EISDIR: illegal operation on a directory, read"
        );
        let unknown = FsError::new(-10_000, "open", Some("/x"));
        assert_eq!(unknown.code(), "Unknown system error -10000");
        assert_eq!(
            unknown.to_string(),
            "Unknown system error -10000: Unknown system error -10000, open '/x'"
        );
    }
}
