//! Bounded IPC client for the retained `PGlite` JavaScript host.

use std::fmt;
use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::directory_lock::{DataDirectoryLock, DirectoryLockError};

#[derive(Clone, Debug)]
pub struct RetainedPgliteConfig {
    pub node_executable: PathBuf,
    pub adapter_path: PathBuf,
    pub package_root: PathBuf,
    pub migrations_root: PathBuf,
    pub data_directory: PathBuf,
    pub max_frame_bytes: usize,
    pub startup_timeout: Duration,
    pub request_timeout: Duration,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostIdentity {
    pub node_version: String,
    pub node_executable: String,
    pub node_executable_sha256: String,
    pub os: String,
    pub arch: String,
    pub package: String,
    pub package_version: String,
    pub package_dependencies: serde_json::Value,
    pub adapter_dependencies: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum IpcValue {
    Null,
    Boolean(bool),
    String(String),
    Binary(Vec<u8>),
    Timestamp(String),
    Numeric(String),
    Json(serde_json::Value),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<IpcValue>>,
    pub affected_rows: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SqlStatement {
    sql: String,
    params: Vec<IpcValue>,
}

impl SqlStatement {
    #[must_use]
    pub fn new(sql: impl Into<String>, params: Vec<IpcValue>) -> Self {
        Self {
            sql: sql.into(),
            params,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MigrationOutcome {
    pub applied: usize,
    pub journal_rows: usize,
}

#[derive(Debug)]
pub enum RetainedHostError {
    Io(std::io::Error),
    Json(serde_json::Error),
    DirectoryInUse,
    FrameTooLarge {
        actual: usize,
        maximum: usize,
    },
    Protocol(String),
    Remote {
        code: String,
        message: String,
        details: serde_json::Value,
    },
    Timeout {
        request_id: u64,
        write_may_have_committed: bool,
    },
    DeliveryTimeout {
        request_id: u64,
        write_may_have_committed: bool,
    },
    DeliveryFailed {
        request_id: u64,
        write_may_have_committed: bool,
        cause: String,
    },
    ReplyLost {
        request_id: u64,
        write_may_have_committed: bool,
        cause: String,
    },
    Poisoned,
}

impl fmt::Display for RetainedHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Json(error) => error.fmt(formatter),
            Self::DirectoryInUse => formatter.write_str("PGlite data directory is already in use"),
            Self::FrameTooLarge { actual, maximum } => {
                write!(
                    formatter,
                    "IPC frame is {actual} bytes; maximum is {maximum}"
                )
            }
            Self::Protocol(message) => write!(formatter, "retained-host protocol error: {message}"),
            Self::Remote { code, message, .. } => {
                write!(formatter, "retained host {code}: {message}")
            }
            Self::Timeout { request_id, .. } => {
                write!(formatter, "retained-host request {request_id} timed out")
            }
            Self::DeliveryTimeout { request_id, .. } => {
                write!(
                    formatter,
                    "retained-host request {request_id} delivery timed out"
                )
            }
            Self::DeliveryFailed {
                request_id, cause, ..
            } => write!(
                formatter,
                "retained-host request {request_id} delivery failed: {cause}"
            ),
            Self::ReplyLost { request_id, .. } => {
                write!(
                    formatter,
                    "retained-host request {request_id} lost its reply"
                )
            }
            Self::Poisoned => formatter.write_str("retained-host lock poisoned"),
        }
    }
}

impl std::error::Error for RetainedHostError {}

impl From<std::io::Error> for RetainedHostError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for RetainedHostError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Deserialize)]
struct Response {
    id: u64,
    ok: bool,
    #[serde(default)]
    result: serde_json::Value,
    error: Option<RemoteError>,
}

#[derive(Deserialize)]
struct RemoteError {
    code: String,
    message: String,
    #[serde(default)]
    details: serde_json::Value,
}

struct HostState {
    child: Child,
    directory_lock: Option<DataDirectoryLock>,
    writer: mpsc::Sender<WriteRequest>,
    responses: Receiver<Result<Response, RetainedHostError>>,
    next_request_id: u64,
    closed: bool,
}

struct WriteRequest {
    frame: Vec<u8>,
    completion: mpsc::Sender<Result<(), std::io::Error>>,
}

pub struct RetainedPgliteHost {
    identity: HostIdentity,
    state: Mutex<HostState>,
    max_frame_bytes: usize,
    request_timeout: Duration,
}

impl RetainedPgliteHost {
    pub fn open(config: &RetainedPgliteConfig) -> Result<Self, RetainedHostError> {
        fs::create_dir_all(&config.data_directory)?;
        let directory_lock =
            DataDirectoryLock::acquire(&config.data_directory).map_err(|error| match error {
                DirectoryLockError::Busy => RetainedHostError::DirectoryInUse,
                DirectoryLockError::Io(error) => RetainedHostError::Io(error),
            })?;
        let mut child = Command::new(&config.node_executable)
            .arg(&config.adapter_path)
            .arg(&config.package_root)
            .arg(&config.migrations_root)
            .arg(&config.data_directory)
            .arg(config.max_frame_bytes.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RetainedHostError::Protocol("child stdin is absent".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RetainedHostError::Protocol("child stdout is absent".into()))?;
        let (writer, write_requests) = mpsc::channel::<WriteRequest>();
        thread::spawn(move || {
            let mut stdin = stdin;
            for request in write_requests {
                let result = stdin.write_all(&request.frame).and_then(|()| stdin.flush());
                let terminal = result.is_err();
                let _ = request.completion.send(result);
                if terminal {
                    break;
                }
            }
        });
        let (sender, responses) = mpsc::channel();
        let maximum = config.max_frame_bytes;
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let result = read_response(&mut reader, maximum);
                let terminal = result.is_err();
                if sender.send(result).is_err() || terminal {
                    break;
                }
            }
        });
        let identity = (|| {
            let hello = responses
                .recv_timeout(config.startup_timeout)
                .map_err(|_| RetainedHostError::ReplyLost {
                    request_id: 0,
                    write_may_have_committed: false,
                    cause: "startup reply channel closed or timed out".into(),
                })??;
            if hello.id != 0 {
                return Err(RetainedHostError::Protocol(format!(
                    "expected hello id 0, received {}",
                    hello.id
                )));
            }
            if !hello.ok {
                let error = hello.error.ok_or_else(|| {
                    RetainedHostError::Protocol("failed hello omitted its error".into())
                })?;
                if error.code == "DIRECTORY_IN_USE" {
                    return Err(RetainedHostError::DirectoryInUse);
                }
                return Err(RetainedHostError::Remote {
                    code: error.code,
                    message: error.message,
                    details: error.details,
                });
            }
            serde_json::from_value(hello.result).map_err(Into::into)
        })();
        let identity = match identity {
            Ok(identity) => identity,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        Ok(Self {
            identity,
            state: Mutex::new(HostState {
                child,
                directory_lock: Some(directory_lock),
                writer,
                responses,
                next_request_id: 1,
                closed: false,
            }),
            max_frame_bytes: config.max_frame_bytes,
            request_timeout: config.request_timeout,
        })
    }

