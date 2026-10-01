//! Port of the Emscripten 3.1.74 virtual filesystem shipped in the pinned
//! `PGlite` 0.5.4 glue (`FS`, `MEMFS`, `NODEFS`, `PROXYFS`, `TTY`, `PIPEFS`
//! and the socket node of `SOCKFS`).
//!
//! The structure follows the glue so each operation can be compared with its
//! JavaScript source: node lookup goes through a name table first, every
//! filesystem decides its own node and stream operations, and errors carry
//! Emscripten errno values.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs as host;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const EPERM: i32 = 63;
pub const ENOENT: i32 = 44;
pub const EIO: i32 = 29;
pub const EBADF: i32 = 8;
pub const EAGAIN: i32 = 6;
pub const EACCES: i32 = 2;
pub const EBUSY: i32 = 10;
pub const EEXIST: i32 = 20;
pub const EXDEV: i32 = 75;
pub const ENODEV: i32 = 43;
pub const ENOTDIR: i32 = 54;
pub const EISDIR: i32 = 31;
pub const EINVAL: i32 = 28;
pub const EMFILE: i32 = 33;
pub const ENOTTY: i32 = 59;
pub const ESPIPE: i32 = 70;
pub const ENOTEMPTY: i32 = 55;
pub const ELOOP: i32 = 32;
pub const ENOSYS: i32 = 52;
pub const EOPNOTSUPP: i32 = 138;
pub const ENXIO: i32 = 60;
pub const ENOMEM: i32 = 48;
pub const EFAULT: i32 = 21;
pub const EPROTONOSUPPORT: i32 = 66;
pub const EHOSTUNREACH: i32 = 23;
pub const ENOTCONN: i32 = 53;
pub const EOVERFLOW: i32 = 61;

pub const S_IFMT: u32 = 61440;
pub const S_IFREG: u32 = 32768;
pub const S_IFDIR: u32 = 16384;
pub const S_IFLNK: u32 = 40960;
pub const S_IFCHR: u32 = 8192;
pub const S_IFIFO: u32 = 4096;
pub const S_IFSOCK: u32 = 49152;

pub const O_ACCMODE: i32 = 2_097_155;
pub const O_WRONLY: i32 = 1;
pub const O_CREAT: i32 = 64;
pub const O_EXCL: i32 = 128;
pub const O_TRUNC: i32 = 512;
pub const O_APPEND: i32 = 1024;
pub const O_DIRECTORY: i32 = 65536;
pub const O_NOFOLLOW: i32 = 131_072;

const MAX_OPEN_FDS: usize = 4096;

/// Failure of a filesystem operation. `Errno` is an Emscripten errno that the
/// syscall layer returns negated; `Fatal` is a JavaScript exception that is not
/// an `ErrnoError` and therefore aborts the module in the glue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsError {
    Errno(i32),
    Fatal(String),
}

pub type FsResult<T> = Result<T, FsError>;

fn errno<T>(code: i32) -> FsResult<T> {
    Err(FsError::Errno(code))
}

/// Milliseconds since the epoch, the unit of JavaScript `Date.now()`.
#[must_use]
pub fn date_now() -> f64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    // Date.now() is an integer count of milliseconds.
    f64::from(u32::try_from(elapsed.as_millis() / 1_000_000_000).unwrap_or(0)) * 1e9
        + f64::from(u32::try_from(elapsed.as_millis() % 1_000_000_000).unwrap_or(0))
}

/// Maps a host I/O error to the errno that Node's error code maps to through
/// the glue's `ERRNO_CODES` table.
#[must_use]
pub fn host_errno(error: &io::Error) -> i32 {
    let Some(raw) = error.raw_os_error() else {
        return EINVAL;
    };
    let table: &[(i32, i32)] = &[
        (libc::EPERM, EPERM),
        (libc::ENOENT, ENOENT),
        (libc::ESRCH, 71),
        (libc::EINTR, 27),
        (libc::EIO, EIO),
        (libc::ENXIO, ENXIO),
        (libc::E2BIG, 1),
        (libc::ENOEXEC, 45),
        (libc::EBADF, EBADF),
        (libc::ECHILD, 12),
        (libc::EAGAIN, EAGAIN),
        (libc::ENOMEM, ENOMEM),
        (libc::EACCES, EACCES),
        (libc::EFAULT, EFAULT),
        (libc::EBUSY, EBUSY),
        (libc::EEXIST, EEXIST),
        (libc::EXDEV, EXDEV),
        (libc::ENODEV, ENODEV),
        (libc::ENOTDIR, ENOTDIR),
        (libc::EISDIR, EISDIR),
        (libc::EINVAL, EINVAL),
        (libc::ENFILE, 41),
        (libc::EMFILE, EMFILE),
        (libc::ENOTTY, ENOTTY),
        (libc::ETXTBSY, 74),
        (libc::EFBIG, 22),
        (libc::ENOSPC, 51),
        (libc::ESPIPE, ESPIPE),
        (libc::EROFS, 69),
        (libc::EMLINK, 34),
        (libc::EPIPE, 64),
        (libc::ERANGE, 68),
        (libc::EDEADLK, 16),
        (libc::ENOLCK, 46),
        (libc::ENOSYS, ENOSYS),
        (libc::ENOTEMPTY, ENOTEMPTY),
        (libc::ENAMETOOLONG, 37),
        (libc::ELOOP, ELOOP),
        (libc::ENOTSUP, EOPNOTSUPP),
        (libc::EOVERFLOW, EOVERFLOW),
        (libc::EDQUOT, 19),
        (libc::ESTALE, 72),
        (libc::EILSEQ, 25),
        (libc::ECANCELED, 11),
    ];
    table
        .iter()
        .find(|(host, _)| *host == raw)
        .map_or(EINVAL, |(_, emscripten)| *emscripten)
}

fn host_error(error: &io::Error) -> FsError {
    FsError::Errno(host_errno(error))
}

#[must_use]
pub fn is_file(mode: u32) -> bool {
    mode & S_IFMT == S_IFREG
}
#[must_use]
pub fn is_dir(mode: u32) -> bool {
    mode & S_IFMT == S_IFDIR
}
#[must_use]
pub fn is_link(mode: u32) -> bool {
    mode & S_IFMT == S_IFLNK
}
#[must_use]
pub fn is_chrdev(mode: u32) -> bool {
    mode & S_IFMT == S_IFCHR
}
#[must_use]
pub fn is_fifo(mode: u32) -> bool {
    mode & S_IFMT == S_IFIFO
}
#[must_use]
pub fn is_socket(mode: u32) -> bool {
    mode & S_IFSOCK == S_IFSOCK
}

#[must_use]
pub const fn makedev(major: u32, minor: u32) -> u32 {
    (major << 8) | minor
}

// ---------------------------------------------------------------------------
// PATH and PATH_FS
// ---------------------------------------------------------------------------

#[must_use]
pub fn path_is_abs(path: &str) -> bool {
    path.starts_with('/')
}

fn normalize_array(parts: Vec<String>, allow_above_root: bool) -> Vec<String> {
    let mut parts = parts;
    let mut up = 0_usize;
    let mut index = parts.len();
    while index > 0 {
        index -= 1;
        let part = parts[index].clone();
        if part == "." {
            parts.remove(index);
        } else if part == ".." {
            parts.remove(index);
            up += 1;
        } else if up > 0 {
            parts.remove(index);
            up -= 1;
        }
    }
    if allow_above_root {
        for _ in 0..up {
            parts.insert(0, "..".to_owned());
        }
    }
    parts
}

#[must_use]
pub fn path_normalize(path: &str) -> String {
    let absolute = path_is_abs(path);
    let trailing = path.ends_with('/');
    let parts = path
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect();
    let mut joined = normalize_array(parts, !absolute).join("/");
    if joined.is_empty() && !absolute {
        ".".clone_into(&mut joined);
    }
    if !joined.is_empty() && trailing {
        joined.push('/');
    }
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// `PATH.splitPath`: root, directory, basename and extension.
fn split_path(path: &str) -> (String, String, String) {
    let (root, rest) = if let Some(stripped) = path.strip_prefix('/') {
        ("/", stripped)
    } else {
        ("", path)
    };
    let trimmed = rest.trim_end_matches('/');
    let trailing = &rest[trimmed.len()..];
    let _ = trailing;
    match trimmed.rfind('/') {
        Some(slash) => (
            root.to_owned(),
            trimmed[..=slash].to_owned(),
            trimmed[slash + 1..].to_owned(),
        ),
        None => (root.to_owned(), String::new(), trimmed.to_owned()),
    }
}

#[must_use]
pub fn path_dirname(path: &str) -> String {
    let (root, mut directory, _) = split_path(path);
    if root.is_empty() && directory.is_empty() {
        return ".".to_owned();
    }
    if !directory.is_empty() {
        directory.pop();
    }
    format!("{root}{directory}")
}

#[must_use]
pub fn path_basename(path: &str) -> String {
    if path == "/" {
        return "/".to_owned();
    }
    let normalized = path_normalize(path);
    let normalized = normalized.strip_suffix('/').unwrap_or(&normalized);
    match normalized.rfind('/') {
        Some(slash) => normalized[slash + 1..].to_owned(),
        None => normalized.to_owned(),
    }
}

#[must_use]
pub fn path_join2(left: &str, right: &str) -> String {
    path_normalize(&format!("{left}/{right}"))
}

#[must_use]
pub fn path_join(parts: &[&str]) -> String {
    path_normalize(&parts.join("/"))
}

fn path_fs_resolve(cwd: &str, paths: &[&str]) -> String {
    let mut resolved = String::new();
    let mut absolute = false;
    for part in paths.iter().rev().copied().chain(std::iter::once(cwd)) {
        if absolute {
            break;
        }
        if part.is_empty() {
            return String::new();
        }
        resolved = format!("{part}/{resolved}");
        absolute = path_is_abs(part);
    }
    let parts = resolved
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect();
    let joined = normalize_array(parts, !absolute).join("/");
    let result = format!("{}{joined}", if absolute { "/" } else { "" });
    if result.is_empty() {
        ".".to_owned()
    } else {
        result
    }
}

/// The `trim` helper of `PATH_FS.relative`: drop leading and trailing empty
/// parts.
fn trim_empty<'a>(parts: &[&'a str]) -> Vec<&'a str> {
    let start = parts.iter().position(|part| !part.is_empty());
    let end = parts.iter().rposition(|part| !part.is_empty());
    match (start, end) {
        (Some(start), Some(end)) if start <= end => parts[start..=end].to_vec(),
        _ => Vec::new(),
    }
}

fn path_fs_relative(cwd: &str, from: &str, to: &str) -> String {
    let from = path_fs_resolve(cwd, &[from]);
    let to = path_fs_resolve(cwd, &[to]);
    let from = from.get(1..).unwrap_or("");
    let to = to.get(1..).unwrap_or("");
    let from_parts = trim_empty(&from.split('/').collect::<Vec<_>>());
    let to_parts = trim_empty(&to.split('/').collect::<Vec<_>>());
    let length = from_parts.len().min(to_parts.len());
    let mut same = length;
    for index in 0..length {
        if from_parts[index] != to_parts[index] {
            same = index;
            break;
        }
    }
    let mut output: Vec<&str> = std::iter::repeat_n("..", from_parts.len() - same).collect();
    output.extend_from_slice(&to_parts[same..]);
    output.join("/")
}

// ---------------------------------------------------------------------------
// Nodes, mounts, streams
// ---------------------------------------------------------------------------

pub type NodeId = usize;
pub type MountId = usize;

/// JavaScript object property order: integer-like keys first in ascending
/// numeric order, then string keys in insertion order. MEMFS directories are
/// plain objects, so `readdir` follows this order.
#[derive(Default, Debug, Clone)]
pub struct JsObjectMap {
    entries: Vec<(String, NodeId)>,
}

fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value: u64 = key.parse().ok()?;
    if value < 4_294_967_295 {
        u32::try_from(value).ok()
    } else {
        None
    }
}

impl JsObjectMap {
    fn set(&mut self, key: &str, id: NodeId) {
        if let Some(entry) = self.entries.iter_mut().find(|(name, _)| name == key) {
            entry.1 = id;
        } else {
            self.entries.push((key.to_owned(), id));
        }
    }
    fn remove(&mut self, key: &str) {
        self.entries.retain(|(name, _)| name != key);
    }
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    fn keys(&self) -> Vec<String> {
        let mut indexed: Vec<(u32, String)> = self
            .entries
            .iter()
            .filter_map(|(name, _)| array_index(name).map(|index| (index, name.clone())))
            .collect();
        indexed.sort_by_key(|(index, _)| *index);
        let mut keys: Vec<String> = indexed.into_iter().map(|(_, name)| name).collect();
        keys.extend(
            self.entries
                .iter()
                .filter(|(name, _)| array_index(name).is_none())
                .map(|(name, _)| name.clone()),
        );
        keys
    }
}

