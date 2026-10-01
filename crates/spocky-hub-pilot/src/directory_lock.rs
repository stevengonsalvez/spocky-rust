use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(crate) const LOCK_FILE: &str = ".paseo-hub.lock";
const GUARD_FILE: &str = ".spocky-hub.lock.guard";
const OWNER_READ_ATTEMPTS: usize = 10;
const OWNER_READ_DELAY: Duration = Duration::from_millis(10);
const LOCK_PROTOCOL: &str = "os-file-lock-v1";

#[derive(Debug)]
pub(crate) enum DirectoryLockError {
    Busy,
    Io(std::io::Error),
}

impl From<std::io::Error> for DirectoryLockError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Deserialize, Serialize)]
struct LockOwner {
    pid: u32,
    token: String,
    #[serde(default)]
    protocol: Option<String>,
}

pub(crate) struct DataDirectoryLock {
    guard: File,
    owner_file: File,
    owner: LockOwner,
}

impl DataDirectoryLock {
    pub(crate) fn acquire(data_directory: &Path) -> Result<Self, DirectoryLockError> {
        let guard_path = data_directory.join(GUARD_FILE);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;

            options.mode(0o600);
        }
        let guard = options.open(guard_path)?;
        match guard.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(DirectoryLockError::Busy);
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            guard.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }

        let owner_path = data_directory.join(LOCK_FILE);
        let mut owner_file = loop {
            let mut owner_options = OpenOptions::new();
            owner_options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;

                owner_options.mode(0o600);
            }
            match owner_options.open(&owner_path) {
                Ok(file) => break file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }

            let mut existing = match OpenOptions::new().read(true).write(true).open(&owner_path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if read_legacy_owner(&mut existing)?
                .is_some_and(|owner| owner.protocol.is_none() && process_is_running(owner.pid))
            {
                return Err(DirectoryLockError::Busy);
            }
            drop(existing);
            match fs::remove_file(&owner_path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            owner_file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        let owner = LockOwner {
            pid: std::process::id(),
            token: Uuid::new_v4().to_string(),
            protocol: Some(LOCK_PROTOCOL.to_owned()),
        };
        write_owner(&mut owner_file, &owner)?;
        Ok(Self {
            guard,
            owner_file,
            owner,
        })
    }

    pub(crate) fn inherited_guard(&self) -> Result<File, std::io::Error> {
        self.guard.try_clone()
    }

    pub(crate) fn set_owner_pid(&mut self, pid: u32) -> Result<(), std::io::Error> {
        self.owner.pid = pid;
        write_owner(&mut self.owner_file, &self.owner)
    }
}

fn write_owner(file: &mut File, owner: &LockOwner) -> Result<(), std::io::Error> {
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    serde_json::to_writer(&mut *file, owner).map_err(std::io::Error::other)?;
    file.flush()
}

fn read_legacy_owner(file: &mut File) -> Result<Option<LockOwner>, std::io::Error> {
    for _ in 0..OWNER_READ_ATTEMPTS {
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if !bytes.is_empty()
            && let Ok(owner) = serde_json::from_slice(&bytes)
        {
            return Ok(Some(owner));
        }
        thread::sleep(OWNER_READ_DELAY);
    }
    Ok(None)
}

fn process_is_running(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    let Ok(output) = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
    else {
        return false;
    };
    output.status.success()
        || String::from_utf8_lossy(&output.stderr).contains("Operation not permitted")
}
