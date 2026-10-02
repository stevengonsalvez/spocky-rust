//! Scripted loopback Responses API stub.
//!
//! Every `POST /v1/responses` request consumes the next scripted reply in
//! order. Every connection, scripted or not, is appended to a JSON Lines record
//! with its method, path, headers in arrival order, raw body, and any parse
//! error. Requests after the script is exhausted, every other path, malformed
//! or timed-out requests, and connections over the limit get a fixed error so
//! an unexpected extra request is visible both in the record and to codex.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Ports the slice must never bind or target.
pub const FORBIDDEN_PORTS: [u16; 2] = [6767, 6768];

/// The only path that consumes scripted replies.
pub const RESPONSES_PATH: &str = "/v1/responses";

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// One scripted HTTP reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScriptedReply {
    pub status: u16,
    /// Server-sent events, written in order as `event: <type>` plus
    /// `data: <compact json>`. Each event must carry a string `type`.
    #[serde(default)]
    pub events: Vec<Value>,
    /// Plain JSON body, used instead of events for error replies.
    #[serde(default)]
    pub json: Option<Value>,
    /// Keep the stream open this long after the events, with no
    /// `content-length`, so the turn stays in flight (for cancel mid-turn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_ms: Option<u64>,
}

/// Longest a scripted reply may hold its stream open.
pub const MAX_HOLD_MS: u64 = 600_000;

/// The ordered replies for one gate run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Script {
    pub responses: Vec<ScriptedReply>,
}

/// One recorded request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedRequest {
    pub seq: u64,
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// Index into the script that answered, or `None` for an unscripted reply.
    pub scripted: Option<usize>,
    /// Why the request could not be read or was refused, if it was.
    #[serde(default)]
    pub error: Option<String>,
}

/// Bounds on one stub instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Per-connection read timeout.
    pub read_timeout: Duration,
    /// Connections handled at once; extra ones are recorded and refused.
    pub max_connections: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            read_timeout: Duration::from_secs(30),
            max_connections: 32,
        }
    }
}

#[derive(Debug)]
struct State {
    script: Script,
    next_reply: usize,
    next_seq: u64,
    record: File,
}

/// Errors from parsing or answering one request.
#[derive(Debug)]
pub enum StubError {
    Io(io::Error),
    Malformed(String),
}

impl std::fmt::Display for StubError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Malformed(message) => write!(formatter, "malformed request: {message}"),
        }
    }
}

impl std::error::Error for StubError {}

impl From<io::Error> for StubError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Validates a script before serving it.
///
/// # Errors
///
/// Returns a message when a reply has an invalid status, mixes events with a
/// JSON body, has neither, or holds an event without a string `type`.
pub fn validate_script(script: &Script) -> Result<(), String> {
    for (index, reply) in script.responses.iter().enumerate() {
        if reply.hold_ms.is_some_and(|hold| hold > MAX_HOLD_MS) {
            return Err(format!("reply {index} holds longer than {MAX_HOLD_MS} ms"));
        }
        if !(100..=599).contains(&reply.status) {
            return Err(format!("reply {index} has invalid status {}", reply.status));
        }
        match (&reply.json, reply.events.is_empty()) {
            (Some(_), false) => {
                return Err(format!("reply {index} has both events and json"));
            }
            (None, true) => return Err(format!("reply {index} has neither events nor json")),
            (Some(_), _) if reply.hold_ms.is_some() => {
                return Err(format!(
                    "reply {index} holds a json body; only event streams hold"
                ));
            }
            _ => {}
        }
        for (event_index, event) in reply.events.iter().enumerate() {
            if !matches!(event.get("type"), Some(Value::String(_))) {
                return Err(format!(
                    "reply {index} event {event_index} has no string type"
                ));
            }
        }
    }
    Ok(())
}

/// Renders scripted events as a server-sent event stream.
#[must_use]
pub fn render_events(events: &[Value]) -> Vec<u8> {
    let mut body = Vec::new();
    for event in events {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        body.extend_from_slice(format!("event: {kind}\ndata: {event}\n\n").as_bytes());
    }
    body
}

/// Returns the first bound value whose port is not forbidden, binding again
/// while the port source keeps handing out forbidden ports.
///
/// # Errors
///
/// Returns the first binding error.
pub fn first_allowed<T>(mut bind: impl FnMut() -> io::Result<(T, u16)>) -> io::Result<T> {
    loop {
        let (bound, port) = bind()?;
        if !FORBIDDEN_PORTS.contains(&port) {
            return Ok(bound);
        }
    }
}