#[derive(Debug, Clone)]
pub enum NodeKind {
    MemDir(JsObjectMap),
    MemFile(Vec<u8>),
    MemLink(String),
    MemChrdev,
    Host,
    Proxy,
    Pipe(Rc<RefCell<Pipe>>),
    Socket,
    ProcFdDir,
    Plain,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub id: u32,
    pub parent: NodeId,
    pub name: String,
    pub mode: u32,
    pub rdev: u32,
    pub mount: MountId,
    pub mounted: Option<MountId>,
    pub atime: f64,
    pub mtime: f64,
    pub ctime: f64,
    pub kind: NodeKind,
}

#[derive(Debug, Clone)]
pub enum MountKind {
    Mem,
    Host(PathBuf),
    Proxy {
        target: Rc<RefCell<Fs>>,
        root: String,
    },
    Sock,
    Pipe,
    ProcFd,
}

impl std::fmt::Debug for Fs {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Fs")
    }
}

#[derive(Debug, Clone)]
pub struct Mount {
    pub kind: MountKind,
    pub mountpoint: String,
    pub root: NodeId,
    pub mounts: Vec<MountId>,
}

#[derive(Debug)]
pub struct Pipe {
    buckets: Vec<PipeBucket>,
    refcnt: u32,
}

#[derive(Debug)]
struct PipeBucket {
    buffer: Vec<u8>,
    offset: usize,
    roffset: usize,
}

const PIPE_BUCKET: usize = 8192;

/// Device operations registered under a device number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Null,
    Tty(usize),
    /// `FS.createDevice` with an input callback that yields bytes.
    Random,
    /// `FS.createDevice` input callback returning `null` (end of input).
    InputNull,
    /// `PGlite` `/dev/blob`.
    Blob,
}

#[derive(Debug, Default, Clone)]
pub struct Tty {
    pub output: Vec<u8>,
    pub sink: usize,
}

pub struct StreamShared {
    pub flags: i32,
    pub position: i64,
    pub refcount: u32,
    pub host: Option<Rc<host::File>>,
}

/// Which `stream_ops` table a stream uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamOps {
    MemDir,
    MemFile,
    MemLink,
    Chrdev,
    Device(Device),
    Host,
    Proxy,
    Pipe,
    Socket,
    ProcFd,
}

pub struct Stream {
    pub node: NodeId,
    pub path: String,
    pub shared: Rc<RefCell<StreamShared>>,
    pub seekable: bool,
    pub ops: StreamOps,
    pub tty: Option<usize>,
    pub getdents: Option<Vec<String>>,
    pub proxy_fd: Option<i32>,
    pub fd: Option<i32>,
}

impl Stream {
    #[must_use]
    pub fn flags(&self) -> i32 {
        self.shared.borrow().flags
    }
    #[must_use]
    pub fn position(&self) -> i64 {
        self.shared.borrow().position
    }
    fn set_position(&self, position: i64) {
        self.shared.borrow_mut().position = position;
    }
}

/// Attributes returned by `node_ops.getattr`.
#[derive(Debug, Clone, Copy)]
pub struct Stat {
    pub dev: i64,
    pub ino: u64,
    pub mode: u32,
    pub nlink: u32,
    pub uid: i64,
    pub gid: i64,
    pub rdev: i64,
    pub size: i64,
    pub atime: f64,
    pub mtime: f64,
    pub ctime: f64,
    pub blocks: i64,
}

/// Values for `setattr`.
#[derive(Default, Debug, Clone, Copy)]
pub struct SetAttr {
    pub mode: Option<u32>,
    pub size: Option<i64>,
    pub atime: Option<f64>,
    pub mtime: Option<f64>,
    pub ctime: Option<f64>,
}

#[derive(Default, Clone, Copy)]
pub struct LookupOptions {
    pub parent: bool,
    pub follow: bool,
    pub follow_mount: Option<bool>,
    pub noent_okay: bool,
}

pub struct Lookup {
    pub path: String,
    pub node: Option<NodeId>,
}

/// Collected text written to the TTY devices, split by the glue's line rule.
/// With `discard` set, lines are dropped, like `PGlite`'s `print` and
/// `printErr` at debug level 0, so a long-lived database does not grow it.
#[derive(Default, Debug, Clone)]
pub struct Console {
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
    pub discard: bool,
}

impl Console {
    fn push(&mut self, sink: usize, text: String) {
        if self.discard {
            return;
        }
        if sink == 0 {
            self.stdout.push(text);
        } else {
            self.stderr.push(text);
        }
    }
}

pub struct Fs {
    nodes: Vec<Option<Node>>,
    name_table: HashMap<(u32, String), Vec<NodeId>>,
    pub mounts: Vec<Mount>,
    pub root: Option<NodeId>,
    pub streams: Vec<Option<Stream>>,
    devices: HashMap<u32, Device>,
    pub ttys: HashMap<u32, Tty>,
    next_inode: u32,
    current_path: String,
    pub ignore_permissions: bool,
    pub initialized: bool,
    create_device_major: u32,
    pipe_names: u32,
    socket_names: u32,
    random_pool: Vec<u8>,
    pub console: Console,
    pub blob: Option<Vec<u8>>,
    pub blob_written: Vec<Vec<u8>>,
    pub sock_root: Option<NodeId>,
    pub pipe_root: Option<NodeId>,
}

impl Default for Fs {
    fn default() -> Self {
        Self::new()
    }
}

