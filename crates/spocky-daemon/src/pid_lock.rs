//! `paseo.pid`: the exclusive, heartbeated lock a running daemon holds on its
//! home, and the record other tools read to find it.
//!
//! Source at Paseo `5de45e2`: `pid-lock.ts`.
//!
//! The file is JSON. Its key order is observable, so it is written by hand:
//! the first write uses the object literal order
//! (`pid, startedAt, hostname, uid, listen, heartbeat, desktopManaged`), and
//! every update rewrites it in zod schema order
//! (`pid, startedAt, hostname, uid, listen, serverId, desktopManaged,
//! heartbeat`) with a `serverId` that was not there yet appended last.

use std::fmt::{self, Write as _};
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use rustix::process::{Pid, test_kill_process};
use serde_json::Value;

use crate::iso_time::{now_ms, parse_iso, to_iso_string};
use crate::private_files::ensure_private_directory;

pub const PID_LOCK_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const PID_LOCK_READ_RETRY_ATTEMPTS: u32 = 10;
const PID_LOCK_READ_RETRY_DELAY: Duration = Duration::from_millis(50);
/// `uptime()` is whole seconds on some platforms, so the derived boot instant
/// carries about a second of error. This covers that and nothing more.
const BOOT_INSTANT_TOLERANCE_MS: f64 = 5_000.0;

/// `PidLockInfo` (`pidLockInfoSchema`).
#[derive(Debug, Clone, PartialEq)]
pub struct PidLockInfo {
    pub pid: i64,
    pub started_at: String,
    pub hostname: String,
    pub uid: serde_json::Number,
    pub listen: Option<String>,
    /// `None` is a missing key, `Some(None)` is `null`.
    pub server_id: Option<Option<String>>,
    pub desktop_managed: Option<bool>,
    /// `heartbeat: true`; `false` is a missing key.
    pub heartbeat: bool,
}

/// `PidLockError`, plus the I/O errors the baseline lets propagate.
#[derive(Debug)]
pub enum PidLockError {
    Lock {
        message: String,
        existing: Option<Box<PidLockInfo>>,
        /// `DAEMON_STATE_READ_FAILED` when the file never became readable.
        code: Option<&'static str>,
    },
    Io(io::Error),
}

impl PidLockError {
    fn lock(message: impl Into<String>) -> Self {
        Self::Lock {
            message: message.into(),
            existing: None,
            code: None,
        }
    }

    fn held(message: impl Into<String>, existing: PidLockInfo) -> Self {
        Self::Lock {
            message: message.into(),
            existing: Some(Box::new(existing)),
            code: None,
        }
    }
}

impl fmt::Display for PidLockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lock { message, .. } => f.write_str(message),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for PidLockError {}

impl From<io::Error> for PidLockError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// `parsePidLockInfo`: zod `safeParse` of the file content.
fn parse_pid_lock_info(raw: &Value) -> Option<PidLockInfo> {
    let object = raw.as_object()?;
    let pid_number = object.get("pid")?.as_f64()?;
    #[allow(clippy::cast_possible_truncation)]
    let pid = (pid_number.fract() == 0.0 && pid_number >= 1.0).then_some(pid_number as i64)?;
    let uid = match object.get("uid")? {
        Value::Number(number) => number.clone(),
        _ => return None,
    };
    let listen = match object.get("listen")? {
        Value::Null => None,
        Value::String(listen) => Some(listen.clone()),
        _ => return None,
    };
    let server_id = match object.get("serverId") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(id)) => Some(Some(id.clone())),
        Some(_) => return None,
    };
    let desktop_managed = match object.get("desktopManaged") {
        None => None,
        Some(Value::Bool(flag)) => Some(*flag),
        Some(_) => return None,
    };
    let heartbeat = match object.get("heartbeat") {
        None => false,
        Some(Value::Bool(true)) => true,
        Some(_) => return None,
    };
    Some(PidLockInfo {
        pid,
        started_at: object.get("startedAt")?.as_str()?.to_owned(),
        hostname: object.get("hostname")?.as_str()?.to_owned(),
        uid,
        listen,
        server_id,
        desktop_managed,
        heartbeat,
    })
}

fn json_string(text: &str) -> String {
    Value::from(text).to_string()
}

fn json_nullable(text: Option<&str>) -> String {
    text.map_or_else(|| "null".to_owned(), json_string)
}