/// Binds a loopback listener on an ephemeral port that is never forbidden.
///
/// # Errors
///
/// Returns an I/O error when binding fails.
pub fn bind_loopback() -> io::Result<TcpListener> {
    first_allowed(|| {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
        let port = listener.local_addr()?.port();
        Ok((listener, port))
    })
}

/// Serves the script on `listener` with default limits until stopped.
///
/// # Errors
///
/// Returns an error when the script is invalid.
pub fn serve(listener: &TcpListener, script: Script, record: File) -> Result<(), StubError> {
    serve_with(listener, script, record, Limits::default())
}

/// Serves the script on `listener` with explicit limits until stopped.
/// Accept errors are logged and serving continues.
///
/// # Errors
///
/// Returns an error when the script is invalid.
pub fn serve_with(
    listener: &TcpListener,
    script: Script,
    record: File,
    limits: Limits,
) -> Result<(), StubError> {
    validate_script(&script).map_err(StubError::Malformed)?;
    let state = Arc::new(Mutex::new(State {
        script,
        next_reply: 0,
        next_seq: 0,
        record,
    }));
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("spocky-responses-stub: accept failed: {error}");
                thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        if active.load(Ordering::SeqCst) >= limits.max_connections {
            let refused = Incoming::failed(None, "connection limit reached".into());
            if let Err(error) = answer(stream, &state, refused) {
                eprintln!("spocky-responses-stub: {error}");
            }
            continue;
        }
        active.fetch_add(1, Ordering::SeqCst);
        let state = Arc::clone(&state);
        let active = Arc::clone(&active);
        thread::spawn(move || {
            let incoming = match stream.set_read_timeout(Some(limits.read_timeout)) {
                Ok(()) => read_incoming(&stream),
                Err(error) => Incoming::failed(None, format!("set read timeout: {error}")),
            };
            if let Err(error) = answer(stream, &state, incoming) {
                eprintln!("spocky-responses-stub: {error}");
            }
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Ok(())
}

/// A parsed request, or what was read before parsing failed.
struct Incoming {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
    error: Option<String>,
}

impl Incoming {
    fn failed(head: Option<Head>, error: String) -> Self {
        let (method, path, headers) = head.unwrap_or_default();
        Self {
            method,
            path,
            headers,
            body: String::new(),
            error: Some(error),
        }
    }
}

fn read_incoming(stream: &TcpStream) -> Incoming {
    let mut reader = match stream.try_clone() {
        Ok(clone) => BufReader::new(clone),
        Err(error) => return Incoming::failed(None, error.to_string()),
    };
    let head = match read_head(&mut reader) {
        Ok(head) => head,
        Err(error) => return Incoming::failed(None, error.to_string()),
    };
    let body = match read_body(&mut reader, &head.2) {
        Ok(body) => body,
        Err(error) => return Incoming::failed(Some(head), error.to_string()),
    };
    match String::from_utf8(body) {
        Ok(body) => {
            let (method, path, headers) = head;
            Incoming {
                method,
                path,
                headers,
                body,
                error: None,
            }
        }
        Err(_) => Incoming::failed(Some(head), "request body is not UTF-8".into()),
    }
}

/// Records one request and writes its reply.
fn answer(
    mut stream: TcpStream,
    state: &Mutex<State>,
    incoming: Incoming,
) -> Result<(), StubError> {
    let (status, content_type, payload, hold_ms) = {
        let mut state = state
            .lock()
            .map_err(|_| StubError::Malformed("stub state lock poisoned".into()))?;
        let scripted = if incoming.error.is_none()
            && incoming.method == "POST"
            && incoming.path == RESPONSES_PATH
            && state.next_reply < state.script.responses.len()
        {
            let index = state.next_reply;
            state.next_reply = index
                .checked_add(1)
                .ok_or_else(|| StubError::Malformed("reply index overflow".into()))?;
            Some(index)
        } else {
            None
        };
        let seq = state.next_seq;
        state.next_seq = seq
            .checked_add(1)
            .ok_or_else(|| StubError::Malformed("request sequence overflow".into()))?;
        let failure = incoming.error.clone();
        let entry = RecordedRequest {
            seq,
            method: incoming.method,
            path: incoming.path,
            headers: incoming.headers,
            body: incoming.body,
            scripted,
            error: incoming.error,
        };
        let mut line =
            serde_json::to_vec(&entry).map_err(|error| StubError::Malformed(error.to_string()))?;
        line.push(b'\n');
        state.record.write_all(&line)?;
        state.record.flush()?;
        let hold_ms = scripted.and_then(|index| state.script.responses[index].hold_ms);
        let (status, content_type, payload) = match failure {
            Some(message) => failure_reply(&message),
            None => reply_for(&state.script, scripted),
        };
        (status, content_type, payload, hold_ms)
    };
    let length = if hold_ms.is_some() {
        String::new()
    } else {
        format!("content-length: {}\r\n", payload.len())
    };
    let head = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\n{length}connection: close\r\n\r\n",
        reason(status),
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&payload)?;
    stream.flush()?;
    if let Some(hold) = hold_ms {
        // The client cancels by dropping the connection; the stub process is
        // stopped at side end, so this never outlives the side.
        thread::sleep(Duration::from_millis(hold.min(MAX_HOLD_MS)));
    }
    Ok(())
}

fn failure_reply(message: &str) -> (u16, &'static str, Vec<u8>) {
    let status = if message == "connection limit reached" {
        503
    } else {
        400
    };
    let body = serde_json::json!({
        "error": {
            "message": format!("spocky-responses-stub: {message}"),
            "type": "invalid_request_error"
        }
    });
    (status, "application/json", body.to_string().into_bytes())
}

fn reply_for(script: &Script, scripted: Option<usize>) -> (u16, &'static str, Vec<u8>) {
    match scripted.map(|index| &script.responses[index]) {
        Some(ScriptedReply {
            status,
            json: Some(json),
            ..
        }) => (*status, "application/json", json.to_string().into_bytes()),
        Some(reply) => (reply.status, "text/event-stream", render_events(&reply.events)),
        None => (
            404,
            "application/json",
            br#"{"error":{"message":"spocky-responses-stub: unscripted request","type":"invalid_request_error"}}"#
                .to_vec(),
        ),
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

type Head = (String, String, Vec<(String, String)>);

fn read_head(reader: &mut impl BufRead) -> Result<Head, StubError> {
    let mut total = 0;
    let mut request_line = String::new();
    total += reader.read_line(&mut request_line)?;
    let mut parts = request_line.trim_end_matches(['\r', '\n']).split(' ');
    let (Some(method), Some(path), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(StubError::Malformed(format!(
            "request line {request_line:?}"
        )));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(StubError::Malformed(format!("version {version:?}")));
    }
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        total += read;
        if read == 0 {
            return Err(StubError::Malformed(
                "headers ended without a blank line".into(),
            ));
        }
        if total > MAX_HEADER_BYTES {
            return Err(StubError::Malformed("headers exceed 64 KiB".into()));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(StubError::Malformed(format!("header line {line:?}")));
        };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    Ok((method.to_owned(), path.to_owned(), headers))
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn read_body(
    reader: &mut impl BufRead,
    headers: &[(String, String)],
) -> Result<Vec<u8>, StubError> {
    if let Some(encoding) = header(headers, "content-encoding") {
        return Err(StubError::Malformed(format!(
            "unsupported content-encoding {encoding:?}"
        )));
    }
    if header(headers, "transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        return read_chunked(reader);
    }
    let length = match header(headers, "content-length") {
        None => 0,
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| StubError::Malformed(format!("content-length {value:?}")))?,
    };
    if length > MAX_BODY_BYTES {
        return Err(StubError::Malformed("body exceeds 16 MiB".into()));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(body)
}

fn read_chunked(reader: &mut impl BufRead) -> Result<Vec<u8>, StubError> {
    let mut body = Vec::new();
    loop {
        let mut size_line = String::new();
        reader.read_line(&mut size_line)?;
        let size_text = size_line
            .trim_end_matches(['\r', '\n'])
            .split(';')
            .next()
            .unwrap_or_default();
        let size = usize::from_str_radix(size_text.trim(), 16)
            .map_err(|_| StubError::Malformed(format!("chunk size {size_line:?}")))?;
        if body.len() + size > MAX_BODY_BYTES {
            return Err(StubError::Malformed("body exceeds 16 MiB".into()));
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader.read_exact(&mut body[start..])?;
        let mut terminator = String::new();
        reader.read_line(&mut terminator)?;
        if size == 0 {
            if !terminator.trim_end_matches(['\r', '\n']).is_empty() {
                return Err(StubError::Malformed(
                    "chunk trailers are unsupported".into(),
                ));
            }
            return Ok(body);
        }
        if terminator != "\r\n" {
            return Err(StubError::Malformed("chunk is not CRLF terminated".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Cursor, Read};

    #[test]
    fn renders_events_in_order_as_sse() {
        let events = [
            json!({"type": "response.created", "response": {"id": "resp_1"}}),
            json!({"type": "response.completed", "response": {"id": "resp_1"}}),
        ];
        assert_eq!(
            String::from_utf8(render_events(&events)).unwrap(),
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n\
             event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_1\"}}\n\n"
        );
    }

    #[test]
    fn rejects_invalid_scripts() {
        let reply = |status, events: Vec<Value>, json| ScriptedReply {
            status,
            events,
            json,
            hold_ms: None,
        };
        let cases = [
            reply(99, vec![json!({"type": "x"})], None),
            reply(200, vec![], None),
            reply(200, vec![json!({"type": "x"})], Some(json!({}))),
            reply(200, vec![json!({"kind": "x"})], None),
        ];
        for case in cases {
            assert!(
                validate_script(&Script {
                    responses: vec![case]
                })
                .is_err()
            );
        }
        assert!(
            validate_script(&Script {
                responses: vec![reply(500, vec![], Some(json!({"error": {}})))]
            })
            .is_ok()
        );
    }

    #[test]
    fn parses_content_length_and_chunked_bodies() {
        let mut fixed = Cursor::new(
            b"POST /v1/responses HTTP/1.1\r\nContent-Length: 4\r\nX-A: b\r\n\r\nbody".to_vec(),
        );
        let (method, path, headers) = read_head(&mut fixed).unwrap();
        assert_eq!((method.as_str(), path.as_str()), ("POST", "/v1/responses"));
        assert_eq!(
            headers,
            vec![
                ("content-length".to_owned(), "4".to_owned()),
                ("x-a".to_owned(), "b".to_owned())
            ]
        );
        assert_eq!(read_body(&mut fixed, &headers).unwrap(), b"body");

        let mut chunked = Cursor::new(
            b"POST /v1/responses HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n"
                .to_vec(),
        );
        let (_, _, headers) = read_head(&mut chunked).unwrap();
        assert_eq!(read_body(&mut chunked, &headers).unwrap(), b"abcde");
    }

    #[test]
    fn rejects_compressed_bodies() {
        let headers = vec![("content-encoding".to_owned(), "zstd".to_owned())];
        assert!(read_body(&mut Cursor::new(Vec::new()), &headers).is_err());
    }

    fn exchange(port: u16, request: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    fn start(limits: Limits, script: Script, tag: u32) -> (u16, std::path::PathBuf) {
        let directory =
            std::env::temp_dir().join(format!("spocky-stub-test-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let record_path = directory.join("record.jsonl");
        let listener = bind_loopback().unwrap();
        let port = listener.local_addr().unwrap().port();
        let record = File::create(&record_path).unwrap();
        thread::spawn(move || serve_with(&listener, script, record, limits));
        (port, record_path)
    }

    fn records(path: &std::path::Path) -> Vec<RecordedRequest> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn one_reply() -> Script {
        Script {
            responses: vec![ScriptedReply {
                status: 200,
                events: vec![json!({"type": "response.created"})],
                json: None,
                hold_ms: None,
            }],
        }
    }

    #[test]
    fn malformed_and_wrong_path_requests_are_recorded_and_unscripted() {
        let (port, path) = start(Limits::default(), one_reply(), line!());
        let malformed = exchange(port, "garbage\r\n\r\n");
        assert!(malformed.starts_with("HTTP/1.1 400 "));
        let wrong_path = exchange(
            port,
            "POST /x/v1/responses HTTP/1.1\r\ncontent-length: 0\r\n\r\n",
        );
        assert!(wrong_path.starts_with("HTTP/1.1 404 "));
        let recorded = records(&path);
        assert_eq!(recorded.len(), 2);
        assert!(
            recorded[0]
                .error
                .as_deref()
                .unwrap()
                .contains("request line")
        );
        assert_eq!(
            (recorded[1].path.as_str(), recorded[1].scripted),
            ("/x/v1/responses", None)
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn idle_connection_times_out_and_is_recorded() {
        let limits = Limits {
            read_timeout: Duration::from_millis(200),
            max_connections: 32,
        };
        let (port, path) = start(limits, one_reply(), line!());
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 400 "));
        let recorded = records(&path);
        assert_eq!(recorded.len(), 1);
        assert!(recorded[0].error.is_some());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn connections_over_the_limit_are_refused_and_recorded() {
        let limits = Limits {
            read_timeout: Duration::from_secs(30),
            max_connections: 0,
        };
        let (port, path) = start(limits, one_reply(), line!());
        // The refusal is written without reading, so send nothing that
        // would be left unread and turn the close into a reset.
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut refused = String::new();
        stream.read_to_string(&mut refused).unwrap();
        assert!(refused.starts_with("HTTP/1.1 503 "));
        let recorded = records(&path);
        assert_eq!(
            recorded[0].error.as_deref(),
            Some("connection limit reached")
        );
        assert_eq!(recorded[0].scripted, None);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn held_reply_streams_events_and_keeps_the_connection_open() {
        let script = Script {
            responses: vec![ScriptedReply {
                status: 200,
                events: vec![json!({"type": "response.created"})],
                json: None,
                hold_ms: Some(5_000),
            }],
        };
        let (port, path) = start(Limits::default(), script, line!());
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(b"POST /v1/responses HTTP/1.1\r\ncontent-length: 2\r\n\r\n{}")
            .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(1_500)))
            .unwrap();
        let mut received = Vec::new();
        let mut buffer = [0_u8; 4096];
        let still_open = loop {
            match stream.read(&mut buffer) {
                Ok(0) => break false,
                Ok(count) => received.extend_from_slice(&buffer[..count]),
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    break true;
                }
                Err(error) => panic!("{error}"),
            }
        };
        let text = String::from_utf8(received).unwrap();
        assert!(still_open, "held stream closed early");
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(!text.contains("content-length"));
        assert!(
            text.ends_with("event: response.created\ndata: {\"type\":\"response.created\"}\n\n")
        );
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn holds_are_bounded_and_only_for_event_streams() {
        let held = |events: Vec<Value>, json, hold_ms| Script {
            responses: vec![ScriptedReply {
                status: 200,
                events,
                json,
                hold_ms,
            }],
        };
        assert!(
            validate_script(&held(vec![json!({"type": "x"})], None, Some(MAX_HOLD_MS))).is_ok()
        );
        assert!(
            validate_script(&held(
                vec![json!({"type": "x"})],
                None,
                Some(MAX_HOLD_MS + 1)
            ))
            .is_err()
        );
        assert!(validate_script(&held(vec![], Some(json!({})), Some(1))).is_err());
    }

    #[test]
    fn serves_script_in_order_and_records_every_request() {
        let directory = std::env::temp_dir().join(format!(
            "spocky-stub-test-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let record_path = directory.join("record.jsonl");
        let script = Script {
            responses: vec![
                ScriptedReply {
                    status: 200,
                    events: vec![json!({"type": "response.created"})],
                    json: None,
                    hold_ms: None,
                },
                ScriptedReply {
                    status: 500,
                    events: vec![],
                    json: Some(json!({"error": {"message": "boom"}})),
                    hold_ms: None,
                },
            ],
        };
        let listener = bind_loopback().unwrap();
        let port = listener.local_addr().unwrap().port();
        let record = File::create(&record_path).unwrap();
        thread::spawn(move || serve(&listener, script, record));

        let request = "POST /v1/responses HTTP/1.1\r\ncontent-length: 2\r\n\r\n{}";
        let first = exchange(port, request);
        assert!(first.starts_with("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n"));
        assert!(
            first.ends_with("event: response.created\ndata: {\"type\":\"response.created\"}\n\n")
        );
        let second = exchange(port, request);
        assert!(second.starts_with("HTTP/1.1 500 "));
        assert!(second.ends_with(r#"{"error":{"message":"boom"}}"#));
        let models = exchange(port, "GET /v1/models HTTP/1.1\r\n\r\n");
        assert!(models.starts_with("HTTP/1.1 404 "));
        let exhausted = exchange(port, request);
        assert!(exhausted.starts_with("HTTP/1.1 404 "));

        let records: Vec<RecordedRequest> = std::fs::read_to_string(&record_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let summary: Vec<_> = records
            .iter()
            .map(|entry| {
                (
                    entry.seq,
                    entry.method.as_str(),
                    entry.path.as_str(),
                    entry.scripted,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (0, "POST", "/v1/responses", Some(0)),
                (1, "POST", "/v1/responses", Some(1)),
                (2, "GET", "/v1/models", None),
                (3, "POST", "/v1/responses", None),
            ]
        );
        assert_eq!(records[0].body, "{}");
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn forbidden_ports_from_the_source_are_skipped() {
        let mut ports = [6767, 6768, 6767, 41234].into_iter();
        let mut attempts = 0;
        let chosen = first_allowed(|| {
            attempts += 1;
            let port = ports.next().expect("source exhausted");
            Ok((port, port))
        })
        .unwrap();
        assert_eq!((chosen, attempts), (41234, 4));
    }
}