impl Fs {
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            name_table: HashMap::new(),
            mounts: Vec::new(),
            root: None,
            streams: Vec::new(),
            devices: HashMap::new(),
            ttys: HashMap::new(),
            next_inode: 1,
            current_path: "/".to_owned(),
            ignore_permissions: true,
            initialized: false,
            create_device_major: 64,
            pipe_names: 0,
            socket_names: 0,
            random_pool: Vec::new(),
            console: Console::default(),
            blob: None,
            blob_written: Vec::new(),
            sock_root: None,
            pipe_root: None,
        }
    }

    #[must_use]
    pub fn node(&self, id: NodeId) -> &Node {
        self.nodes[id]
            .as_ref()
            .unwrap_or_else(|| unreachable_node(id))
    }

    fn node_mut(&mut self, id: NodeId) -> &mut Node {
        self.nodes[id]
            .as_mut()
            .unwrap_or_else(|| unreachable_node(id))
    }

    #[must_use]
    pub fn cwd(&self) -> String {
        self.current_path.clone()
    }

    // ----- node table -----------------------------------------------------

    fn alloc_node(
        &mut self,
        parent: Option<NodeId>,
        name: &str,
        mode: u32,
        rdev: u32,
        kind: NodeKind,
        mount: MountId,
    ) -> NodeId {
        let id = self.next_inode;
        self.next_inode += 1;
        let index = self.nodes.len();
        let now = date_now();
        self.nodes.push(Some(Node {
            id,
            parent: parent.unwrap_or(index),
            name: name.to_owned(),
            mode,
            rdev,
            mount,
            mounted: None,
            atime: now,
            mtime: now,
            ctime: now,
            kind,
        }));
        index
    }

    /// `FS.createNode`: allocate and register in the name table.
    fn create_node(
        &mut self,
        parent: Option<NodeId>,
        name: &str,
        mode: u32,
        rdev: u32,
        kind: NodeKind,
        mount: Option<MountId>,
    ) -> NodeId {
        let mount = match (mount, parent) {
            (Some(mount), _) => mount,
            (None, Some(parent)) => self.node(parent).mount,
            (None, None) => 0,
        };
        let id = self.alloc_node(parent, name, mode, rdev, kind, mount);
        self.hash_add(id);
        id
    }

    fn hash_add(&mut self, id: NodeId) {
        let node = self.node(id);
        let parent_inode = self.node(node.parent).id;
        let key = (parent_inode, node.name.clone());
        self.name_table.entry(key).or_default().insert(0, id);
    }

    fn hash_remove(&mut self, id: NodeId) {
        let node = self.node(id);
        let parent_inode = self.node(node.parent).id;
        let key = (parent_inode, node.name.clone());
        if let Some(chain) = self.name_table.get_mut(&key) {
            if let Some(position) = chain.iter().position(|candidate| *candidate == id) {
                chain.remove(position);
            }
            if chain.is_empty() {
                self.name_table.remove(&key);
            }
        }
    }

    fn destroy_node(&mut self, id: NodeId) {
        self.hash_remove(id);
    }

    #[must_use]
    pub fn is_root(&self, id: NodeId) -> bool {
        self.node(id).parent == id
    }

    fn is_mountpoint(&self, id: NodeId) -> bool {
        self.node(id).mounted.is_some()
    }

    /// `FS.getPath`.
    #[must_use]
    pub fn get_path(&self, id: NodeId) -> String {
        let mut suffix: Option<String> = None;
        let mut current = id;
        loop {
            if self.is_root(current) {
                let mount = &self.mounts[self.node(current).mount];
                let mountpoint = mount.mountpoint.clone();
                return match suffix {
                    Some(suffix) => {
                        if mountpoint.ends_with('/') {
                            format!("{mountpoint}{suffix}")
                        } else {
                            format!("{mountpoint}/{suffix}")
                        }
                    }
                    None => mountpoint,
                };
            }
            let name = self.node(current).name.clone();
            suffix = Some(match suffix {
                Some(suffix) => format!("{name}/{suffix}"),
                None => name,
            });
            current = self.node(current).parent;
        }
    }

    /// `realPath` of NODEFS and PROXYFS: mount root joined with node names.
    fn real_path(&self, id: NodeId) -> String {
        let mut parts = Vec::new();
        let mut current = id;
        while self.node(current).parent != current {
            parts.push(self.node(current).name.clone());
            current = self.node(current).parent;
        }
        let root = match &self.mounts[self.node(current).mount].kind {
            MountKind::Host(root) => root.to_string_lossy().into_owned(),
            MountKind::Proxy { root, .. } => root.clone(),
            _ => String::new(),
        };
        parts.push(root);
        parts.reverse();
        let borrowed: Vec<&str> = parts.iter().map(String::as_str).collect();
        path_join(&borrowed)
    }

    fn proxy_target(&self, id: NodeId) -> Option<Rc<RefCell<Fs>>> {
        match &self.mounts[self.node(id).mount].kind {
            MountKind::Proxy { target, .. } => Some(Rc::clone(target)),
            _ => None,
        }
    }

    fn node_permissions(&self, id: NodeId, permission: &str) -> i32 {
        if self.ignore_permissions {
            return 0;
        }
        let mode = self.node(id).mode;
        if (permission.contains('r') && mode & 0o444 == 0)
            || (permission.contains('w') && mode & 0o222 == 0)
            || (permission.contains('x') && mode & 0o111 == 0)
        {
            EACCES
        } else {
            0
        }
    }

    fn has_lookup(&self, id: NodeId) -> bool {
        matches!(
            self.node(id).kind,
            NodeKind::MemDir(_) | NodeKind::Host | NodeKind::Proxy | NodeKind::ProcFdDir
        ) || (is_dir(self.node(id).mode) && matches!(self.node(id).kind, NodeKind::Plain))
    }

    fn may_lookup(&self, id: NodeId) -> i32 {
        if !is_dir(self.node(id).mode) {
            return ENOTDIR;
        }
        let permission = self.node_permissions(id, "x");
        if permission != 0 {
            return permission;
        }
        if self.has_lookup(id) { 0 } else { EACCES }
    }

    /// `FS.lookupNode`.
    pub fn lookup_node(&mut self, parent: NodeId, name: &str) -> FsResult<NodeId> {
        let error = self.may_lookup(parent);
        if error != 0 {
            return errno(error);
        }
        let key = (self.node(parent).id, name.to_owned());
        if let Some(chain) = self.name_table.get(&key) {
            for candidate in chain {
                let node = self.node(*candidate);
                if self.node(node.parent).id == self.node(parent).id && node.name == name {
                    return Ok(*candidate);
                }
            }
        }
        self.op_lookup(parent, name)
    }

    fn op_lookup(&mut self, parent: NodeId, name: &str) -> FsResult<NodeId> {
        match &self.node(parent).kind {
            NodeKind::MemDir(_) | NodeKind::Plain => errno(ENOENT),
            NodeKind::Host => {
                let path = path_join2(&self.real_path(parent), name);
                let mode = host_lstat(&path)?.mode;
                self.host_create_node(Some(parent), name, mode)
            }
            NodeKind::Proxy => {
                let target = self.proxy_target(parent).ok_or(FsError::Errno(EINVAL))?;
                let path = path_join2(&self.real_path(parent), name);
                let mode = target.borrow_mut().lstat(&path)?.mode;
                self.proxy_create_node(Some(parent), name, mode)
            }
            NodeKind::ProcFdDir => {
                // The glue returns a detached fake node whose readlink yields
                // the stream path. It is only reached through readlink.
                let fd: i32 = name.parse().unwrap_or(-1);
                let path = self.get_stream_checked(fd)?.path.clone();
                let mount = self.node(parent).mount;
                let id = self.alloc_node(
                    None,
                    name,
                    S_IFLNK | 0o777,
                    0,
                    NodeKind::MemLink(path),
                    mount,
                );
                self.node_mut(id).id = u32::try_from(fd + 1).unwrap_or(0);
                Ok(id)
            }
            _ => errno(EACCES),
        }
    }

    fn host_create_node(
        &mut self,
        parent: Option<NodeId>,
        name: &str,
        mode: u32,
    ) -> FsResult<NodeId> {
        if !is_dir(mode) && !is_file(mode) && !is_link(mode) {
            return errno(EINVAL);
        }
        Ok(self.create_node(parent, name, mode, 0, NodeKind::Host, None))
    }

    fn proxy_create_node(
        &mut self,
        parent: Option<NodeId>,
        name: &str,
        mode: u32,
    ) -> FsResult<NodeId> {
        if !is_dir(mode) && !is_file(mode) && !is_link(mode) {
            return errno(EINVAL);
        }
        Ok(self.create_node(parent, name, mode, 0, NodeKind::Proxy, None))
    }

    fn mem_create_node(
        &mut self,
        parent: Option<NodeId>,
        name: &str,
        mode: u32,
        rdev: u32,
        mount: Option<MountId>,
    ) -> FsResult<NodeId> {
        if mode & S_IFMT == 24576 || is_fifo(mode) {
            return errno(EPERM);
        }
        let kind = if is_dir(mode) {
            NodeKind::MemDir(JsObjectMap::default())
        } else if is_file(mode) {
            NodeKind::MemFile(Vec::new())
        } else if is_link(mode) {
            NodeKind::MemLink(String::new())
        } else if is_chrdev(mode) {
            NodeKind::MemChrdev
        } else {
            NodeKind::Plain
        };
        let id = self.create_node(parent, name, mode, rdev, kind, mount);
        let now = date_now();
        {
            let node = self.node_mut(id);
            node.atime = now;
            node.mtime = now;
            node.ctime = now;
        }
        if let Some(parent) = parent {
            if let NodeKind::MemDir(contents) = &mut self.node_mut(parent).kind {
                contents.set(name, id);
            }
            let parent_node = self.node_mut(parent);
            parent_node.atime = now;
            parent_node.mtime = now;
            parent_node.ctime = now;
        }
        Ok(id)
    }

    // ----- mounts -----------------------------------------------------------

    /// `FS.mount`.
    pub fn mount(&mut self, kind: &MountKind, mountpoint: Option<&str>) -> FsResult<NodeId> {
        let is_root = mountpoint == Some("/");
        let pseudo = mountpoint.is_none();
        if is_root && self.root.is_some() {
            return errno(EBUSY);
        }
        let mut point_node = None;
        let mut point_path = mountpoint.unwrap_or("").to_owned();
        if !is_root && !pseudo {
            let lookup = self.lookup_path(
                &point_path,
                LookupOptions {
                    follow_mount: Some(false),
                    ..LookupOptions::default()
                },
            )?;
            point_path = lookup.path;
            let node = lookup.node.ok_or(FsError::Errno(ENOENT))?;
            if self.is_mountpoint(node) {
                return errno(EBUSY);
            }
            if !is_dir(self.node(node).mode) {
                return errno(ENOTDIR);
            }
            point_node = Some(node);
        }
        let mount_id = self.mounts.len();
        self.mounts.push(Mount {
            kind: kind.clone(),
            mountpoint: point_path,
            root: 0,
            mounts: Vec::new(),
        });
        let root = match kind {
            MountKind::Mem => self.mem_create_node(None, "/", 16895, 0, Some(mount_id))?,
            MountKind::Host(root) => {
                let mode = host_lstat(&root.to_string_lossy())?.mode;
                let id = self.create_node(None, "/", mode, 0, NodeKind::Host, Some(mount_id));
                if !is_dir(mode) && !is_file(mode) && !is_link(mode) {
                    return errno(EINVAL);
                }
                id
            }
            MountKind::Proxy { target, root } => {
                let mode = target.borrow_mut().lstat(root)?.mode;
                self.create_node(None, "/", mode, 0, NodeKind::Proxy, Some(mount_id))
            }
            MountKind::Sock | MountKind::Pipe => {
                self.create_node(None, "/", 16895, 0, NodeKind::Plain, Some(mount_id))
            }
            MountKind::ProcFd => {
                let parent = point_node.ok_or(FsError::Errno(ENOENT))?;
                let parent_of_point = self.node(parent).parent;
                self.create_node(
                    Some(parent_of_point),
                    "fd",
                    16895,
                    73,
                    NodeKind::ProcFdDir,
                    Some(mount_id),
                )
            }
        };
        self.node_mut(root).mount = mount_id;
        self.mounts[mount_id].root = root;
        if is_root {
            self.root = Some(root);
        } else if let Some(point) = point_node {
            self.node_mut(point).mounted = Some(mount_id);
            let parent_mount = self.node(point).mount;
            self.mounts[parent_mount].mounts.push(mount_id);
        }
        Ok(root)
    }

    // ----- path lookup ------------------------------------------------------

    /// `FS.lookupPath`.
    pub fn lookup_path(&mut self, path: &str, options: LookupOptions) -> FsResult<Lookup> {
        if path.is_empty() {
            return Ok(Lookup {
                path: String::new(),
                node: None,
            });
        }
        let follow_mount = options.follow_mount.unwrap_or(true);
        let mut path = if path_is_abs(path) {
            path.to_owned()
        } else {
            format!("{}/{path}", self.cwd())
        };
        'restart: for _ in 0..40 {
            let parts: Vec<String> = path
                .split('/')
                .filter(|part| !part.is_empty() && *part != ".")
                .map(str::to_owned)
                .collect();
            let mut current = self.root.ok_or(FsError::Errno(ENOENT))?;
            let mut current_path = "/".to_owned();
            for (index, part) in parts.iter().enumerate() {
                let last = index == parts.len() - 1;
                if last && options.parent {
                    break;
                }
                if part == ".." {
                    current_path = path_dirname(&current_path);
                    current = self.node(current).parent;
                    continue;
                }
                current_path = path_join2(&current_path, part);
                match self.lookup_node(current, part) {
                    Ok(node) => current = node,
                    Err(FsError::Errno(ENOENT)) if last && options.noent_okay => {
                        return Ok(Lookup {
                            path: current_path,
                            node: None,
                        });
                    }
                    Err(error) => return Err(error),
                }
                if self.is_mountpoint(current) && (!last || follow_mount) {
                    let mount = self.node(current).mounted.unwrap_or(0);
                    current = self.mounts[mount].root;
                }
                if is_link(self.node(current).mode) && (!last || options.follow) {
                    let target = self.readlink_node(current)?;
                    let target = if path_is_abs(&target) {
                        target
                    } else {
                        format!("{}/{target}", path_dirname(&current_path))
                    };
                    path = format!("{target}/{}", parts[index + 1..].join("/"));
                    continue 'restart;
                }
            }
            return Ok(Lookup {
                path: current_path,
                node: Some(current),
            });
        }
        errno(ELOOP)
    }

    fn lookup_follow(&mut self, path: &str, follow: bool) -> FsResult<Lookup> {
        self.lookup_path(
            path,
            LookupOptions {
                follow,
                ..LookupOptions::default()
            },
        )
    }

    fn lookup_parent(&mut self, path: &str) -> FsResult<Lookup> {
        self.lookup_path(
            path,
            LookupOptions {
                parent: true,
                ..LookupOptions::default()
            },
        )
    }

    // ----- may* checks ------------------------------------------------------

    fn may_create(&mut self, directory: NodeId, name: &str) -> i32 {
        if !is_dir(self.node(directory).mode) {
            return ENOTDIR;
        }
        if self.lookup_node(directory, name).is_ok() {
            return EEXIST;
        }
        self.node_permissions(directory, "wx")
    }

    fn may_delete(&mut self, directory: NodeId, name: &str, is_directory: bool) -> i32 {
        let node = match self.lookup_node(directory, name) {
            Ok(node) => node,
            Err(FsError::Errno(code)) => return code,
            Err(FsError::Fatal(_)) => return EIO,
        };
        let permission = self.node_permissions(directory, "wx");
        if permission != 0 {
            return permission;
        }
        if is_directory {
            if !is_dir(self.node(node).mode) {
                return ENOTDIR;
            }
            if self.is_root(node) || self.get_path(node) == self.cwd() {
                return EBUSY;
            }
        } else if is_dir(self.node(node).mode) {
            return EISDIR;
        }
        0
    }

    fn flags_to_permission(flags: i32) -> String {
        let mut permission = ["r", "w", "rw"]
            .get(usize::try_from(flags & 3).unwrap_or(0))
            .copied()
            .unwrap_or("")
            .to_owned();
        if flags & O_TRUNC != 0 {
            permission.push('w');
        }
        permission
    }

    fn may_open(&self, node: Option<NodeId>, flags: i32) -> i32 {
        let Some(node) = node else {
            return ENOENT;
        };
        let mode = self.node(node).mode;
        if is_link(mode) {
            return ELOOP;
        }
        if is_dir(mode) && (Self::flags_to_permission(flags) != "r" || flags & O_TRUNC != 0) {
            return EISDIR;
        }
        self.node_permissions(node, &Self::flags_to_permission(flags))
    }

    // ----- node operations by filesystem ----------------------------------

    fn has_mknod(&self, id: NodeId) -> bool {
        matches!(
            self.node(id).kind,
            NodeKind::MemDir(_) | NodeKind::Host | NodeKind::Proxy
        )
    }

    fn op_mknod(&mut self, parent: NodeId, name: &str, mode: u32, rdev: u32) -> FsResult<NodeId> {
        match self.node(parent).kind {
            NodeKind::MemDir(_) => self.mem_create_node(Some(parent), name, mode, rdev, None),
            NodeKind::Host => {
                let id = self.host_create_node(Some(parent), name, mode)?;
                let path = self.real_path(id);
                let node_mode = self.node(id).mode;
                let result = if is_dir(node_mode) {
                    host::DirBuilder::new()
                        .mode(node_mode & 0o7777)
                        .create(&path)
                } else {
                    host::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(true)
                        .mode(node_mode & 0o7777)
                        .open(&path)
                        .map(|_| ())
                };
                result.map_err(|error| host_error(&error))?;
                Ok(id)
            }
            NodeKind::Proxy => {
                let id = self.proxy_create_node(Some(parent), name, mode)?;
                let path = self.real_path(id);
                let target = self.proxy_target(id).ok_or(FsError::Errno(EINVAL))?;
                let node_mode = self.node(id).mode;
                if is_dir(node_mode) {
                    target.borrow_mut().mkdir(&path, node_mode)?;
                } else {
                    target.borrow_mut().write_file(
                        &path,
                        &[],
                        O_CREAT | O_TRUNC | O_WRONLY,
                        node_mode,
                    )?;
                }
                Ok(id)
            }
            _ => errno(EPERM),
        }
    }

    /// `node_ops.getattr`.
    pub fn getattr(&mut self, id: NodeId) -> FsResult<Stat> {
        let node = self.node(id).clone();
        match &node.kind {
            NodeKind::Host => {
                let metadata = host::symlink_metadata(self.real_path(id))
                    .map_err(|error| host_error(&error))?;
                Ok(stat_from_metadata(&metadata))
            }
            NodeKind::Proxy => {
                let target = self.proxy_target(id).ok_or(FsError::Errno(EINVAL))?;
                let path = self.real_path(id);
                let stat = target.borrow_mut().lstat(&path)?;
                Ok(stat)
            }
            NodeKind::Pipe(_) | NodeKind::Socket | NodeKind::ProcFdDir | NodeKind::Plain
                if !matches!(node.kind, NodeKind::Plain)
                    || self.mounts[node.mount].kind_is_special() =>
            {
                errno(EPERM)
            }
            _ => {
                let size = match &node.kind {
                    _ if is_dir(node.mode) => 4096,
                    NodeKind::MemFile(data) => i64::try_from(data.len()).unwrap_or(i64::MAX),
                    NodeKind::MemLink(link) => i64::try_from(link.len()).unwrap_or(0),
                    _ => 0,
                };
                Ok(Stat {
                    dev: if is_chrdev(node.mode) {
                        i64::from(node.id)
                    } else {
                        1
                    },
                    ino: u64::from(node.id),
                    mode: node.mode,
                    nlink: 1,
                    uid: 0,
                    gid: 0,
                    rdev: i64::from(node.rdev),
                    size,
                    atime: node.atime,
                    mtime: node.mtime,
                    ctime: node.ctime,
                    blocks: (size + 4095) / 4096,
                })
            }
        }
    }

    fn has_setattr(&self, id: NodeId) -> bool {
        let node = self.node(id);
        match node.kind {
            NodeKind::Pipe(_) | NodeKind::Socket | NodeKind::ProcFdDir => false,
            NodeKind::Plain => !self.mounts[node.mount].kind_is_special(),
            _ => true,
        }
    }

    /// `node_ops.setattr`.
    pub fn setattr(&mut self, id: NodeId, attr: SetAttr) -> FsResult<()> {
        match self.node(id).kind.clone() {
            NodeKind::Host => {
                let path = self.real_path(id);
                if let Some(mode) = attr.mode {
                    host::set_permissions(&path, host::Permissions::from_mode(mode & 0o7777))
                        .map_err(|error| host_error(&error))?;
                    self.node_mut(id).mode = mode;
                }
                if attr.atime.is_some() || attr.mtime.is_some() {
                    set_host_times(&path, attr.atime, attr.mtime)?;
                }
                if let Some(size) = attr.size {
                    host::OpenOptions::new()
                        .write(true)
                        .open(&path)
                        .and_then(|file| file.set_len(u64::try_from(size).unwrap_or(0)))
                        .map_err(|error| host_error(&error))?;
                }
                Ok(())
            }
            NodeKind::Proxy => {
                let target = self.proxy_target(id).ok_or(FsError::Errno(EINVAL))?;
                let path = self.real_path(id);
                if let Some(mode) = attr.mode {
                    target.borrow_mut().chmod_path(&path, mode, false)?;
                    self.node_mut(id).mode = mode;
                }
                if attr.atime.is_some() || attr.mtime.is_some() {
                    let atime = attr.atime.or(attr.mtime);
                    let mtime = attr.mtime.or(attr.atime);
                    target.borrow_mut().utime(&path, atime, mtime)?;
                }
                if let Some(size) = attr.size {
                    target.borrow_mut().truncate_path(&path, size)?;
                }
                Ok(())
            }
            _ => {
                let node = self.node_mut(id);
                // MEMFS copies truthy mode, atime, mtime and ctime.
                if let Some(mode) = attr.mode.filter(|mode| *mode != 0) {
                    node.mode = mode;
                }
                if let Some(atime) = attr.atime.filter(|value| *value != 0.0) {
                    node.atime = atime;
                }
                if let Some(mtime) = attr.mtime.filter(|value| *value != 0.0) {
                    node.mtime = mtime;
                }
                if let Some(ctime) = attr.ctime.filter(|value| *value != 0.0) {
                    node.ctime = ctime;
                }
                if let Some(size) = attr.size
                    && let NodeKind::MemFile(data) = &mut node.kind
                {
                    data.resize(usize::try_from(size).unwrap_or(0), 0);
                }
                Ok(())
            }
        }
    }

    fn readlink_node(&mut self, id: NodeId) -> FsResult<String> {
        match &self.node(id).kind {
            NodeKind::MemLink(link) => Ok(link.clone()),
            NodeKind::Host => {
                let path = self.real_path(id);
                host::read_link(path)
                    .map(|target| target.to_string_lossy().into_owned())
                    .map_err(|error| host_error(&error))
            }
            NodeKind::Proxy => {
                let target = self.proxy_target(id).ok_or(FsError::Errno(EINVAL))?;
                let path = self.real_path(id);
                let link = target.borrow_mut().readlink(&path)?;
                Ok(link)
            }
            _ if is_link(self.node(id).mode) => errno(EINVAL),
            _ => errno(ENOSYS),
        }
    }

    fn has_readlink(&self, id: NodeId) -> bool {
        matches!(
            self.node(id).kind,
            NodeKind::MemLink(_) | NodeKind::Host | NodeKind::Proxy
        )
    }

    fn op_readdir(&mut self, id: NodeId) -> FsResult<Vec<String>> {
        match &self.node(id).kind {
            NodeKind::MemDir(contents) => {
                let mut names = vec![".".to_owned(), "..".to_owned()];
                names.extend(contents.keys());
                Ok(names)
            }
            NodeKind::Host => {
                let path = self.real_path(id);
                host_readdir(&path)
            }
            NodeKind::Proxy => {
                let target = self.proxy_target(id).ok_or(FsError::Errno(EINVAL))?;
                let path = self.real_path(id);
                let names = target.borrow_mut().readdir(&path)?;
                Ok(names)
            }
            NodeKind::ProcFdDir => Ok(self
                .streams
                .iter()
                .enumerate()
                .filter(|(_, stream)| stream.is_some())
                .map(|(fd, _)| fd.to_string())
                .collect()),
            _ => errno(ENOTDIR),
        }
    }

    // ----- FS API -----------------------------------------------------------

    /// `FS.mknod`.
    pub fn mknod(&mut self, path: &str, mode: u32, rdev: u32) -> FsResult<NodeId> {
        let parent = self
            .lookup_parent(path)?
            .node
            .ok_or(FsError::Errno(ENOENT))?;
        let name = path_basename(path);
        if name.is_empty() || name == "." || name == ".." {
            return errno(EINVAL);
        }
        let error = self.may_create(parent, &name);
        if error != 0 {
            return errno(error);
        }
        if !self.has_mknod(parent) {
            return errno(EPERM);
        }
        self.op_mknod(parent, &name, mode, rdev)
    }

    pub fn create(&mut self, path: &str, mode: u32) -> FsResult<NodeId> {
        self.mknod(path, (mode & 4095) | S_IFREG, 0)
    }

    pub fn mkdir(&mut self, path: &str, mode: u32) -> FsResult<NodeId> {
        self.mknod(path, (mode & 1023) | S_IFDIR, 0)
    }

    pub fn mkdev(&mut self, path: &str, mode: Option<u32>, dev: u32) -> FsResult<NodeId> {
        let mode = mode.unwrap_or(438) | S_IFCHR;
        self.mknod(path, mode, dev)
    }

    pub fn symlink(&mut self, old: &str, new: &str) -> FsResult<NodeId> {
        if path_fs_resolve(&self.cwd(), &[old]).is_empty() {
            return errno(ENOENT);
        }
        let parent = self
            .lookup_parent(new)?
            .node
            .ok_or(FsError::Errno(ENOENT))?;
        let name = path_basename(new);
        let error = self.may_create(parent, &name);
        if error != 0 {
            return errno(error);
        }
        match self.node(parent).kind {
            NodeKind::MemDir(_) => {
                let id = self.mem_create_node(Some(parent), &name, 41471, 0, None)?;
                self.node_mut(id).kind = NodeKind::MemLink(old.to_owned());
                Ok(id)
            }
            NodeKind::Host => {
                let path = path_join2(&self.real_path(parent), &name);
                std::os::unix::fs::symlink(old, &path).map_err(|error| host_error(&error))?;
                // NODEFS.symlink does not return a node.
                Ok(parent)
            }
            NodeKind::Proxy => {
                let target = self.proxy_target(parent).ok_or(FsError::Errno(EINVAL))?;
                let path = path_join2(&self.real_path(parent), &name);
                target.borrow_mut().symlink(old, &path)?;
                Ok(parent)
            }
            _ => errno(EPERM),
        }
    }

    /// `FS.rename`.
    pub fn rename(&mut self, old_path: &str, new_path: &str) -> FsResult<()> {
        let old_dir = path_dirname(old_path);
        let new_dir = path_dirname(new_path);
        let old_name = path_basename(old_path);
        let new_name = path_basename(new_path);
        let old_parent = self.lookup_parent(old_path)?.node;
        let new_parent = self.lookup_parent(new_path)?.node;
        let (Some(old_parent), Some(new_parent)) = (old_parent, new_parent) else {
            return errno(ENOENT);
        };
        if self.node(old_parent).mount != self.node(new_parent).mount {
            return errno(EXDEV);
        }
        let old_node = self.lookup_node(old_parent, &old_name)?;
        let cwd = self.cwd();
        let relative = path_fs_relative(&cwd, old_path, &new_dir);
        if !relative.starts_with('.') {
            return errno(EINVAL);
        }
        let relative = path_fs_relative(&cwd, new_path, &old_dir);
        if !relative.starts_with('.') {
            return errno(ENOTEMPTY);
        }
        let new_node = self.lookup_node(new_parent, &new_name).ok();
        if Some(old_node) == new_node {
            return Ok(());
        }
        let directory = is_dir(self.node(old_node).mode);
        let error = self.may_delete(old_parent, &old_name, directory);
        if error != 0 {
            return errno(error);
        }
        let error = if new_node.is_some() {
            self.may_delete(new_parent, &new_name, directory)
        } else {
            self.may_create(new_parent, &new_name)
        };
        if error != 0 {
            return errno(error);
        }
        if !matches!(
            self.node(old_parent).kind,
            NodeKind::MemDir(_) | NodeKind::Host | NodeKind::Proxy
        ) {
            return errno(EPERM);
        }
        if self.is_mountpoint(old_node) || new_node.is_some_and(|node| self.is_mountpoint(node)) {
            return errno(EBUSY);
        }
        if new_parent != old_parent {
            let permission = self.node_permissions(old_parent, "w");
            if permission != 0 {
                return errno(permission);
            }
        }
        self.hash_remove(old_node);
        let result = self.op_rename(old_node, new_parent, &new_name);
        if result.is_ok() {
            self.node_mut(old_node).parent = new_parent;
        }
        self.hash_add(old_node);
        result
    }

    fn op_rename(&mut self, node: NodeId, new_parent: NodeId, new_name: &str) -> FsResult<()> {
        match self.node(node).kind.clone() {
            NodeKind::Host => {
                let old = self.real_path(node);
                let new = path_join2(&self.real_path(new_parent), new_name);
                // NODEFS.rename calls FS.unlink with the host path, which the
                // virtual filesystem normally does not contain.
                let _ = self.unlink(&new);
                host::rename(&old, &new).map_err(|error| host_error(&error))?;
                new_name.clone_into(&mut self.node_mut(node).name);
                Ok(())
            }
            NodeKind::Proxy => {
                let target = self.proxy_target(node).ok_or(FsError::Errno(EINVAL))?;
                let old = self.real_path(node);
                let new = path_join2(&self.real_path(new_parent), new_name);
                target.borrow_mut().rename(&old, &new)?;
                new_name.clone_into(&mut self.node_mut(node).name);
                Ok(())
            }
            _ => {
                if let Ok(existing) = self.lookup_node(new_parent, new_name) {
                    if is_dir(self.node(node).mode)
                        && let NodeKind::MemDir(contents) = &self.node(existing).kind
                        && !contents.is_empty()
                    {
                        return errno(ENOTEMPTY);
                    }
                    self.hash_remove(existing);
                }
                let old_parent = self.node(node).parent;
                let old_name = self.node(node).name.clone();
                if let NodeKind::MemDir(contents) = &mut self.node_mut(old_parent).kind {
                    contents.remove(&old_name);
                }
                if let NodeKind::MemDir(contents) = &mut self.node_mut(new_parent).kind {
                    contents.set(new_name, node);
                }
                new_name.clone_into(&mut self.node_mut(node).name);
                let now = date_now();
                for touched in [new_parent, old_parent] {
                    let touched = self.node_mut(touched);
                    touched.ctime = now;
                    touched.mtime = now;
                }
                Ok(())
            }
        }
    }

    pub fn rmdir(&mut self, path: &str) -> FsResult<()> {
        let parent = self
            .lookup_parent(path)?
            .node
            .ok_or(FsError::Errno(ENOENT))?;
        let name = path_basename(path);
        let node = self.lookup_node(parent, &name)?;
        let error = self.may_delete(parent, &name, true);
        if error != 0 {
            return errno(error);
        }
        if self.is_mountpoint(node) {
            return errno(EBUSY);
        }
        match self.node(parent).kind {
            NodeKind::MemDir(_) => {
                let child = self.lookup_node(parent, &name)?;
                if let NodeKind::MemDir(contents) = &self.node(child).kind
                    && !contents.is_empty()
                {
                    return errno(ENOTEMPTY);
                }
                if let NodeKind::MemDir(contents) = &mut self.node_mut(parent).kind {
                    contents.remove(&name);
                }
                let now = date_now();
                let parent_node = self.node_mut(parent);
                parent_node.ctime = now;
                parent_node.mtime = now;
            }
            NodeKind::Host => {
                let host_path = path_join2(&self.real_path(parent), &name);
                host::remove_dir(&host_path).map_err(|error| host_error(&error))?;
            }
            NodeKind::Proxy => {
                let target = self.proxy_target(parent).ok_or(FsError::Errno(EINVAL))?;
                let target_path = path_join2(&self.real_path(parent), &name);
                target.borrow_mut().rmdir(&target_path)?;
            }
            _ => return errno(EPERM),
        }
        self.destroy_node(node);
        Ok(())
    }

    pub fn readdir(&mut self, path: &str) -> FsResult<Vec<String>> {
        let node = self
            .lookup_follow(path, true)?
            .node
            .ok_or(FsError::Errno(ENOENT))?;
        if !matches!(
            self.node(node).kind,
            NodeKind::MemDir(_) | NodeKind::Host | NodeKind::Proxy | NodeKind::ProcFdDir
        ) {
            return errno(ENOTDIR);
        }
        self.op_readdir(node)
    }

    pub fn unlink(&mut self, path: &str) -> FsResult<()> {
        let parent = self
            .lookup_parent(path)?
            .node
            .ok_or(FsError::Errno(ENOENT))?;
        let name = path_basename(path);
        let node = self.lookup_node(parent, &name)?;
        let error = self.may_delete(parent, &name, false);
        if error != 0 {
            return errno(error);
        }
        if self.is_mountpoint(node) {
            return errno(EBUSY);
        }
        match self.node(parent).kind {
            NodeKind::MemDir(_) => {
                if let NodeKind::MemDir(contents) = &mut self.node_mut(parent).kind {
                    contents.remove(&name);
                }
                let now = date_now();
                let parent_node = self.node_mut(parent);
                parent_node.ctime = now;
                parent_node.mtime = now;
            }
            NodeKind::Host => {
                let host_path = path_join2(&self.real_path(parent), &name);
                host::remove_file(&host_path).map_err(|error| host_error(&error))?;
            }
            NodeKind::Proxy => {
                let target = self.proxy_target(parent).ok_or(FsError::Errno(EINVAL))?;
                let target_path = path_join2(&self.real_path(parent), &name);
                target.borrow_mut().unlink(&target_path)?;
            }
            _ => return errno(EPERM),
        }
        self.destroy_node(node);
        Ok(())
    }

    pub fn readlink(&mut self, path: &str) -> FsResult<String> {
        let node = self
            .lookup_path(path, LookupOptions::default())?
            .node
            .ok_or(FsError::Errno(ENOENT))?;
        if !self.has_readlink(node) {
            return errno(EINVAL);
        }
        self.readlink_node(node)
    }

    /// `FS.stat`.
    pub fn stat(&mut self, path: &str, no_follow: bool) -> FsResult<Stat> {
        let node = self
            .lookup_follow(path, !no_follow)?
            .node
            .ok_or(FsError::Errno(ENOENT))?;
        self.getattr(node)
    }

    pub fn lstat(&mut self, path: &str) -> FsResult<Stat> {
        self.stat(path, true)
    }

    pub fn chmod_node(&mut self, node: NodeId, mode: u32) -> FsResult<()> {
        if !self.has_setattr(node) {
            return errno(EPERM);
        }
        let current = self.node(node).mode;
        self.setattr(
            node,
            SetAttr {
                mode: Some((mode & 4095) | (current & !4095)),
                ctime: Some(date_now()),
                ..SetAttr::default()
            },
        )
    }

    pub fn chmod_path(&mut self, path: &str, mode: u32, no_follow: bool) -> FsResult<()> {
        let node = self
            .lookup_follow(path, !no_follow)?
            .node
            .ok_or(FsError::Errno(ENOENT))?;
        self.chmod_node(node, mode)
    }

    pub fn fchmod(&mut self, fd: i32, mode: u32) -> FsResult<()> {
        let node = self.get_stream_checked(fd)?.node;
        self.chmod_node(node, mode)
    }

    pub fn chown_node(&mut self, node: NodeId) -> FsResult<()> {
        if !self.has_setattr(node) {
            return errno(EPERM);
        }
        self.setattr(
            node,
            SetAttr {
                ..SetAttr::default()
            },
        )
    }

    pub fn chown_path(&mut self, path: &str, no_follow: bool) -> FsResult<()> {
        let node = self
            .lookup_follow(path, !no_follow)?
            .node
            .ok_or(FsError::Errno(ENOENT))?;
        self.chown_node(node)
    }

    pub fn truncate_node(&mut self, node: NodeId, length: i64) -> FsResult<()> {
        if length < 0 {
            return errno(EINVAL);
        }
        if !self.has_setattr(node) {
            return errno(EPERM);
        }
        let mode = self.node(node).mode;
        if is_dir(mode) {
            return errno(EISDIR);
        }
        if !is_file(mode) {
            return errno(EINVAL);
        }
        let permission = self.node_permissions(node, "w");
        if permission != 0 {
            return errno(permission);
        }
        self.setattr(
            node,
            SetAttr {
                size: Some(length),
                ..SetAttr::default()
            },
        )
    }

    pub fn truncate_path(&mut self, path: &str, length: i64) -> FsResult<()> {
        if length < 0 {
            return errno(EINVAL);
        }
        let node = self
            .lookup_follow(path, true)?
            .node
            .ok_or(FsError::Errno(ENOENT))?;
        self.truncate_node(node, length)
    }

    pub fn ftruncate(&mut self, fd: i32, length: i64) -> FsResult<()> {
        let stream = self.get_stream_checked(fd)?;
        if stream.flags() & O_ACCMODE == 0 {
            return errno(EINVAL);
        }
        let node = stream.node;
        self.truncate_node(node, length)
    }

    /// `FS.utime` with millisecond values.
    pub fn utime(&mut self, path: &str, atime: Option<f64>, mtime: Option<f64>) -> FsResult<()> {
        let node = self
            .lookup_follow(path, true)?
            .node
            .ok_or(FsError::Errno(ENOENT))?;
        self.setattr(
            node,
            SetAttr {
                atime,
                mtime,
                ..SetAttr::default()
            },
        )
    }

    pub fn chdir(&mut self, path: &str) -> FsResult<()> {
        let lookup = self.lookup_follow(path, true)?;
        let node = lookup.node.ok_or(FsError::Errno(ENOENT))?;
        if !is_dir(self.node(node).mode) {
            return errno(ENOTDIR);
        }
        let permission = self.node_permissions(node, "x");
        if permission != 0 {
            return errno(permission);
        }
        self.current_path = lookup.path;
        Ok(())
    }

    #[must_use]
    pub fn statfs_values(&self) -> [i64; 10] {
        // bsize, frsize, blocks, bfree, bavail, files, ffree, fsid, flags, namelen
        [
            4096,
            4096,
            1_000_000,
            500_000,
            500_000,
            i64::from(self.next_inode),
            i64::from(self.next_inode) - 1,
            42,
            2,
            255,
        ]
    }

    /// `FS.statfs`. NODEFS adds the host values.
    pub fn statfs(&mut self, path: &str) -> FsResult<[i64; 10]> {
        let mut values = self.statfs_values();
        let node = self.lookup_follow(path, true)?.node;
        if let Some(node) = node
            && let NodeKind::Host = self.node(node).kind
            && let MountKind::Host(root) = &self.mounts[self.node(node).mount].kind
        {
            let host_values = host_statfs(root);
            if let Some([bsize, blocks, bfree, bavail, files, ffree]) = host_values {
                values[0] = bsize;
                values[1] = bsize;
                values[2] = blocks;
                values[3] = bfree;
                values[4] = bavail;
                values[5] = files;
                values[6] = ffree;
            }
        }
        Ok(values)
    }

    // ----- streams ----------------------------------------------------------

    fn next_fd(&self) -> FsResult<i32> {
        for fd in 0..=MAX_OPEN_FDS {
            if self.streams.get(fd).is_none_or(Option::is_none) {
                return Ok(i32::try_from(fd).unwrap_or(0));
            }
        }
        errno(EMFILE)
    }

    pub fn get_stream(&self, fd: i32) -> Option<&Stream> {
        usize::try_from(fd)
            .ok()
            .and_then(|fd| self.streams.get(fd))
            .and_then(Option::as_ref)
    }

    pub fn get_stream_mut(&mut self, fd: i32) -> Option<&mut Stream> {
        usize::try_from(fd)
            .ok()
            .and_then(|fd| self.streams.get_mut(fd))
            .and_then(Option::as_mut)
    }

    pub fn get_stream_checked(&self, fd: i32) -> FsResult<&Stream> {
        self.get_stream(fd).ok_or(FsError::Errno(EBADF))
    }

    fn create_stream(&mut self, mut stream: Stream, fd: Option<i32>) -> FsResult<i32> {
        let fd = match fd {
            Some(fd) => fd,
            None => self.next_fd()?,
        };
        stream.fd = Some(fd);
        let index = usize::try_from(fd).map_err(|_| FsError::Errno(EBADF))?;
        if self.streams.len() <= index {
            self.streams.resize_with(index + 1, || None);
        }
        self.streams[index] = Some(stream);
        Ok(fd)
    }

    fn close_stream_slot(&mut self, fd: i32) {
        if let Some(slot) = usize::try_from(fd)
            .ok()
            .and_then(|fd| self.streams.get_mut(fd))
        {
            *slot = None;
        }
    }

    /// `FS.dupStream`.
    pub fn dup_stream(&mut self, fd: i32, target: Option<i32>) -> FsResult<i32> {
        let original = self.get_stream_checked(fd)?;
        let copy = Stream {
            node: original.node,
            path: original.path.clone(),
            shared: Rc::clone(&original.shared),
            seekable: original.seekable,
            ops: original.ops,
            tty: original.tty,
            getdents: original.getdents.clone(),
            proxy_fd: original.proxy_fd,
            fd: None,
        };
        let ops = copy.ops;
        let new_fd = self.create_stream(copy, target)?;
        if ops == StreamOps::Host
            && let Some(stream) = self.get_stream(new_fd)
        {
            stream.shared.borrow_mut().refcount += 1;
        }
        Ok(new_fd)
    }

    fn stream_ops_for(&self, node: NodeId) -> StreamOps {
        let node_ref = self.node(node);
        match &node_ref.kind {
            NodeKind::MemDir(_) | NodeKind::Plain => StreamOps::MemDir,
            NodeKind::MemFile(_) => StreamOps::MemFile,
            NodeKind::MemLink(_) => StreamOps::MemLink,
            NodeKind::MemChrdev => StreamOps::Chrdev,
            NodeKind::Host => StreamOps::Host,
            NodeKind::Proxy => StreamOps::Proxy,
            NodeKind::Pipe(_) => StreamOps::Pipe,
            NodeKind::Socket => StreamOps::Socket,
            NodeKind::ProcFdDir => StreamOps::ProcFd,
        }
    }

    /// `FS.open`. Returns the new file descriptor.
    pub fn open(&mut self, path: &str, flags: i32, mode: Option<u32>) -> FsResult<i32> {
        if path.is_empty() {
            return errno(ENOENT);
        }
        let mut flags = flags;
        let mode = if flags & O_CREAT != 0 {
            (mode.unwrap_or(438) & 4095) | S_IFREG
        } else {
            0
        };
        let lookup = self.lookup_path(
            path,
            LookupOptions {
                follow: flags & O_NOFOLLOW == 0,
                noent_okay: true,
                ..LookupOptions::default()
            },
        )?;
        let mut node = lookup.node;
        let path = lookup.path;
        let mut created = false;
        if flags & O_CREAT != 0 {
            if node.is_some() {
                if flags & O_EXCL != 0 {
                    return errno(EEXIST);
                }
            } else {
                node = Some(self.mknod(&path, mode, 0)?);
                created = true;
            }
        }
        let Some(node) = node else {
            return errno(ENOENT);
        };
        if is_chrdev(self.node(node).mode) {
            flags &= !O_TRUNC;
        }
        if flags & O_DIRECTORY != 0 && !is_dir(self.node(node).mode) {
            return errno(ENOTDIR);
        }
        if !created {
            let error = self.may_open(Some(node), flags);
            if error != 0 {
                return errno(error);
            }
        }
        if flags & O_TRUNC != 0 && !created {
            self.truncate_node(node, 0)?;
        }
        flags &= !(O_CREAT | O_EXCL | O_TRUNC | O_NOFOLLOW);
        let stream = Stream {
            node,
            path: self.get_path(node),
            shared: Rc::new(RefCell::new(StreamShared {
                flags,
                position: 0,
                refcount: 0,
                host: None,
            })),
            seekable: true,
            ops: self.stream_ops_for(node),
            tty: None,
            getdents: None,
            proxy_fd: None,
            fd: None,
        };
        let fd = self.create_stream(stream, None)?;
        self.stream_open(fd)?;
        Ok(fd)
    }

    fn stream_open(&mut self, fd: i32) -> FsResult<()> {
        let stream = self.get_stream_checked(fd)?;
        let node = stream.node;
        match stream.ops {
            StreamOps::Chrdev => {
                let rdev = self.node(node).rdev;
                let device = self.devices.get(&rdev).copied().ok_or(FsError::Fatal(
                    "Cannot read properties of undefined (reading 'stream_ops')".into(),
                ))?;
                let stream = self.get_stream_mut(fd).ok_or(FsError::Errno(EBADF))?;
                stream.ops = StreamOps::Device(device);
                match device {
                    Device::Tty(_) => {
                        if !self.ttys.contains_key(&rdev) {
                            return errno(ENXIO);
                        }
                        let stream = self.get_stream_mut(fd).ok_or(FsError::Errno(EBADF))?;
                        stream.tty = Some(usize::try_from(rdev).unwrap_or(0));
                        stream.seekable = false;
                    }
                    Device::Random | Device::InputNull => {
                        let stream = self.get_stream_mut(fd).ok_or(FsError::Errno(EBADF))?;
                        stream.seekable = false;
                    }
                    Device::Null | Device::Blob => {}
                }
                Ok(())
            }
            StreamOps::Host => {
                if is_file(self.node(node).mode) {
                    let path = self.real_path(node);
                    let flags = stream.flags();
                    let file = host_open(&path, flags)?;
                    let stream = self.get_stream_checked(fd)?;
                    let mut shared = stream.shared.borrow_mut();
                    shared.refcount = 1;
                    shared.host = Some(Rc::new(file));
                }
                Ok(())
            }
            StreamOps::Proxy => {
                let target = self.proxy_target(node).ok_or(FsError::Errno(EINVAL))?;
                let path = self.real_path(node);
                let flags = stream.flags();
                let target_fd = target.borrow_mut().open(&path, flags, None)?;
                let stream = self.get_stream_mut(fd).ok_or(FsError::Errno(EBADF))?;
                stream.proxy_fd = Some(target_fd);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// `FS.close`.
    pub fn close(&mut self, fd: i32) -> FsResult<()> {
        let stream = self.get_stream_checked(fd)?;
        let ops = stream.ops;
        let node = stream.node;
        let result = self.stream_close(fd, ops, node);
        self.close_stream_slot(fd);
        result
    }

    fn stream_close(&mut self, fd: i32, ops: StreamOps, node: NodeId) -> FsResult<()> {
        match ops {
            StreamOps::Host => {
                if is_file(self.node(node).mode) {
                    let stream = self.get_stream_checked(fd)?;
                    let mut shared = stream.shared.borrow_mut();
                    if shared.host.is_some() {
                        shared.refcount = shared.refcount.saturating_sub(1);
                        if shared.refcount == 0 {
                            shared.host = None;
                        }
                    }
                }
                Ok(())
            }
            StreamOps::Proxy => {
                let target = self.proxy_target(node).ok_or(FsError::Errno(EINVAL))?;
                let target_fd = self.get_stream_checked(fd)?.proxy_fd.unwrap_or(-1);
                target.borrow_mut().close(target_fd)?;
                Ok(())
            }
            StreamOps::Device(Device::Tty(_)) => {
                let tty = self.get_stream_checked(fd)?.tty.unwrap_or(0);
                self.tty_flush(u32::try_from(tty).unwrap_or(0));
                Ok(())
            }
            StreamOps::Pipe => {
                if let NodeKind::Pipe(pipe) = &self.node(node).kind {
                    let mut pipe = pipe.borrow_mut();
                    pipe.refcnt = pipe.refcnt.saturating_sub(1);
                    if pipe.refcnt == 0 {
                        pipe.buckets.clear();
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn has_llseek(ops: StreamOps) -> bool {
        matches!(
            ops,
            StreamOps::MemDir
                | StreamOps::MemFile
                | StreamOps::Chrdev
                | StreamOps::Device(Device::Null | Device::Blob)
                | StreamOps::Host
                | StreamOps::Proxy
                | StreamOps::ProcFd
        )
    }

    /// `FS.llseek`.
    pub fn llseek(&mut self, fd: i32, offset: i64, whence: i32) -> FsResult<i64> {
        let stream = self.get_stream_checked(fd)?;
        if !stream.seekable || !Self::has_llseek(stream.ops) {
            return errno(ESPIPE);
        }
        if whence != 0 && whence != 1 && whence != 2 {
            return errno(EINVAL);
        }
        let ops = stream.ops;
        let node = stream.node;
        let position = stream.position();
        let mut target = offset;
        match ops {
            StreamOps::Chrdev => return errno(ESPIPE),
            StreamOps::Device(Device::Null) => target = 0,
            StreamOps::Device(Device::Blob) => {
                let blob = self.blob.as_ref().ok_or_else(|| {
                    FsError::Fatal("No /dev/blob File or Blob provided to llseek".into())
                })?;
                if whence == 1 {
                    target += position;
                } else if whence == 2 {
                    target = i64::try_from(blob.len()).unwrap_or(0);
                }
                if target < 0 {
                    return errno(EINVAL);
                }
            }
            StreamOps::Host => {
                if whence == 1 {
                    target += position;
                } else if whence == 2 && is_file(self.node(node).mode) {
                    let stream = self.get_stream_checked(fd)?;
                    let file = stream.shared.borrow().host.clone();
                    if let Some(file) = file {
                        let size = file.metadata().map_err(|error| host_error(&error))?.len();
                        target += i64::try_from(size).unwrap_or(i64::MAX);
                    }
                }
                if target < 0 {
                    return errno(EINVAL);
                }
            }
            StreamOps::Proxy => {
                if whence == 1 {
                    target += position;
                } else if whence == 2 && is_file(self.node(node).mode) {
                    target += self.getattr(node)?.size;
                }
                if target < 0 {
                    return errno(EINVAL);
                }
            }
            _ => {
                if whence == 1 {
                    target += position;
                } else if whence == 2
                    && is_file(self.node(node).mode)
                    && let NodeKind::MemFile(data) = &self.node(node).kind
                {
                    target += i64::try_from(data.len()).unwrap_or(0);
                }
                if target < 0 {
                    return errno(EINVAL);
                }
            }
        }
        let stream = self.get_stream_checked(fd)?;
        stream.set_position(target);
        Ok(target)
    }

    /// `FS.read` into `buffer`. `position` is `None` for the stream position.
    pub fn read(&mut self, fd: i32, buffer: &mut [u8], position: Option<i64>) -> FsResult<usize> {
        if position.is_some_and(|position| position < 0) {
            return errno(EINVAL);
        }
        let stream = self.get_stream_checked(fd)?;
        if stream.flags() & O_ACCMODE == O_WRONLY {
            return errno(EBADF);
        }
        let node = stream.node;
        if is_dir(self.node(node).mode) {
            return errno(EISDIR);
        }
        let ops = stream.ops;
        if !Self::has_read(ops) {
            return errno(EINVAL);
        }
        let explicit = position.is_some();
        let offset = match position {
            Some(position) => {
                if !stream.seekable {
                    return errno(ESPIPE);
                }
                position
            }
            None => stream.position(),
        };
        let count = self.stream_read(fd, ops, node, buffer, offset)?;
        if !explicit {
            let stream = self.get_stream_checked(fd)?;
            stream.set_position(stream.position() + i64::try_from(count).unwrap_or(0));
        }
        Ok(count)
    }

    fn has_read(ops: StreamOps) -> bool {
        matches!(
            ops,
            StreamOps::MemFile
                | StreamOps::Device(_)
                | StreamOps::Host
                | StreamOps::Proxy
                | StreamOps::Pipe
                | StreamOps::Socket
        )
    }

    fn has_write(ops: StreamOps) -> bool {
        Self::has_read(ops)
    }

    fn stream_read(
        &mut self,
        fd: i32,
        ops: StreamOps,
        node: NodeId,
        buffer: &mut [u8],
        offset: i64,
    ) -> FsResult<usize> {
        let length = buffer.len();
        match ops {
            StreamOps::MemFile => {
                let NodeKind::MemFile(data) = &self.node(node).kind else {
                    return Ok(0);
                };
                let offset = usize::try_from(offset).unwrap_or(usize::MAX);
                if offset >= data.len() {
                    return Ok(0);
                }
                let count = (data.len() - offset).min(length);
                buffer[..count].copy_from_slice(&data[offset..offset + count]);
                Ok(count)
            }
            StreamOps::Host => {
                if length == 0 {
                    return Ok(0);
                }
                let file = self.get_stream_checked(fd)?.shared.borrow().host.clone();
                let Some(file) = file else {
                    return errno(EBADF);
                };
                file.read_at(buffer, u64::try_from(offset).unwrap_or(0))
                    .map_err(|error| host_error(&error))
            }
            StreamOps::Proxy => {
                let target = self.proxy_target(node).ok_or(FsError::Errno(EINVAL))?;
                let target_fd = self.get_stream_checked(fd)?.proxy_fd.unwrap_or(-1);
                let count = target.borrow_mut().read(target_fd, buffer, Some(offset))?;
                Ok(count)
            }
            StreamOps::Device(device) => self.device_read(fd, device, node, buffer, offset),
            StreamOps::Pipe => {
                let NodeKind::Pipe(pipe) = &self.node(node).kind else {
                    return Ok(0);
                };
                pipe_read(&mut pipe.borrow_mut(), buffer)
            }
            StreamOps::Socket => errno(ENOTCONN),
            _ => errno(EINVAL),
        }
    }

    fn device_read(
        &mut self,
        fd: i32,
        device: Device,
        node: NodeId,
        buffer: &mut [u8],
        offset: i64,
    ) -> FsResult<usize> {
        match device {
            Device::Null | Device::InputNull => Ok(0),
            Device::Blob => {
                let blob = self.blob.as_ref().ok_or_else(|| {
                    FsError::Fatal("No /dev/blob File or Blob provided to read from".into())
                })?;
                let offset = usize::try_from(offset).unwrap_or(usize::MAX);
                if offset >= blob.len() {
                    return Ok(0);
                }
                let count = (blob.len() - offset).min(buffer.len());
                buffer[..count].copy_from_slice(&blob[offset..offset + count]);
                Ok(count)
            }
            Device::Random => {
                let mut count = 0;
                for slot in buffer.iter_mut() {
                    if self.random_pool.is_empty() {
                        let mut pool = vec![0_u8; 1024];
                        getrandom::fill(&mut pool)
                            .map_err(|error| FsError::Fatal(error.to_string()))?;
                        self.random_pool = pool;
                    }
                    *slot = self.random_pool.pop().unwrap_or(0);
                    count += 1;
                }
                if count > 0 {
                    self.node_mut(node).atime = date_now();
                }
                Ok(count)
            }
            Device::Tty(_) => {
                let tty = self.get_stream_checked(fd)?.tty;
                if tty.is_none() {
                    return errno(ENXIO);
                }
                // default_tty_ops.get_char reads the Node process stdin; the
                // PGlite modules replace stdin, so the TTY is never read.
                // tty1 has no get_char at all.
                errno(ENXIO)
            }
        }
    }

    /// `FS.write` from `buffer`.
    pub fn write(&mut self, fd: i32, buffer: &[u8], position: Option<i64>) -> FsResult<usize> {
        if position.is_some_and(|position| position < 0) {
            return errno(EINVAL);
        }
        let stream = self.get_stream_checked(fd)?;
        if stream.flags() & O_ACCMODE == 0 {
            return errno(EBADF);
        }
        let node = stream.node;
        if is_dir(self.node(node).mode) {
            return errno(EISDIR);
        }
        let ops = stream.ops;
        if !Self::has_write(ops) {
            return errno(EINVAL);
        }
        if stream.seekable && stream.flags() & O_APPEND != 0 {
            self.llseek(fd, 0, 2)?;
        }
        let stream = self.get_stream_checked(fd)?;
        let explicit = position.is_some();
        let offset = match position {
            Some(position) => {
                if !stream.seekable {
                    return errno(ESPIPE);
                }
                position
            }
            None => stream.position(),
        };
        let count = self.stream_write(fd, ops, node, buffer, offset)?;
        if !explicit {
            let stream = self.get_stream_checked(fd)?;
            stream.set_position(stream.position() + i64::try_from(count).unwrap_or(0));
        }
        Ok(count)
    }

    fn stream_write(
        &mut self,
        fd: i32,
        ops: StreamOps,
        node: NodeId,
        buffer: &[u8],
        offset: i64,
    ) -> FsResult<usize> {
        match ops {
            StreamOps::MemFile => {
                if buffer.is_empty() {
                    return Ok(0);
                }
                let now = date_now();
                let node = self.node_mut(node);
                node.mtime = now;
                node.ctime = now;
                if let NodeKind::MemFile(data) = &mut node.kind {
                    let offset = usize::try_from(offset).unwrap_or(0);
                    let end = offset + buffer.len();
                    if data.len() < end {
                        data.resize(end, 0);
                    }
                    data[offset..end].copy_from_slice(buffer);
                }
                Ok(buffer.len())
            }
            StreamOps::Host => {
                let file = self.get_stream_checked(fd)?.shared.borrow().host.clone();
                let Some(file) = file else {
                    return errno(EBADF);
                };
                file.write_at(buffer, u64::try_from(offset).unwrap_or(0))
                    .map_err(|error| host_error(&error))
            }
            StreamOps::Proxy => {
                let target = self.proxy_target(node).ok_or(FsError::Errno(EINVAL))?;
                let target_fd = self.get_stream_checked(fd)?.proxy_fd.unwrap_or(-1);
                let count = target.borrow_mut().write(target_fd, buffer, Some(offset))?;
                Ok(count)
            }
            StreamOps::Device(device) => self.device_write(fd, device, node, buffer),
            StreamOps::Pipe => {
                let NodeKind::Pipe(pipe) = &self.node(node).kind else {
                    return Ok(0);
                };
                Ok(pipe_write(&mut pipe.borrow_mut(), buffer))
            }
            StreamOps::Socket => errno(ENOTCONN),
            _ => errno(EINVAL),
        }
    }

    fn device_write(
        &mut self,
        fd: i32,
        device: Device,
        node: NodeId,
        buffer: &[u8],
    ) -> FsResult<usize> {
        match device {
            Device::Null => Ok(buffer.len()),
            Device::Blob => {
                self.blob_written.push(buffer.to_vec());
                Ok(buffer.len())
            }
            Device::Random | Device::InputNull => {
                // createDevice without an output callback calls `a(...)` on
                // undefined, which the device catches as EIO.
                if buffer.is_empty() { Ok(0) } else { errno(EIO) }
            }
            Device::Tty(_) => {
                let tty = self.get_stream_checked(fd)?.tty;
                let Some(tty) = tty else {
                    return errno(ENXIO);
                };
                let tty = u32::try_from(tty).unwrap_or(0);
                for byte in buffer {
                    self.tty_put_char(tty, Some(*byte));
                }
                if !buffer.is_empty() {
                    let now = date_now();
                    let node = self.node_mut(node);
                    node.mtime = now;
                    node.ctime = now;
                }
                Ok(buffer.len())
            }
        }
    }

    fn tty_put_char(&mut self, tty: u32, byte: Option<u8>) {
        let Some(state) = self.ttys.get_mut(&tty) else {
            return;
        };
        match byte {
            None | Some(10) => {
                let text = utf8_array_to_string(&state.output);
                state.output.clear();
                self.console.push(state.sink, text);
            }
            Some(0) => {}
            Some(byte) => state.output.push(byte),
        }
    }

    pub fn tty_flush(&mut self, tty: u32) {
        let Some(state) = self.ttys.get_mut(&tty) else {
            return;
        };
        if !state.output.is_empty() {
            let text = utf8_array_to_string(&state.output);
            state.output.clear();
            self.console.push(state.sink, text);
        }
    }

    /// `FS.allocate`.
    pub fn allocate(&mut self, fd: i32, offset: i64, length: i64) -> FsResult<()> {
        let stream = self.get_stream_checked(fd)?;
        if offset < 0 || length <= 0 {
            return errno(EINVAL);
        }
        if stream.flags() & O_ACCMODE == 0 {
            return errno(EBADF);
        }
        let node = stream.node;
        let mode = self.node(node).mode;
        if !is_file(mode) && !is_dir(mode) {
            return errno(ENODEV);
        }
        if stream.ops != StreamOps::MemFile {
            return errno(EOPNOTSUPP);
        }
        if let NodeKind::MemFile(data) = &mut self.node_mut(node).kind {
            let end = usize::try_from(offset + length).unwrap_or(0);
            if data.len() < end {
                data.resize(end, 0);
            }
        }
        Ok(())
    }

    pub fn fd_sync(&mut self, fd: i32) -> FsResult<i32> {
        let stream = self.get_stream_checked(fd)?;
        match stream.ops {
            StreamOps::Device(Device::Tty(_)) => {
                let tty = stream.tty.unwrap_or(0);
                self.tty_flush(u32::try_from(tty).unwrap_or(0));
                Ok(0)
            }
            StreamOps::Pipe => Ok(EINVAL),
            _ => Ok(0),
        }
    }

    /// The node at a stream, for mmap and getdents.
    pub fn stream_node(&self, fd: i32) -> FsResult<NodeId> {
        Ok(self.get_stream_checked(fd)?.node)
    }

    /// Bytes for `mmap`: reads `length` bytes from `offset` through the
    /// filesystem the way `stream_ops.mmap` does.
    pub fn mmap_read(&mut self, fd: i32, length: usize, offset: i64) -> FsResult<Vec<u8>> {
        let stream = self.get_stream_checked(fd)?;
        let ops = stream.ops;
        let node = stream.node;
        match ops {
            StreamOps::MemFile => {
                let NodeKind::MemFile(data) = &self.node(node).kind else {
                    return Ok(vec![0; length]);
                };
                let mut output = vec![0_u8; length];
                let start = usize::try_from(offset).unwrap_or(0).min(data.len());
                let end = (start + length).min(data.len());
                output[..end - start].copy_from_slice(&data[start..end]);
                Ok(output)
            }
            StreamOps::Host => {
                if !is_file(self.node(node).mode) {
                    return errno(ENODEV);
                }
                let mut output = vec![0_u8; length];
                if length > 0 {
                    let count = self.stream_read(fd, ops, node, &mut output, offset)?;
                    let _ = count;
                }
                Ok(output)
            }
            _ => errno(ENODEV),
        }
    }

    pub fn has_mmap(&self, fd: i32) -> FsResult<bool> {
        Ok(matches!(
            self.get_stream_checked(fd)?.ops,
            StreamOps::MemFile | StreamOps::Host
        ))
    }

    /// `FS.msync` for a writable shared mapping.
    pub fn msync(&mut self, fd: i32, bytes: &[u8], offset: i64) -> FsResult<()> {
        let stream = self.get_stream_checked(fd)?;
        let ops = stream.ops;
        let node = stream.node;
        match ops {
            StreamOps::MemFile | StreamOps::Host => {
                self.stream_write(fd, ops, node, bytes, offset)?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    // ----- setup helpers matching FS.staticInit and FS.init ----------------

    pub fn register_device(&mut self, dev: u32, device: Device) {
        self.devices.insert(dev, device);
    }

    /// `FS.createDevice`.
    pub fn create_device(
        &mut self,
        parent: &str,
        name: &str,
        device: Device,
        readable: bool,
        writable: bool,
    ) -> FsResult<NodeId> {
        let path = path_join2(parent, name);
        let mut mode = 0;
        if readable {
            mode |= 0o555;
        }
        if writable {
            mode |= 0o222;
        }
        let dev = makedev(self.create_device_major, 0);
        self.create_device_major += 1;
        self.register_device(dev, device);
        self.mkdev(&path, Some(mode), dev)
    }

    /// `FS.staticInit` plus the default directories, devices and `/proc`.
    pub fn static_init(&mut self) -> FsResult<()> {
        self.mount(&MountKind::Mem, Some("/"))?;
        self.mkdir("/tmp", 511)?;
        self.mkdir("/home", 511)?;
        self.mkdir("/home/web_user", 511)?;
        self.mkdir("/dev", 511)?;
        self.register_device(makedev(1, 3), Device::Null);
        self.mkdev("/dev/null", None, makedev(1, 3))?;
        self.ttys.insert(
            makedev(5, 0),
            Tty {
                output: Vec::new(),
                sink: 0,
            },
        );
        self.register_device(makedev(5, 0), Device::Tty(0));
        self.ttys.insert(
            makedev(6, 0),
            Tty {
                output: Vec::new(),
                sink: 1,
            },
        );
        self.register_device(makedev(6, 0), Device::Tty(1));
        self.mkdev("/dev/tty", None, makedev(5, 0))?;
        self.mkdev("/dev/tty1", None, makedev(6, 0))?;
        self.create_device("/dev", "random", Device::Random, true, false)?;
        self.create_device("/dev", "urandom", Device::Random, true, false)?;
        self.mkdir("/dev/shm", 511)?;
        self.mkdir("/dev/shm/tmp", 511)?;
        self.mkdir("/proc", 511)?;
        self.mkdir("/proc/self", 511)?;
        self.mkdir("/proc/self/fd", 511)?;
        self.mount(&MountKind::ProcFd, Some("/proc/self/fd"))?;
        Ok(())
    }

    /// `FS.init` with a null-returning stdin callback and default stdout and
    /// stderr, as both `PGlite` modules configure it.
    pub fn init_standard_streams(&mut self) -> FsResult<()> {
        self.initialized = true;
        self.create_device("/dev", "stdin", Device::InputNull, true, false)?;
        self.symlink("/dev/tty", "/dev/stdout")?;
        self.symlink("/dev/tty1", "/dev/stderr")?;
        self.open("/dev/stdin", 0, None)?;
        self.open("/dev/stdout", 1, None)?;
        self.open("/dev/stderr", 1, None)?;
        Ok(())
    }

    /// `initRuntime` mounts after FS.init.
    pub fn init_runtime_mounts(&mut self) -> FsResult<()> {
        self.ignore_permissions = false;
        self.sock_root = Some(self.mount(&MountKind::Sock, None)?);
        self.pipe_root = Some(self.mount(&MountKind::Pipe, None)?);
        Ok(())
    }

    /// `FS.quit` without the libc flush, which the caller performs first.
    pub fn quit_streams(&mut self) {
        self.initialized = false;
        for fd in 0..self.streams.len() {
            if self.streams[fd].is_some() {
                let _ = self.close(i32::try_from(fd).unwrap_or(0));
            }
        }
    }

    /// `FS.writeFile` with flags 577 unless given.
    pub fn write_file(&mut self, path: &str, data: &[u8], flags: i32, mode: u32) -> FsResult<()> {
        let fd = self.open(path, flags, Some(mode))?;
        let result = self.write(fd, data, None);
        self.close(fd)?;
        result.map(|_| ())
    }

    /// `FS.readFile` in binary mode.
    pub fn read_file(&mut self, path: &str) -> FsResult<Vec<u8>> {
        let fd = self.open(path, 0, None)?;
        let size = usize::try_from(self.stat(path, false)?.size).unwrap_or(0);
        let mut data = vec![0_u8; size];
        let result = self.read(fd, &mut data, Some(0));
        self.close(fd)?;
        result?;
        Ok(data)
    }

    /// `FS.createPath(parent, path, true, true)`.
    pub fn create_path(&mut self, parent: &str, path: &str) {
        let mut current = parent.to_owned();
        for part in path.split('/') {
            if part.is_empty() {
                continue;
            }
            let next = path_join2(&current, part);
            let _ = self.mkdir(&next, 511);
            current = next;
        }
    }

    /// `FS.createDataFile(path, null, data, true, true, true)`.
    pub fn create_data_file(&mut self, path: &str, data: &[u8]) -> FsResult<()> {
        let mode = 0o555 | 0o222;
        let node = self.create(path, mode)?;
        self.chmod_node(node, mode | 0o222)?;
        let fd = self.open_node(node, 577)?;
        self.write(fd, data, Some(0))?;
        self.close(fd)?;
        self.chmod_node(node, mode)?;
        Ok(())
    }

    fn open_node(&mut self, node: NodeId, flags: i32) -> FsResult<i32> {
        let mut flags = flags;
        let error = self.may_open(Some(node), flags);
        if error != 0 {
            return errno(error);
        }
        if flags & O_TRUNC != 0 {
            self.truncate_node(node, 0)?;
        }
        flags &= !(O_CREAT | O_EXCL | O_TRUNC | O_NOFOLLOW);
        let stream = Stream {
            node,
            path: self.get_path(node),
            shared: Rc::new(RefCell::new(StreamShared {
                flags,
                position: 0,
                refcount: 0,
                host: None,
            })),
            seekable: true,
            ops: self.stream_ops_for(node),
            tty: None,
            getdents: None,
            proxy_fd: None,
            fd: None,
        };
        let fd = self.create_stream(stream, None)?;
        self.stream_open(fd)?;
        Ok(fd)
    }

    /// `FS.analyzePath(path).exists`: true whenever `lookupPath` does not
    /// throw, which includes the empty path.
    pub fn exists(&mut self, path: &str) -> bool {
        self.lookup_follow(path, true).is_ok()
    }

    /// `PIPEFS.createPipe`.
    pub fn create_pipe(&mut self) -> FsResult<(i32, i32)> {
        let root = self.pipe_root.ok_or(FsError::Errno(EINVAL))?;
        let pipe = Rc::new(RefCell::new(Pipe {
            buckets: vec![PipeBucket {
                buffer: vec![0; PIPE_BUCKET],
                offset: 0,
                roffset: 0,
            }],
            refcnt: 2,
        }));
        let read_name = format!("pipe[{}]", self.pipe_names);
        self.pipe_names += 1;
        let write_name = format!("pipe[{}]", self.pipe_names);
        self.pipe_names += 1;
        let read_node = self.create_node(
            Some(root),
            &read_name,
            S_IFIFO,
            0,
            NodeKind::Pipe(Rc::clone(&pipe)),
            None,
        );
        let write_node = self.create_node(
            Some(root),
            &write_name,
            S_IFIFO,
            0,
            NodeKind::Pipe(pipe),
            None,
        );
        let read_fd = self.create_stream(
            Stream {
                node: read_node,
                path: read_name,
                shared: Rc::new(RefCell::new(StreamShared {
                    flags: 0,
                    position: 0,
                    refcount: 0,
                    host: None,
                })),
                seekable: false,
                ops: StreamOps::Pipe,
                tty: None,
                getdents: None,
                proxy_fd: None,
                fd: None,
            },
            None,
        )?;
        let write_fd = self.create_stream(
            Stream {
                node: write_node,
                path: write_name,
                shared: Rc::new(RefCell::new(StreamShared {
                    flags: 1,
                    position: 0,
                    refcount: 0,
                    host: None,
                })),
                seekable: false,
                ops: StreamOps::Pipe,
                tty: None,
                getdents: None,
                proxy_fd: None,
                fd: None,
            },
            None,
        )?;
        Ok((read_fd, write_fd))
    }

    /// `SOCKFS.createSocket` up to the stream; no peer is ever created.
    pub fn create_socket(&mut self, kind: i32, protocol: i32) -> FsResult<i32> {
        let kind = kind & !0x8_0801;
        let stream_socket = kind == 1;
        if stream_socket && protocol != 0 && protocol != 6 {
            return errno(EPROTONOSUPPORT);
        }
        let root = self.sock_root.ok_or(FsError::Errno(EINVAL))?;
        let name = format!("socket[{}]", self.socket_names);
        self.socket_names += 1;
        let node = self.create_node(Some(root), &name, S_IFSOCK, 0, NodeKind::Socket, None);
        self.create_stream(
            Stream {
                node,
                path: name,
                shared: Rc::new(RefCell::new(StreamShared {
                    flags: 2,
                    position: 0,
                    refcount: 0,
                    host: None,
                })),
                seekable: false,
                ops: StreamOps::Socket,
                tty: None,
                getdents: None,
                proxy_fd: None,
                fd: None,
            },
            None,
        )
    }

    #[must_use]
    pub fn is_socket_fd(&self, fd: i32) -> bool {
        self.get_stream(fd)
            .is_some_and(|stream| is_socket(self.node(stream.node).mode))
    }

    /// `getdents64` entry: inode and `d_type` for a name in the open
    /// directory, `None` when the glue skips it (EINVAL from lookup).
    pub fn dirent(&mut self, fd: i32, name: &str) -> FsResult<Option<(u64, u8)>> {
        let stream = self.get_stream_checked(fd)?;
        let node = stream.node;
        let path = stream.path.clone();
        if name == "." {
            return Ok(Some((u64::from(self.node(node).id), 4)));
        }
        if name == ".." {
            let parent = self
                .lookup_parent(&path)?
                .node
                .ok_or(FsError::Errno(ENOENT))?;
            return Ok(Some((u64::from(self.node(parent).id), 4)));
        }
        match self.lookup_node(node, name) {
            Ok(child) => {
                let mode = self.node(child).mode;
                let kind = if is_chrdev(mode) {
                    2
                } else if is_dir(mode) {
                    4
                } else if is_link(mode) {
                    10
                } else {
                    8
                };
                Ok(Some((u64::from(self.node(child).id), kind)))
            }
            Err(FsError::Errno(EINVAL)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// `faccessat` permission check on a resolved path.
    pub fn access(&mut self, path: &str, amode: i32) -> FsResult<i32> {
        if amode & !7 != 0 {
            return Ok(-EINVAL);
        }
        let node = self.lookup_follow(path, true)?.node;
        let Some(node) = node else {
            return Ok(-ENOENT);
        };
        let mut permission = String::new();
        if amode & 4 != 0 {
            permission.push('r');
        }
        if amode & 2 != 0 {
            permission.push('w');
        }
        if amode & 1 != 0 {
            permission.push('x');
        }
        if !permission.is_empty() && self.node_permissions(node, &permission) != 0 {
            Ok(-EACCES)
        } else {
            Ok(0)
        }
    }

    /// `node_ops.readdir` of every directory below `path` in the order the
    /// `PGlite` tar dump walks it: name, mode, size, mtime and file bytes.
    pub fn walk(&mut self, path: &str) -> FsResult<Vec<WalkEntry>> {
        let mut entries = Vec::new();
        self.walk_into(path, path, &mut entries)?;
        Ok(entries)
    }

    fn walk_into(&mut self, base: &str, directory: &str, out: &mut Vec<WalkEntry>) -> FsResult<()> {
        for name in self.readdir(directory)? {
            if name == "." || name == ".." {
                continue;
            }
            let full = format!("{directory}/{name}");
            let stat = self.stat(&full, false)?;
            let data = if is_file(stat.mode) {
                self.read_file(&full)?
            } else {
                Vec::new()
            };
            let directory_entry = is_dir(stat.mode);
            out.push(WalkEntry {
                name: full[base.len()..].to_owned(),
                mtime: stat.mtime,
                is_file: is_file(stat.mode),
                data,
            });
            if directory_entry {
                self.walk_into(base, &full, out)?;
            }
        }
        Ok(())
    }
}

impl Mount {
    fn kind_is_special(&self) -> bool {
        matches!(self.kind, MountKind::Sock | MountKind::Pipe)
    }
}

pub struct WalkEntry {
    pub name: String,
    pub mtime: f64,
    pub is_file: bool,
    pub data: Vec<u8>,
}

fn unreachable_node(id: NodeId) -> ! {
    panic!("virtual filesystem node {id} was released")
}

fn pipe_read(pipe: &mut Pipe, buffer: &mut [u8]) -> FsResult<usize> {
    let available: usize = pipe
        .buckets
        .iter()
        .map(|bucket| bucket.offset - bucket.roffset)
        .sum();
    if buffer.is_empty() {
        return Ok(0);
    }
    if available == 0 {
        return errno(EAGAIN);
    }
    let mut remaining = available.min(buffer.len());
    let total = remaining;
    let mut written = 0;
    let mut consumed = 0;
    for bucket in &mut pipe.buckets {
        let size = bucket.offset - bucket.roffset;
        if remaining <= size {
            buffer[written..written + remaining]
                .copy_from_slice(&bucket.buffer[bucket.roffset..bucket.roffset + remaining]);
            if remaining < size {
                bucket.roffset += remaining;
            } else {
                consumed += 1;
            }
            break;
        }
        buffer[written..written + size]
            .copy_from_slice(&bucket.buffer[bucket.roffset..bucket.offset]);
        written += size;
        remaining -= size;
        consumed += 1;
    }
    if consumed > 0 && consumed == pipe.buckets.len() {
        consumed -= 1;
        let last = &mut pipe.buckets[consumed];
        last.offset = 0;
        last.roffset = 0;
    }
    pipe.buckets.drain(0..consumed);
    Ok(total)
}

fn pipe_write(pipe: &mut Pipe, data: &[u8]) -> usize {
    let total = data.len();
    if total == 0 {
        return 0;
    }
    if pipe.buckets.is_empty() {
        pipe.buckets.push(PipeBucket {
            buffer: vec![0; PIPE_BUCKET],
            offset: 0,
            roffset: 0,
        });
    }
    let mut data = data;
    let last = pipe.buckets.len() - 1;
    let free = PIPE_BUCKET - pipe.buckets[last].offset;
    if free >= data.len() {
        let bucket = &mut pipe.buckets[last];
        bucket.buffer[bucket.offset..bucket.offset + data.len()].copy_from_slice(data);
        bucket.offset += data.len();
        return total;
    }
    if free > 0 {
        let bucket = &mut pipe.buckets[last];
        bucket.buffer[bucket.offset..].copy_from_slice(&data[..free]);
        bucket.offset += free;
        data = &data[free..];
    }
    while data.len() >= PIPE_BUCKET {
        let mut buffer = vec![0; PIPE_BUCKET];
        buffer.copy_from_slice(&data[..PIPE_BUCKET]);
        pipe.buckets.push(PipeBucket {
            buffer,
            offset: PIPE_BUCKET,
            roffset: 0,
        });
        data = &data[PIPE_BUCKET..];
    }
    if !data.is_empty() {
        let mut buffer = vec![0; PIPE_BUCKET];
        buffer[..data.len()].copy_from_slice(data);
        pipe.buckets.push(PipeBucket {
            buffer,
            offset: data.len(),
            roffset: 0,
        });
    }
    total
}

/// `UTF8ArrayToString` with the decoder's replacement behavior.
#[must_use]
pub fn utf8_array_to_string(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Milliseconds of a host timestamp truncated the way a Node `Stats` `Date`
/// is (`new Date(atimeMs)` keeps whole milliseconds).
fn host_time_ms(seconds: i64, nanoseconds: i64) -> f64 {
    let total_ms = i128::from(seconds) * 1000 + i128::from(nanoseconds) / 1_000_000;
    // Precision: timestamps are within +/- 2^53 milliseconds.
    #[allow(
        clippy::cast_precision_loss,
        reason = "Node keeps stat times as Number milliseconds; host timestamps are within 2^53 ms"
    )]
    let value = total_ms as f64;
    value
}

fn stat_from_metadata(metadata: &host::Metadata) -> Stat {
    Stat {
        dev: i64::try_from(metadata.dev()).unwrap_or(0),
        ino: metadata.ino(),
        mode: metadata.mode(),
        nlink: u32::try_from(metadata.nlink()).unwrap_or(u32::MAX),
        uid: i64::from(metadata.uid()),
        gid: i64::from(metadata.gid()),
        rdev: i64::try_from(metadata.rdev()).unwrap_or(0),
        size: i64::try_from(metadata.size()).unwrap_or(i64::MAX),
        atime: host_time_ms(metadata.atime(), metadata.atime_nsec()),
        mtime: host_time_ms(metadata.mtime(), metadata.mtime_nsec()),
        ctime: host_time_ms(metadata.ctime(), metadata.ctime_nsec()),
        blocks: i64::try_from(metadata.blocks()).unwrap_or(0),
    }
}

fn host_lstat(path: &str) -> FsResult<Stat> {
    host::symlink_metadata(path)
        .map(|metadata| stat_from_metadata(&metadata))
        .map_err(|error| host_error(&error))
}

/// Node `fs.readdirSync`: libuv `scandir` sorted by byte order, without `.`
/// and `..`.
fn host_readdir(path: &str) -> FsResult<Vec<String>> {
    let mut names = Vec::new();
    for entry in host::read_dir(path).map_err(|error| host_error(&error))? {
        let entry = entry.map_err(|error| host_error(&error))?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    Ok(names)
}

/// `NODEFS.flagsForNode` then Node `fs.openSync(path, flags)`.
fn host_open(path: &str, flags: i32) -> FsResult<host::File> {
    let mut remaining = flags;
    remaining &= !2_097_152;
    remaining &= !2048;
    remaining &= !32768;
    remaining &= !524_288;
    remaining &= !65536;
    let mut options = host::OpenOptions::new();
    let mut custom = 0;
    let access = remaining & 3;
    remaining &= !3;
    match access {
        0 => {
            options.read(true);
        }
        1 => {
            options.write(true);
        }
        2 => {
            options.read(true).write(true);
        }
        _ => return errno(EINVAL),
    }
    let map: &[(i32, i32)] = &[
        (1024, libc::O_APPEND),
        (64, libc::O_CREAT),
        (128, libc::O_EXCL),
        (256, libc::O_NOCTTY),
        (4096, libc::O_SYNC),
        (512, libc::O_TRUNC),
        (131_072, libc::O_NOFOLLOW),
    ];
    for (emscripten, native) in map {
        if remaining & emscripten != 0 {
            custom |= native;
            remaining ^= emscripten;
        }
    }
    if remaining != 0 {
        return errno(EINVAL);
    }
    if custom & libc::O_APPEND != 0 {
        options.append(true);
        custom &= !libc::O_APPEND;
    }
    if custom & libc::O_CREAT != 0 {
        if access == 0 {
            options.write(true);
        }
        options.create(true);
        custom &= !libc::O_CREAT;
    }
    if custom & libc::O_EXCL != 0 {
        options.create_new(true);
        custom &= !libc::O_EXCL;
    }
    if custom & libc::O_TRUNC != 0 {
        options.truncate(true);
        custom &= !libc::O_TRUNC;
    }
    options.custom_flags(custom).mode(0o666);
    options.open(path).map_err(|error| host_error(&error))
}

/// Node `fs.utimesSync(path, atimeDate, mtimeDate)` with millisecond values.
fn set_host_times(path: &str, atime: Option<f64>, mtime: Option<f64>) -> FsResult<()> {
    let file = host::OpenOptions::new()
        .read(true)
        .open(path)
        .or_else(|_| host::OpenOptions::new().write(true).open(path))
        .map_err(|error| host_error(&error))?;
    let to_time = |value: f64| -> SystemTime {
        if value >= 0.0 {
            UNIX_EPOCH + Duration::from_secs_f64(value / 1000.0)
        } else {
            UNIX_EPOCH - Duration::from_secs_f64(-value / 1000.0)
        }
    };
    let metadata = file.metadata().map_err(|error| host_error(&error))?;
    let current_access = metadata.accessed().unwrap_or(UNIX_EPOCH);
    let current_modify = metadata.modified().unwrap_or(UNIX_EPOCH);
    let times = host::FileTimes::new()
        .set_accessed(atime.map_or(current_access, to_time))
        .set_modified(mtime.map_or(current_modify, to_time));
    file.set_times(times).map_err(|error| host_error(&error))
}

/// Node `fs.statfsSync`: bsize, blocks, bfree, bavail, files, ffree.
fn host_statfs(_root: &std::path::Path) -> Option<[i64; 6]> {
    // Node's statfs needs the statfs(2) system call, which has no safe
    // standard-library wrapper. PostgreSQL does not call statfs in the
    // workloads covered here (0 calls in the import trace), so the host keeps
    // the Emscripten defaults and records this as an untested difference.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discarding_console_keeps_no_lines() {
        let mut fs = Fs::new();
        fs.ttys.insert(
            7,
            Tty {
                output: Vec::new(),
                sink: 1,
            },
        );
        for byte in b"LOG: one\n" {
            fs.tty_put_char(7, Some(*byte));
        }
        assert_eq!(fs.console.stderr, ["LOG: one"]);
        fs.console.discard = true;
        for byte in b"LOG: two\n" {
            fs.tty_put_char(7, Some(*byte));
        }
        assert_eq!(fs.console.stderr, ["LOG: one"]);
    }
}