impl PidLockInfo {
    /// The object literal `acquirePidLock` writes.
    fn to_initial_json(&self) -> String {
        let mut text = format!(
            "{{\"pid\":{},\"startedAt\":{},\"hostname\":{},\"uid\":{},\"listen\":{}",
            self.pid,
            json_string(&self.started_at),
            json_string(&self.hostname),
            self.uid,
            json_nullable(self.listen.as_deref()),
        );
        if self.heartbeat {
            text.push_str(",\"heartbeat\":true");
        }
        if let Some(flag) = self.desktop_managed {
            let _ = write!(text, ",\"desktopManaged\":{flag}");
        }
        text.push('}');
        text
    }

    /// `JSON.stringify({ ...parsed, ...patch })`: zod key order, with a
    /// `serverId` the file did not have yet appended last.
    fn to_updated_json(&self, appended_server_id: bool) -> String {
        let server_id = self
            .server_id
            .as_ref()
            .map(|id| format!("\"serverId\":{}", json_nullable(id.as_deref())));
        let mut fields = vec![
            format!("\"pid\":{}", self.pid),
            format!("\"startedAt\":{}", json_string(&self.started_at)),
            format!("\"hostname\":{}", json_string(&self.hostname)),
            format!("\"uid\":{}", self.uid),
            format!("\"listen\":{}", json_nullable(self.listen.as_deref())),
        ];
        if !appended_server_id {
            fields.extend(server_id.clone());
        }
        if let Some(flag) = self.desktop_managed {
            fields.push(format!("\"desktopManaged\":{flag}"));
        }
        if self.heartbeat {
            fields.push("\"heartbeat\":true".to_owned());
        }
        if appended_server_id {
            fields.extend(server_id);
        }
        format!("{{{}}}", fields.join(","))
    }
}

/// `isPidRunning`: signal 0 succeeds, or fails with `EPERM`.
fn is_pid_running(pid: i64) -> bool {
    let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return false;
    };
    match test_kill_process(pid) {
        Ok(()) => true,
        Err(error) => error == rustix::io::Errno::PERM,
    }
}

/// `os.uptime()` in seconds, or `None` where the platform value is unavailable.
fn uptime_seconds() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        fs::read_to_string("/proc/uptime")
            .ok()?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("sysctl")
            .args(["-n", "kern.boottime"])
            .output()
            .ok()?;
        let text = String::from_utf8(output.stdout).ok()?;
        let seconds: f64 = text
            .split("sec =")
            .nth(1)?
            .split(',')
            .next()?
            .trim()
            .parse()
            .ok()?;
        #[allow(clippy::cast_precision_loss)]
        Some((now_ms() as f64 / 1000.0 - seconds).floor())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// `precedesThisBoot`: a process cannot predate the boot it runs under.
#[allow(clippy::cast_precision_loss)]
fn precedes_this_boot(started_at: &str) -> bool {
    let (Some(stamped), Some(uptime)) = (parse_iso(started_at), uptime_seconds()) else {
        return false;
    };
    (stamped as f64) < now_ms() as f64 - uptime * 1000.0 - BOOT_INSTANT_TOLERANCE_MS
}

/// `isPidLockOwnerRunning`: a lock stamped before this boot is abandoned
/// however alive its PID looks.
#[must_use]
pub fn is_pid_lock_owner_running(lock: &PidLockInfo) -> bool {
    if precedes_this_boot(&lock.started_at) {
        return false;
    }
    is_pid_running(lock.pid)
}

fn pid_file_path(paseo_home: &Path) -> PathBuf {
    paseo_home.join("paseo.pid")
}

/// `isSamePidLock`.
#[must_use]
pub fn is_same_pid_lock(left: &PidLockInfo, right: &PidLockInfo) -> bool {
    left.pid == right.pid && left.started_at == right.started_at
}

fn touch(pid_path: &Path) -> io::Result<()> {
    let now = SystemTime::now();
    OpenOptions::new()
        .write(true)
        .open(pid_path)?
        .set_times(FileTimes::new().set_accessed(now).set_modified(now))
}

