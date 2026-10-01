//! Scripted loopback Responses API stub.
//!
//! Every `POST .../responses` request consumes the next scripted reply in
//! order. Every request, scripted or not, is appended to a JSON Lines record
//! with its method, path, headers in arrival order, and raw body. Requests
//! after the script is exhausted, and every other path, get a fixed error so
//! an unexpected extra request is visible both in the record and to codex.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Ports the slice must never bind or target.
pub const FORBIDDEN_PORTS: [u16; 2] = [6767, 6768];

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
}

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
        if !(100..=599).contains(&reply.status) {
            return Err(format!("reply {index} has invalid status {}", reply.status));
        }
        match (&reply.json, reply.events.is_empty()) {
            (Some(_), false) => {
                return Err(format!("reply {index} has both events and json"));
            }
            (None, true) => return Err(format!("reply {index} has neither events nor json")),
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

/// Binds a loopback listener on an ephemeral port that is never forbidden.
///
/// # Errors
///
/// Returns an I/O error when binding fails.
pub fn bind_loopback() -> io::Result<TcpListener> {
    loop {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
        if !FORBIDDEN_PORTS.contains(&listener.local_addr()?.port()) {
            return Ok(listener);
        }
    }
}

/// Serves the script on `listener` until the process is stopped.
///
/// # Errors
///
/// Returns an error when the script is invalid or accepting fails.
pub fn serve(listener: &TcpListener, script: Script, record: File) -> Result<(), StubError> {
    validate_script(&script).map_err(StubError::Malformed)?;
    let state = Arc::new(Mutex::new(State {
        script,
        next_reply: 0,
        next_seq: 0,
        record,
    }));
    for stream in listener.incoming() {
        let stream = stream?;
        let state = Arc::clone(&state);
        thread::spawn(move || {
            if let Err(error) = handle(stream, &state) {
                eprintln!("spocky-responses-stub: {error}");
            }
        });
    }
    Ok(())
}

fn handle(stream: TcpStream, state: &Mutex<State>) -> Result<(), StubError> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let (method, path, headers) = read_head(&mut reader)?;
    let body = read_body(&mut reader, &headers)?;
    let body = String::from_utf8(body)
        .map_err(|_| StubError::Malformed("request body is not UTF-8".into()))?;

    let (status, content_type, payload) = {
        let mut state = state
            .lock()
            .map_err(|_| StubError::Malformed("stub state lock poisoned".into()))?;
        let scripted = if method == "POST" && path.ends_with("/responses") {
            let index = state.next_reply;
            if index < state.script.responses.len() {
                state.next_reply += 1;
                Some(index)
            } else {
                None
            }
        } else {
            None
        };
        let entry = RecordedRequest {
            seq: state.next_seq,
            method,
            path,
            headers,
            body,
            scripted,
        };
        state.next_seq += 1;
        let mut line =
            serde_json::to_vec(&entry).map_err(|error| StubError::Malformed(error.to_string()))?;
        line.push(b'\n');
        state.record.write_all(&line)?;
        state.record.flush()?;
        reply_for(&state.script, scripted)
    };

    let mut stream = stream;
    let head = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        reason(status),
        payload.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&payload)?;
    stream.flush()?;
    Ok(())
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
                },
                ScriptedReply {
                    status: 500,
                    events: vec![],
                    json: Some(json!({"error": {"message": "boom"}})),
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
    fn bound_port_is_never_forbidden() {
        let listener = bind_loopback().unwrap();
        assert!(!FORBIDDEN_PORTS.contains(&listener.local_addr().unwrap().port()));
    }
}
