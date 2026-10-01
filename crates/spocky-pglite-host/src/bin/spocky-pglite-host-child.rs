//! Rust counterpart of `scripts/phase2/hub-embedded-retained-host.mjs`.
//!
//! Serves the retained `PGlite` child protocol on stdin and stdout with
//! `PgliteHost`, so the adapter in `spocky-hub-pilot` can start this binary
//! where it starts Node (`SPOCKY_NODE`). Frames are a 4-byte big-endian
//! length and UTF-8 JSON. Arguments are `<package root> <migrations root>
//! <data directory> <frame maximum>`, optionally preceded by the adapter
//! path the parent passes to Node, which is ignored. `SPOCKY_PGLITE_CACHE_DIR`
//! enables the Wasmtime compilation cache.

use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use spocky_pglite_host::host::{PgliteHost, PgliteHostConfig, PgliteHostError, SqlStatement};
use spocky_pglite_host::pglite::EngineOptions;
use spocky_pglite_host::values::IpcValue;

enum Input {
    Request(Value),
    TooLarge,
    End,
}

struct Output {
    stdout: Mutex<io::Stdout>,
    maximum: usize,
}

impl Output {
    fn reply(&self, id: &Value, outcome: Result<String, Value>) {
        let text = match outcome {
            Ok(result) => format!(r#"{{"id":{id},"ok":true,"result":{result}}}"#),
            Err(error) => format!(r#"{{"id":{id},"ok":false,"error":{error}}}"#),
        };
        if text.len() > self.maximum {
            let fallback = json!({
                "code": "FRAME_TOO_LARGE",
                "message": format!("response exceeds {}", self.maximum),
            });
            self.write(format!(r#"{{"id":{id},"ok":false,"error":{fallback}}}"#).as_bytes());
            return;
        }
        self.write(text.as_bytes());
    }

    fn write(&self, bytes: &[u8]) {
        let Ok(length) = u32::try_from(bytes.len()) else {
            return;
        };
        if let Ok(mut stdout) = self.stdout.lock() {
            // A closed pipe means the parent is gone; the read side ends us.
            let _ = stdout
                .write_all(&length.to_be_bytes())
                .and_then(|()| stdout.write_all(bytes))
                .and_then(|()| stdout.flush());
        }
    }
}

/// `errorPayload` for an error that carries no database fields.
fn plain_error(code: &str, message: &str) -> Value {
    json!({
        "code": code,
        "message": message,
        "details": {
            "name": "Error",
            "severity": null,
            "detail": null,
            "hint": null,
            "position": null,
            "schema": null,
            "table": null,
            "column": null,
            "constraint": null,
        },
    })
}

fn host_error(error: PgliteHostError) -> Value {
    match error {
        PgliteHostError::Remote {
            code,
            message,
            details,
        } => json!({ "code": code, "message": message, "details": details }),
        PgliteHostError::Closed => plain_error("REMOTE_ERROR", "PGlite is closed"),
        PgliteHostError::Startup(message) => plain_error("REMOTE_ERROR", &message),
        PgliteHostError::Timeout => plain_error("REMOTE_ERROR", "PGlite host request timed out"),
    }
}

/// `Number(request.id)` as the Node child writes it back.
fn request_id(value: &Value) -> Value {
    match value {
        Value::Number(_) => value.clone(),
        Value::String(text) => text
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or(Value::Null, Value::Number),
        _ => Value::Null,
    }
}

/// `decodeParams`: a missing list is empty; an unknown tag is
/// `INVALID_VALUE`.
fn decode_params(value: Option<&Value>) -> Result<Vec<IpcValue>, Value> {
    let Some(Value::Array(items)) = value else {
        return Ok(Vec::new());
    };
    items
        .iter()
        .map(|item| {
            serde_json::from_value::<IpcValue>(item.clone()).map_err(|error| {
                let kind = item.get("type").and_then(Value::as_str);
                let known = matches!(
                    kind,
                    Some(
                        "null" | "boolean" | "string" | "binary" | "timestamp" | "numeric" | "json"
                    )
                );
                if known {
                    plain_error("INVALID_VALUE", &error.to_string())
                } else {
                    plain_error(
                        "INVALID_VALUE",
                        &format!("unknown value type: {}", kind.unwrap_or("undefined")),
                    )
                }
            })
        })
        .collect()
}

fn to_json(value: &impl serde::Serialize) -> Result<String, Value> {
    serde_json::to_string(value).map_err(|error| plain_error("REMOTE_ERROR", &error.to_string()))
}

fn sql_of(request: &Value) -> &str {
    request.get("sql").and_then(Value::as_str).unwrap_or("")
}

fn statements_of(request: &Value) -> Result<Vec<SqlStatement>, Value> {
    let Some(Value::Array(items)) = request.get("statements") else {
        return Ok(Vec::new());
    };
    items
        .iter()
        .map(|item| {
            Ok(SqlStatement::new(
                sql_of(item),
                decode_params(item.get("params"))?,
            ))
        })
        .collect()
}

struct Child {
    host: PgliteHost,
    output: Arc<Output>,
    stalled: Arc<AtomicBool>,
    fail_close: bool,
}

impl Child {
    fn handle(&mut self, request: &Value) {
        let id = request_id(request.get("id").unwrap_or(&Value::Null));
        let outcome = match request.get("operation").and_then(Value::as_str) {
            Some("query") => decode_params(request.get("params")).and_then(|params| {
                self.host
                    .query(sql_of(request), &params)
                    .map_err(host_error)
                    .and_then(|result| to_json(&result))
            }),
            Some("execute") => self
                .host
                .execute(sql_of(request))
                .map(|()| "null".to_owned())
                .map_err(host_error),
            Some("transaction") => statements_of(request).and_then(|statements| {
                self.host
                    .transaction(&statements)
                    .map_err(host_error)
                    .and_then(|results| to_json(&results))
            }),
            Some("migrate") => self
                .host
                .migrate()
                .map_err(host_error)
                .and_then(|outcome| to_json(&outcome)),
            Some("crash") => process::exit(86),
            Some("executeThenCrash") => match self.host.execute(sql_of(request)) {
                Ok(()) => process::exit(87),
                Err(error) => Err(host_error(error)),
            },
            Some("delay") => {
                let milliseconds = request
                    .get("milliseconds")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                thread::sleep(Duration::from_millis(milliseconds));
                Ok("null".to_owned())
            }
            Some("stallReads") => {
                self.stalled.store(true, Ordering::SeqCst);
                Ok("null".to_owned())
            }
            Some("failClose") => {
                self.fail_close = true;
                Ok("null".to_owned())
            }
            Some("close") => self.close_and_reply(&id),
            other => Err(plain_error(
                "UNKNOWN_OPERATION",
                &format!("unknown operation: {}", other.unwrap_or("undefined")),
            )),
        };
        self.output.reply(&id, outcome);
    }

    /// `closeAndReply`: the reply carries the close result, then the child
    /// exits with 0, or 1 when close failed.
    fn close_and_reply(&self, id: &Value) -> ! {
        let failure = match self.host.close() {
            Err(PgliteHostError::Remote {
                code,
                message,
                details,
            }) => Some(json!({ "code": code, "message": message, "details": details })),
            Err(error) => Some(plain_error("CLOSE_FAILED", &error.to_string())),
            Ok(()) if self.fail_close => Some(plain_error(
                "CLOSE_FAILED",
                "injected close failure after durable close",
            )),
            Ok(()) => None,
        };
        let code = i32::from(failure.is_some());
        self.output
            .reply(id, failure.map_or_else(|| Ok("null".to_owned()), Err));
        process::exit(code);
    }

    fn shutdown(&self, code: i32) -> ! {
        let _ = self.host.close();
        process::exit(code);
    }
}

/// `process.stdin.pause()`: once `stallReads` has run, nothing more is
/// read from stdin.
fn park_while_stalled(stalled: &AtomicBool) {
    while stalled.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_secs(1));
    }
}

/// Reads frames until end of input. A frame over the maximum is answered
/// at once and ends reading; invalid JSON is answered at once, as in the
/// Node child, which replies before queued operations finish.
fn read_frames(output: &Output, stalled: &AtomicBool, inputs: &mpsc::Sender<Input>) {
    let mut stdin = io::stdin().lock();
    loop {
        park_while_stalled(stalled);
        let mut header = [0_u8; 4];
        if stdin.read_exact(&mut header).is_err() {
            let _ = inputs.send(Input::End);
            return;
        }
        // The read of this header may have started before stallReads ran;
        // the body must not be read after it.
        park_while_stalled(stalled);
        let length = u32::from_be_bytes(header) as usize;
        if length > output.maximum {
            output.write(
                json!({
                    "id": 0,
                    "ok": false,
                    "error": {
                        "code": "FRAME_TOO_LARGE",
                        "message": format!("frame {length} exceeds {}", output.maximum),
                    },
                })
                .to_string()
                .as_bytes(),
            );
            let _ = inputs.send(Input::TooLarge);
            return;
        }
        let mut body = vec![0_u8; length];
        if stdin.read_exact(&mut body).is_err() {
            let _ = inputs.send(Input::End);
            return;
        }
        match serde_json::from_slice::<Value>(&body) {
            Ok(request) => {
                if inputs.send(Input::Request(request)).is_err() {
                    return;
                }
            }
            Err(error) => output.reply(
                &json!(0),
                Err(json!({
                    "code": "INVALID_JSON",
                    "message": error.to_string(),
                    "details": {
                        "name": "SyntaxError",
                        "severity": null,
                        "detail": null,
                        "hint": null,
                        "position": null,
                        "schema": null,
                        "table": null,
                        "column": null,
                        "constraint": null,
                    },
                })),
            ),
        }
    }
}

fn identity(host: &PgliteHost) -> Value {
    let identity = host.identity();
    let executable = std::env::current_exe().unwrap_or_default();
    let executable_sha256 = std::fs::read(&executable)
        .map(|bytes| {
            Sha256::digest(bytes)
                .iter()
                .fold(String::new(), |mut text, byte| {
                    let _ = write!(text, "{byte:02x}");
                    text
                })
        })
        .unwrap_or_default();
    // The adapter's identity fields are named for Node; this host reports
    // its own runtime in them.
    json!({
        "nodeVersion": format!(
            "spocky-pglite-host {} ({} {})",
            env!("CARGO_PKG_VERSION"),
            identity.runtime,
            identity.runtime_version
        ),
        "nodeExecutable": executable.display().to_string(),
        "nodeExecutableSha256": executable_sha256,
        "os": identity.os,
        "arch": identity.arch,
        "package": identity.package,
        "packageVersion": identity.package_version,
        "packageDependencies": identity.package_dependencies,
        "adapterDependencies": ["spocky-pglite-host"],
    })
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let positional = match arguments.len() {
        4 => &arguments[..],
        5 => &arguments[1..],
        _ => {
            eprintln!(
                "package root, migrations root, data directory, and frame maximum are required"
            );
            process::exit(1);
        }
    };
    let maximum = match positional[3].parse::<usize>() {
        Ok(maximum) if (1024..=(1 << 53)).contains(&maximum) => maximum,
        _ => {
            eprintln!("invalid frame maximum");
            process::exit(1);
        }
    };
    let output = Arc::new(Output {
        stdout: Mutex::new(io::stdout()),
        maximum,
    });
    let config = PgliteHostConfig {
        package_root: PathBuf::from(&positional[0]),
        migrations_root: PathBuf::from(&positional[1]),
        data_directory: PathBuf::from(&positional[2]),
        engine: EngineOptions {
            cache_directory: std::env::var_os("SPOCKY_PGLITE_CACHE_DIR").map(PathBuf::from),
            ..EngineOptions::default()
        },
        request_timeout: None,
    };
    let host = match PgliteHost::open(&config) {
        Ok(host) => host,
        Err(error) => {
            output.reply(&json!(0), Err(host_error(error)));
            process::exit(1);
        }
    };
    output.reply(&json!(0), Ok(identity(&host).to_string()));

    let stalled = Arc::new(AtomicBool::new(false));
    let (inputs, received) = mpsc::channel();
    {
        let output = Arc::clone(&output);
        let stalled = Arc::clone(&stalled);
        thread::spawn(move || read_frames(&output, &stalled, &inputs));
    }
    let mut child = Child {
        host,
        output,
        stalled,
        fail_close: false,
    };
    for input in received {
        match input {
            Input::Request(request) => child.handle(&request),
            Input::TooLarge => child.shutdown(1),
            Input::End => child.shutdown(0),
        }
    }
    child.shutdown(0);
}