/// `readPidLock`: up to 10 reads 50 ms apart, because a writer creates the file
/// just before it fills it. A missing file is `None`; a file still empty after
/// the retries was abandoned and is `None`; anything else unreadable is an
/// error with code `DAEMON_STATE_READ_FAILED`.
fn read_pid_lock(pid_path: &Path) -> Result<Option<PidLockInfo>, PidLockError> {
    let mut last_error = String::new();
    let mut empty = false;
    for _ in 0..PID_LOCK_READ_RETRY_ATTEMPTS {
        match fs::read(pid_path) {
            Ok(bytes) => {
                let content = String::from_utf8_lossy(&bytes);
                empty = content.is_empty();
                if !empty {
                    match serde_json::from_str::<Value>(&content) {
                        Ok(value) => match parse_pid_lock_info(&value) {
                            Some(lock) => return Ok(Some(lock)),
                            None => "Invalid lock shape".clone_into(&mut last_error),
                        },
                        Err(error) => {
                            empty = false;
                            last_error = error.to_string();
                        }
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                empty = false;
                last_error = error.to_string();
            }
        }
        // The baseline waits after every attempt, the last one included.
        thread::sleep(PID_LOCK_READ_RETRY_DELAY);
    }
    if empty {
        return Ok(None);
    }
    Err(PidLockError::Lock {
        message: format!(
            "Cannot read daemon state at {}: {last_error}",
            pid_path.display()
        ),
        existing: None,
        code: Some("DAEMON_STATE_READ_FAILED"),
    })
}

fn resolve_owner_pid(owner_pid: Option<u32>) -> i64 {
    owner_pid
        .filter(|pid| *pid > 0)
        .map_or_else(|| i64::from(std::process::id()), i64::from)
}

fn lock_held_error(lock: &PidLockInfo) -> PidLockError {
    PidLockError::held(
        format!(
            "Another Paseo daemon is already running (PID {}, started {})",
            lock.pid, lock.started_at
        ),
        lock.clone(),
    )
}

enum Cleared {
    AlreadyOwned,
    Cleared,
}

/// `clearExistingPidLock`.
fn clear_existing_pid_lock(
    pid_path: &Path,
    existing: &PidLockInfo,
    lock_owner_pid: i64,
) -> Result<Cleared, PidLockError> {
    let owner_running = is_pid_lock_owner_running(existing);
    if existing.pid == lock_owner_pid && owner_running {
        touch(pid_path)?;
        return Ok(Cleared::AlreadyOwned);
    }
    if owner_running {
        return Err(lock_held_error(existing));
    }
    let confirmed = read_pid_lock(pid_path)?;
    match confirmed {
        Some(confirmed)
            if is_same_pid_lock(existing, &confirmed) && !is_pid_lock_owner_running(&confirmed) =>
        {
            let _ = fs::remove_file(pid_path);
            Ok(Cleared::Cleared)
        }
        _ => Err(PidLockError::lock(
            "PID lock changed while checking whether it was abandoned",
        )),
    }
}

/// `removeEmptyPidLock`.
fn remove_empty_pid_lock(pid_path: &Path) -> io::Result<()> {
    match fs::metadata(pid_path) {
        Ok(metadata) if metadata.len() == 0 => fs::remove_file(pid_path),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// `writeNewPidLock`: exclusive create (`wx`), default file mode.
fn write_new_pid_lock(pid_path: &Path, lock: &PidLockInfo) -> Result<(), PidLockError> {
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(pid_path)
    {
        Ok(mut file) => {
            file.write_all(lock.to_initial_json().as_bytes())?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            match read_pid_lock(pid_path)? {
                Some(race) => Err(PidLockError::held(
                    format!("Another Paseo daemon is already running (PID {})", race.pid),
                    race,
                )),
                None => Err(PidLockError::lock(
                    "Failed to acquire PID lock due to race condition",
                )),
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// Inputs `acquirePidLock` reads from its options and the environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct AcquireOptions {
    /// `ownerPid`; the current process when `None`.
    pub owner_pid: Option<u32>,
    /// `process.env.PASEO_DESKTOP_MANAGED === "1"`.
    pub desktop_managed: bool,
}

/// `acquirePidLock`.
///
/// # Errors
///
/// [`PidLockError::Lock`] when a live daemon holds the lock or the file changed
/// during the abandonment check; [`PidLockError::Io`] for file-system failures.
pub fn acquire_pid_lock(
    paseo_home: &Path,
    listen: Option<&str>,
    options: AcquireOptions,
) -> Result<(), PidLockError> {
    let pid_path = pid_file_path(paseo_home);
    ensure_private_directory(paseo_home)?;

    let lock_owner_pid = resolve_owner_pid(options.owner_pid);
    match read_pid_lock(&pid_path)? {
        Some(existing) => {
            if matches!(
                clear_existing_pid_lock(&pid_path, &existing, lock_owner_pid)?,
                Cleared::AlreadyOwned
            ) {
                return Ok(());
            }
        }
        None => remove_empty_pid_lock(&pid_path)?,
    }

    let lock = PidLockInfo {
        pid: lock_owner_pid,
        started_at: to_iso_string(now_ms()),
        hostname: gethostname::gethostname().to_string_lossy().into_owned(),
        uid: rustix::process::getuid().as_raw().into(),
        listen: listen.map(str::to_owned),
        server_id: None,
        desktop_managed: options.desktop_managed.then_some(true),
        heartbeat: true,
    };
    write_new_pid_lock(&pid_path, &lock)
}

/// `readPidLockFromHandle`: any failure is `None`.
fn read_pid_lock_from_handle(file: &mut File) -> Option<PidLockInfo> {
    let size = file.metadata().ok()?.len();
    if size == 0 {
        return None;
    }
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut content = Vec::new();
    file.take(size).read_to_end(&mut content).ok()?;
    let value = serde_json::from_str::<Value>(&String::from_utf8_lossy(&content)).ok()?;
    parse_pid_lock_info(&value)
}

/// `readPidLockFromHandleWithRetry`.
fn read_pid_lock_from_handle_with_retry(file: &mut File) -> Option<PidLockInfo> {
    for attempt in 0..PID_LOCK_READ_RETRY_ATTEMPTS {
        if let Some(lock) = read_pid_lock_from_handle(file) {
            return Some(lock);
        }
        if attempt < PID_LOCK_READ_RETRY_ATTEMPTS - 1 {
            thread::sleep(PID_LOCK_READ_RETRY_DELAY);
        }
    }
    None
}

/// `refreshPidLock`: touch the lock file while the caller still owns it.
///
/// # Errors
///
/// [`PidLockError::Lock`] when the file is missing, invalid, or owned by another
/// PID; [`PidLockError::Io`] otherwise.
pub fn refresh_pid_lock(paseo_home: &Path, owner_pid: Option<u32>) -> Result<(), PidLockError> {
    let pid_path = pid_file_path(paseo_home);
    let lock_owner_pid = resolve_owner_pid(owner_pid);
    let mut file = match OpenOptions::new().read(true).write(true).open(&pid_path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(PidLockError::lock(
                "Cannot refresh PID lock: lock file is missing",
            ));
        }
        Err(error) => return Err(error.into()),
    };
    let Some(lock) = read_pid_lock_from_handle_with_retry(&mut file) else {
        return Err(PidLockError::lock(
            "Cannot refresh PID lock: invalid lock file",
        ));
    };
    if lock.pid != lock_owner_pid {
        let message = format!("Cannot refresh PID lock owned by PID {}", lock.pid);
        return Err(PidLockError::held(message, lock));
    }
    let now = SystemTime::now();
    file.set_times(FileTimes::new().set_accessed(now).set_modified(now))?;
    Ok(())
}

/// A running heartbeat; dropping it or calling [`HeartbeatHandle::stop`] ends it.
pub struct HeartbeatHandle {
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl HeartbeatHandle {
    /// `clearInterval`: stop and wait for the timer thread.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for HeartbeatHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// `onError` of `startPidLockHeartbeat`.
pub type HeartbeatErrorHandler = Box<dyn Fn(&PidLockError) + Send + 'static>;

/// `startPidLockHeartbeat`: refresh every `interval`. With no handler a failure
/// is written to stderr as `PID lock heartbeat failed: <message>`.
#[must_use]
pub fn start_pid_lock_heartbeat(
    paseo_home: PathBuf,
    owner_pid: Option<u32>,
    interval: Duration,
    on_error: Option<HeartbeatErrorHandler>,
) -> HeartbeatHandle {
    let (stop, stopped) = mpsc::channel::<()>();
    let thread = thread::spawn(move || {
        while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(interval) {
            if let Err(error) = refresh_pid_lock(&paseo_home, owner_pid) {
                match &on_error {
                    Some(handler) => handler(&error),
                    None => {
                        let _ = writeln!(io::stderr(), "PID lock heartbeat failed: {error}");
                    }
                }
            }
        }
    });
    HeartbeatHandle {
        stop: Some(stop),
        thread: Some(thread),
    }
}

/// The patch `updatePidLock` accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PidLockPatch {
    Listening { listen: String, server_id: String },
    Cleared,
}

/// `updatePidLock`: rewrite the lock with a new `listen` and `serverId`.
///
/// # Errors
///
/// [`PidLockError::Lock`] when the file is invalid or owned by another PID;
/// [`PidLockError::Io`] otherwise (including a missing file).
pub fn update_pid_lock(
    paseo_home: &Path,
    patch: &PidLockPatch,
    owner_pid: Option<u32>,
) -> Result<(), PidLockError> {
    let pid_path = pid_file_path(paseo_home);
    let lock_owner_pid = resolve_owner_pid(owner_pid);
    let mut file = OpenOptions::new().read(true).write(true).open(&pid_path)?;
    let Some(mut lock) = read_pid_lock_from_handle_with_retry(&mut file) else {
        return Err(PidLockError::lock(
            "Cannot update PID lock: invalid lock file",
        ));
    };
    if lock.pid != lock_owner_pid {
        let message = format!("Cannot update PID lock owned by PID {}", lock.pid);
        return Err(PidLockError::held(message, lock));
    }
    let appended_server_id = lock.server_id.is_none();
    match patch {
        PidLockPatch::Listening { listen, server_id } => {
            lock.listen = Some(listen.clone());
            lock.server_id = Some(Some(server_id.clone()));
        }
        PidLockPatch::Cleared => {
            lock.listen = None;
            lock.server_id = Some(None);
        }
    }
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(lock.to_updated_json(appended_server_id).as_bytes())?;
    Ok(())
}

/// `releasePidLock`: remove the file only when it is this owner's lock. Every
/// error is ignored; the lock may already be gone.
pub fn release_pid_lock(paseo_home: &Path, owner_pid: Option<u32>, started_at: Option<&str>) {
    let pid_path = pid_file_path(paseo_home);
    let lock_owner_pid = resolve_owner_pid(owner_pid);
    let Ok(content) = fs::read(&pid_path) else {
        return;
    };
    let Ok(value) = serde_json::from_str::<Value>(&String::from_utf8_lossy(&content)) else {
        return;
    };
    if let Some(lock) = parse_pid_lock_info(&value)
        && lock.pid == lock_owner_pid
        && started_at.is_none_or(|started| lock.started_at == started)
    {
        let _ = fs::remove_file(&pid_path);
    }
}

/// `getPidLockInfo`.
///
/// # Errors
///
/// [`PidLockError::Lock`] with code `DAEMON_STATE_READ_FAILED` when the file
/// exists but never becomes valid.
pub fn get_pid_lock_info(paseo_home: &Path) -> Result<Option<PidLockInfo>, PidLockError> {
    read_pid_lock(&pid_file_path(paseo_home))
}

/// `isLocked`: whether a live owner holds the lock, with the lock if any.
///
/// # Errors
///
/// As [`get_pid_lock_info`].
pub fn is_locked(paseo_home: &Path) -> Result<(bool, Option<PidLockInfo>), PidLockError> {
    let Some(info) = get_pid_lock_info(paseo_home)? else {
        return Ok((false, None));
    };
    let locked = is_pid_lock_owner_running(&info);
    Ok((locked, Some(info)))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const STARTED: &str = "2026-10-01T14:00:00.000Z";

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn lock_file(home: &Path) -> PathBuf {
        home.join("paseo.pid")
    }

    fn sample(desktop_managed: bool) -> PidLockInfo {
        PidLockInfo {
            pid: 4242,
            started_at: STARTED.to_owned(),
            hostname: "box".to_owned(),
            uid: 501.into(),
            listen: None,
            server_id: None,
            desktop_managed: desktop_managed.then_some(true),
            heartbeat: true,
        }
    }

    fn live_child() -> Child {
        Command::new("sleep")
            .arg("60")
            .stdout(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn dead_pid() -> i64 {
        let mut child = Command::new("true").spawn().unwrap();
        let pid = i64::from(child.id());
        child.wait().unwrap();
        pid
    }

    fn write_lock(home: &Path, pid: i64, started_at: &str) {
        let mut lock = sample(false);
        lock.pid = pid;
        started_at.clone_into(&mut lock.started_at);
        fs::write(lock_file(home), lock.to_initial_json()).unwrap();
    }

    // Expected strings come from the pinned zod schema and JSON.stringify:
    // `JSON.stringify({ ...schema.parse(JSON.parse(previous)), listen, serverId })`.
    const ORACLE: [(bool, [&str; 4]); 2] = [
        (
            false,
            [
                r#"{"pid":4242,"startedAt":"2026-10-01T14:00:00.000Z","hostname":"box","uid":501,"listen":null,"heartbeat":true}"#,
                r#"{"pid":4242,"startedAt":"2026-10-01T14:00:00.000Z","hostname":"box","uid":501,"listen":"127.0.0.1:7000","heartbeat":true,"serverId":"srv_abc"}"#,
                r#"{"pid":4242,"startedAt":"2026-10-01T14:00:00.000Z","hostname":"box","uid":501,"listen":null,"serverId":null,"heartbeat":true}"#,
                r#"{"pid":4242,"startedAt":"2026-10-01T14:00:00.000Z","hostname":"box","uid":501,"listen":"[::1]:1","serverId":"srv_d","heartbeat":true}"#,
            ],
        ),
        (
            true,
            [
                r#"{"pid":4242,"startedAt":"2026-10-01T14:00:00.000Z","hostname":"box","uid":501,"listen":null,"heartbeat":true,"desktopManaged":true}"#,
                r#"{"pid":4242,"startedAt":"2026-10-01T14:00:00.000Z","hostname":"box","uid":501,"listen":"127.0.0.1:7000","desktopManaged":true,"heartbeat":true,"serverId":"srv_abc"}"#,
                r#"{"pid":4242,"startedAt":"2026-10-01T14:00:00.000Z","hostname":"box","uid":501,"listen":null,"serverId":null,"desktopManaged":true,"heartbeat":true}"#,
                r#"{"pid":4242,"startedAt":"2026-10-01T14:00:00.000Z","hostname":"box","uid":501,"listen":"[::1]:1","serverId":"srv_d","desktopManaged":true,"heartbeat":true}"#,
            ],
        ),
    ];

    #[test]
    fn the_file_text_and_key_order_match_the_pinned_writer_through_updates() {
        for (desktop_managed, expected) in ORACLE {
            let home = home();
            fs::write(
                lock_file(home.path()),
                sample(desktop_managed).to_initial_json(),
            )
            .unwrap();
            assert_eq!(
                fs::read_to_string(lock_file(home.path())).unwrap(),
                expected[0]
            );
            let owner = Some(4242);
            let steps = [
                PidLockPatch::Listening {
                    listen: "127.0.0.1:7000".to_owned(),
                    server_id: "srv_abc".to_owned(),
                },
                PidLockPatch::Cleared,
                PidLockPatch::Listening {
                    listen: "[::1]:1".to_owned(),
                    server_id: "srv_d".to_owned(),
                },
            ];
            for (step, want) in steps.iter().zip(&expected[1..]) {
                update_pid_lock(home.path(), step, owner).unwrap();
                assert_eq!(&fs::read_to_string(lock_file(home.path())).unwrap(), want);
            }
        }
    }

    #[test]
    fn a_shorter_rewrite_leaves_no_trailing_bytes() {
        let home = home();
        fs::write(lock_file(home.path()), sample(true).to_initial_json()).unwrap();
        let long = PidLockPatch::Listening {
            listen: "a".repeat(200),
            server_id: "b".repeat(50),
        };
        update_pid_lock(home.path(), &long, Some(4242)).unwrap();
        update_pid_lock(home.path(), &PidLockPatch::Cleared, Some(4242)).unwrap();
        let text = fs::read_to_string(lock_file(home.path())).unwrap();
        assert_eq!(text, ORACLE[1].1[2]);
    }

    #[test]
    fn acquire_writes_the_initial_record() {
        let home = home();
        let options = AcquireOptions {
            owner_pid: None,
            desktop_managed: true,
        };
        acquire_pid_lock(&home.path().join("nested"), None, options).unwrap();
        let text = fs::read_to_string(lock_file(&home.path().join("nested"))).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        let keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            [
                "pid",
                "startedAt",
                "hostname",
                "uid",
                "listen",
                "heartbeat",
                "desktopManaged"
            ]
        );
        assert_eq!(value["pid"], std::process::id());
        assert_eq!(value["listen"], Value::Null);
        assert!(parse_iso(value["startedAt"].as_str().unwrap()).is_some());
    }

    #[test]
    fn acquire_by_the_current_owner_keeps_the_record() {
        let home = home();
        acquire_pid_lock(home.path(), Some("127.0.0.1:1"), AcquireOptions::default()).unwrap();
        let first = fs::read_to_string(lock_file(home.path())).unwrap();
        acquire_pid_lock(home.path(), Some("other"), AcquireOptions::default()).unwrap();
        assert_eq!(fs::read_to_string(lock_file(home.path())).unwrap(), first);
    }

    #[test]
    fn acquire_is_refused_while_another_live_process_holds_the_lock() {
        let home = home();
        let mut child = live_child();
        write_lock(home.path(), i64::from(child.id()), &to_iso_string(now_ms()));
        let error = acquire_pid_lock(home.path(), None, AcquireOptions::default()).unwrap_err();
        child.kill().unwrap();
        child.wait().unwrap();
        let PidLockError::Lock {
            message, existing, ..
        } = error
        else {
            panic!("expected a lock error");
        };
        assert!(message.starts_with("Another Paseo daemon is already running (PID "));
        assert!(message.contains(", started "));
        assert_eq!(existing.unwrap().pid, i64::from(child.id()));
    }

    #[test]
    fn a_lock_whose_process_is_gone_is_replaced() {
        let home = home();
        write_lock(home.path(), dead_pid(), &to_iso_string(now_ms()));
        acquire_pid_lock(home.path(), None, AcquireOptions::default()).unwrap();
        let info = get_pid_lock_info(home.path()).unwrap().unwrap();
        assert_eq!(info.pid, i64::from(std::process::id()));
    }

    #[test]
    fn a_lock_stamped_before_this_boot_is_abandoned_even_if_the_pid_lives() {
        let home = home();
        let mut child = live_child();
        write_lock(
            home.path(),
            i64::from(child.id()),
            "2000-01-01T00:00:00.000Z",
        );
        let stale = get_pid_lock_info(home.path()).unwrap().unwrap();
        assert!(!is_pid_lock_owner_running(&stale));
        acquire_pid_lock(home.path(), None, AcquireOptions::default()).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(
            get_pid_lock_info(home.path()).unwrap().unwrap().pid,
            i64::from(std::process::id())
        );
    }

    #[test]
    fn a_current_lock_with_a_live_pid_is_running() {
        let mut live = sample(false);
        live.pid = i64::from(std::process::id());
        live.started_at = to_iso_string(now_ms());
        assert!(is_pid_lock_owner_running(&live));
        live.pid = dead_pid();
        assert!(!is_pid_lock_owner_running(&live));
        live.pid = i64::MAX;
        assert!(!is_pid_lock_owner_running(&live));
    }

    #[test]
    fn an_empty_lock_file_is_removed_after_the_read_retries() {
        let home = home();
        fs::write(lock_file(home.path()), "").unwrap();
        acquire_pid_lock(home.path(), None, AcquireOptions::default()).unwrap();
        assert_eq!(
            get_pid_lock_info(home.path()).unwrap().unwrap().pid,
            i64::from(std::process::id())
        );
    }

    #[test]
    fn an_unreadable_lock_file_is_a_state_read_error() {
        let home = home();
        fs::write(lock_file(home.path()), "{not json").unwrap();
        let error = get_pid_lock_info(home.path()).unwrap_err();
        let PidLockError::Lock { message, code, .. } = error else {
            panic!("expected a lock error");
        };
        assert_eq!(code, Some("DAEMON_STATE_READ_FAILED"));
        assert!(message.starts_with("Cannot read daemon state at "));
        fs::write(lock_file(home.path()), r#"{"pid":1}"#).unwrap();
        let PidLockError::Lock { message, .. } = get_pid_lock_info(home.path()).unwrap_err() else {
            panic!("expected a lock error");
        };
        assert!(message.ends_with("Invalid lock shape"));
    }

    #[test]
    fn the_schema_rejects_what_zod_rejects() {
        let valid = r#"{"pid":5,"startedAt":"x","hostname":"h","uid":0,"listen":null}"#;
        let parse = |text: &str| parse_pid_lock_info(&serde_json::from_str(text).unwrap());
        assert!(parse(valid).is_some());
        for bad in [
            r#"{"pid":0,"startedAt":"x","hostname":"h","uid":0,"listen":null}"#,
            r#"{"pid":1.5,"startedAt":"x","hostname":"h","uid":0,"listen":null}"#,
            r#"{"pid":"5","startedAt":"x","hostname":"h","uid":0,"listen":null}"#,
            r#"{"pid":5,"startedAt":"x","hostname":"h","uid":0}"#,
            r#"{"pid":5,"startedAt":"x","hostname":"h","uid":"0","listen":null}"#,
            r#"{"pid":5,"startedAt":"x","hostname":"h","uid":0,"listen":null,"heartbeat":false}"#,
            r#"{"pid":5,"startedAt":"x","hostname":"h","uid":0,"listen":null,"desktopManaged":null}"#,
            r#"{"pid":5,"startedAt":"x","hostname":"h","uid":0,"listen":null,"serverId":1}"#,
            r"[]",
        ] {
            assert!(parse(bad).is_none(), "{bad}");
        }
        assert!(
            parse(r#"{"pid":5.0,"startedAt":"x","hostname":"h","uid":0,"listen":null,"extra":1}"#)
                .is_some()
        );
    }

    #[test]
    fn update_refuses_another_owner_and_a_missing_file() {
        let home = home();
        assert!(matches!(
            update_pid_lock(home.path(), &PidLockPatch::Cleared, Some(1)),
            Err(PidLockError::Io(_))
        ));
        fs::write(lock_file(home.path()), sample(false).to_initial_json()).unwrap();
        let PidLockError::Lock {
            message, existing, ..
        } = update_pid_lock(home.path(), &PidLockPatch::Cleared, Some(7)).unwrap_err()
        else {
            panic!("expected a lock error");
        };
        assert_eq!(message, "Cannot update PID lock owned by PID 4242");
        assert_eq!(existing.unwrap().pid, 4242);
    }

    #[test]
    fn refresh_touches_the_file_only_for_the_owner() {
        let home = home();
        let PidLockError::Lock { message, .. } =
            refresh_pid_lock(home.path(), Some(4242)).unwrap_err()
        else {
            panic!("expected a lock error");
        };
        assert_eq!(message, "Cannot refresh PID lock: lock file is missing");
        fs::write(lock_file(home.path()), sample(false).to_initial_json()).unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        File::options()
            .write(true)
            .open(lock_file(home.path()))
            .unwrap()
            .set_times(FileTimes::new().set_modified(old))
            .unwrap();
        refresh_pid_lock(home.path(), Some(4242)).unwrap();
        let modified = fs::metadata(lock_file(home.path()))
            .unwrap()
            .modified()
            .unwrap();
        assert!(modified > old + Duration::from_secs(3000));
        let PidLockError::Lock { message, .. } =
            refresh_pid_lock(home.path(), Some(1)).unwrap_err()
        else {
            panic!("expected a lock error");
        };
        assert_eq!(message, "Cannot refresh PID lock owned by PID 4242");
    }

    #[test]
    fn release_removes_only_the_owners_matching_lock() {
        let home = home();
        fs::write(lock_file(home.path()), sample(false).to_initial_json()).unwrap();
        release_pid_lock(home.path(), Some(1), None);
        assert!(lock_file(home.path()).exists());
        release_pid_lock(home.path(), Some(4242), Some("2000-01-01T00:00:00.000Z"));
        assert!(lock_file(home.path()).exists());
        release_pid_lock(home.path(), Some(4242), Some(STARTED));
        assert!(!lock_file(home.path()).exists());
        release_pid_lock(home.path(), Some(4242), None);
        fs::write(lock_file(home.path()), "garbage").unwrap();
        release_pid_lock(home.path(), Some(4242), None);
        assert!(lock_file(home.path()).exists());
    }

    #[test]
    fn is_locked_reports_a_live_owner() {
        let home = home();
        assert!(!is_locked(home.path()).unwrap().0);
        acquire_pid_lock(home.path(), None, AcquireOptions::default()).unwrap();
        let (locked, info) = is_locked(home.path()).unwrap();
        assert!(locked);
        assert_eq!(info.unwrap().pid, i64::from(std::process::id()));
        write_lock(home.path(), dead_pid(), &to_iso_string(now_ms()));
        assert!(!is_locked(home.path()).unwrap().0);
    }

    #[test]
    fn the_heartbeat_refreshes_and_reports_errors_until_stopped() {
        let home = home();
        let errors = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&errors);
        let handle = start_pid_lock_heartbeat(
            home.path().to_path_buf(),
            Some(4242),
            Duration::from_millis(20),
            Some(Box::new(move |error| {
                assert!(error.to_string().contains("lock file is missing"));
                seen.fetch_add(1, Ordering::SeqCst);
            })),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while errors.load(Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        handle.stop();
        let after_stop = errors.load(Ordering::SeqCst);
        assert!(after_stop >= 2);
        thread::sleep(Duration::from_millis(100));
        assert_eq!(errors.load(Ordering::SeqCst), after_stop);
    }
}