    #[must_use]
    pub fn identity(&self) -> &HostIdentity {
        &self.identity
    }

    pub fn process_id(&self) -> Result<u32, RetainedHostError> {
        let state = self.state.lock().map_err(|_| RetainedHostError::Poisoned)?;
        Ok(state.child.id())
    }

    pub fn migrate(&self) -> Result<MigrationOutcome, RetainedHostError> {
        let value = self.request(serde_json::json!({ "operation": "migrate" }))?;
        serde_json::from_value(value).map_err(Into::into)
    }

    pub fn query(&self, sql: &str, params: &[IpcValue]) -> Result<QueryResult, RetainedHostError> {
        let value = self.request(serde_json::json!({
            "operation": "query",
            "sql": sql,
            "params": params,
        }))?;
        serde_json::from_value(value).map_err(Into::into)
    }

    pub fn execute(&self, sql: &str) -> Result<(), RetainedHostError> {
        self.request(serde_json::json!({
            "operation": "execute",
            "sql": sql,
        }))?;
        Ok(())
    }

    pub fn transaction(
        &self,
        statements: &[SqlStatement],
    ) -> Result<Vec<QueryResult>, RetainedHostError> {
        let value = self.request(serde_json::json!({
            "operation": "transaction",
            "statements": statements,
        }))?;
        serde_json::from_value(value).map_err(Into::into)
    }

    pub fn close(&self) -> Result<(), RetainedHostError> {
        let result = self
            .request(serde_json::json!({ "operation": "close" }))
            .map(|_| ());
        let mut state = self.state.lock().map_err(|_| RetainedHostError::Poisoned)?;
        state.closed = true;
        terminate_and_reap(&mut state.child, self.request_timeout);
        state.directory_lock = None;
        result
    }

    #[doc(hidden)]
    pub fn crash_for_test(&self) -> Result<(), RetainedHostError> {
        self.request(serde_json::json!({ "operation": "crash" }))?;
        Ok(())
    }

    #[doc(hidden)]
    pub fn execute_then_crash_for_test(&self, sql: &str) -> Result<(), RetainedHostError> {
        self.request(serde_json::json!({
            "operation": "executeThenCrash",
            "sql": sql,
        }))?;
        Ok(())
    }

    #[doc(hidden)]
    pub fn delay_for_test(&self, duration: Duration) -> Result<(), RetainedHostError> {
        self.request(serde_json::json!({
            "operation": "delay",
            "milliseconds": duration.as_millis(),
        }))?;
        Ok(())
    }

    #[doc(hidden)]
    pub fn stall_reads_for_test(&self) -> Result<(), RetainedHostError> {
        self.request(serde_json::json!({ "operation": "stallReads" }))?;
        Ok(())
    }

    #[doc(hidden)]
    pub fn fail_close_for_test(&self) -> Result<(), RetainedHostError> {
        self.request(serde_json::json!({ "operation": "failClose" }))?;
        Ok(())
    }

    fn request(
        &self,
        mut value: serde_json::Value,
    ) -> Result<serde_json::Value, RetainedHostError> {
        let mut state = self.state.lock().map_err(|_| RetainedHostError::Poisoned)?;
        if state.closed {
            return Err(RetainedHostError::Protocol("host is closed".into()));
        }
        let request_id = state.next_request_id;
        state.next_request_id += 1;
        value["id"] = request_id.into();
        deliver_frame(
            &mut state,
            &value,
            self.max_frame_bytes,
            self.request_timeout,
            request_id,
        )?;
        match state.responses.recv_timeout(self.request_timeout) {
            Ok(Ok(response)) if response.id == request_id && response.ok => Ok(response.result),
            Ok(Ok(response)) if response.id == request_id => {
                let error = response.error.ok_or_else(|| {
                    RetainedHostError::Protocol("failed response omitted its error".into())
                })?;
                if error.code == "FRAME_TOO_LARGE" {
                    state.closed = true;
                    let _ = state.child.kill();
                    return Err(RetainedHostError::ReplyLost {
                        request_id,
                        write_may_have_committed: true,
                        cause: error.message,
                    });
                }
                Err(RetainedHostError::Remote {
                    code: error.code,
                    message: error.message,
                    details: error.details,
                })
            }
            Ok(Ok(response)) => Err(RetainedHostError::Protocol(format!(
                "expected response {request_id}, received {}",
                response.id
            ))),
            Ok(Err(error)) => {
                state.closed = true;
                let _ = state.child.kill();
                Err(RetainedHostError::ReplyLost {
                    request_id,
                    write_may_have_committed: true,
                    cause: error.to_string(),
                })
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                state.closed = true;
                let _ = state.child.kill();
                Err(RetainedHostError::ReplyLost {
                    request_id,
                    write_may_have_committed: true,
                    cause: "response channel disconnected".into(),
                })
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                state.closed = true;
                let _ = state.child.kill();
                Err(RetainedHostError::Timeout {
                    request_id,
                    write_may_have_committed: true,
                })
            }
        }
    }
}

impl Drop for RetainedPgliteHost {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            if !state.closed {
                let request_id = state.next_request_id;
                state.next_request_id += 1;
                let close = serde_json::json!({ "id": request_id, "operation": "close" });
                let delivered = deliver_frame(
                    &mut state,
                    &close,
                    self.max_frame_bytes,
                    self.request_timeout,
                    request_id,
                );
                if delivered.is_ok() {
                    let _ = state.responses.recv_timeout(self.request_timeout);
                }
                state.closed = true;
            }
            terminate_and_reap(&mut state.child, self.request_timeout);
        }
    }
}

fn encode_frame(value: &serde_json::Value, maximum: usize) -> Result<Vec<u8>, RetainedHostError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > maximum {
        return Err(RetainedHostError::FrameTooLarge {
            actual: bytes.len(),
            maximum,
        });
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| RetainedHostError::Protocol("frame exceeds u32 length".into()))?;
    let mut frame = Vec::with_capacity(4 + bytes.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&bytes);
    Ok(frame)
}

fn deliver_frame(
    state: &mut HostState,
    value: &serde_json::Value,
    maximum: usize,
    timeout: Duration,
    request_id: u64,
) -> Result<(), RetainedHostError> {
    let frame = encode_frame(value, maximum)?;
    let (completion, completed) = mpsc::channel();
    if state
        .writer
        .send(WriteRequest { frame, completion })
        .is_err()
    {
        state.closed = true;
        terminate_and_reap(&mut state.child, timeout);
        return Err(RetainedHostError::DeliveryFailed {
            request_id,
            write_may_have_committed: true,
            cause: "writer channel disconnected".into(),
        });
    }
    match completed.recv_timeout(timeout) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            state.closed = true;
            terminate_and_reap(&mut state.child, timeout);
            Err(RetainedHostError::DeliveryFailed {
                request_id,
                write_may_have_committed: true,
                cause: error.to_string(),
            })
        }
        Err(_) => {
            state.closed = true;
            terminate_and_reap(&mut state.child, timeout);
            Err(RetainedHostError::DeliveryTimeout {
                request_id,
                write_may_have_committed: true,
            })
        }
    }
}

fn terminate_and_reap(child: &mut Child, timeout: Duration) {
    let _ = child.kill();
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(Some(_) | None) | Err(_) => return,
        }
    }
}

fn read_response(reader: &mut impl Read, maximum: usize) -> Result<Response, RetainedHostError> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > maximum {
        return Err(RetainedHostError::FrameTooLarge {
            actual: length,
            maximum,
        });
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(Into::into)
}
