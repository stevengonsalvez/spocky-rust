//! Shared harness for the encrypted-channel differential tests.
//!
//! An endpoint runs one channel and executes JSON operations against it,
//! returning the entries each operation produced. [`NodeEndpoint`] drives
//! the pinned TypeScript channel through `scripts/phase3/e2ee-driver.mjs`;
//! [`RustEndpoint`] drives the Rust port. Both render entries exactly as
//! the driver's `JSON.stringify` does, so transcripts compare as strings.
#![allow(dead_code)]

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, VecDeque},
    ffi::OsString,
    fmt::Write as _,
    io::{BufRead, BufReader, Write as _},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    rc::Rc,
    sync::{
        OnceLock,
        mpsc::{Receiver, RecvTimeoutError, channel},
    },
    thread,
    time::Duration,
};

use rand_core::{RngCore, impls};
use serde_json::Value;
use spocky_crypto::{
    base64_js::base64_to_array_buffer,
    channel::{
        AppSend, ChannelControl, ChannelError, ChannelEvents, ChannelOptions, ChannelState, Data,
        EncryptedChannel, NORMAL_CLOSURE_CODE, NORMAL_CLOSURE_REASON, SendId, SendStatus,
        Transport, TransportError, TransportMessage, base64_encrypted_wire_byte_length,
        max_base64_encrypted_plaintext_byte_length,
    },
    js_json::{self, JsonValue},
    js_string::{JsString, decode_utf8_fatal, decode_utf8_lossy, json_quote, utf16},
    key_pair_from_secret,
};

pub const PASEO_COMMIT: &str = "5de45e208690b0efc51c59a585ae9729325a9204";
pub const NODE_VERSION: &str = "v22.20.0";
/// SHA-256 of the node 22.20.0 darwin-x64 binary, as pinned in
/// `scripts/phase3/pins.sh` (`P3_NODE_BINARY_SHA256`).
pub const NODE_BINARY_SHA256: &str =
    "1fdf607e61ae32be3f77e4e3cf1257c677aeb694e409f99586084839f61ad931";

/// SHA-256 of every file the driver loads, from the pinned commit and the
/// pinned lockfile install (tweetnacl 1.0.3, base64-js 1.5.1).
pub const PINNED_DIGESTS: [(&str, &str); 7] = [
    (
        "encrypted-channel.ts",
        "2b0d31520917d24993644fc30f065be4d46d3517475f8821388a2263ced3e6a6",
    ),
    (
        "crypto.ts",
        "309d5cb94ceb236ced93a188d0a0bc3b42a0e347815b4dbe6679374d810db845",
    ),
    (
        "base64.ts",
        "74d69461af9727aa3fb70b37ee50ae54e467e1eebf26fb0356453557094137e7",
    ),
    (
        "tweetnacl/nacl-fast.js",
        "6bcd37a3b20dce913f82d4b23e4e2b661058b4b953df8a3f8c45d56ac4f72447",
    ),
    (
        "tweetnacl/package.json",
        "929dc634f6c254c5881612db9a4c52e2ef16cc1b3e3da3343b9e31e6cf037448",
    ),
    (
        "base64-js/index.js",
        "829eadd8a1a441d25be0cb93b00e16a0d0c20fd294db95d8f2ed87e6954b7182",
    ),
    (
        "base64-js/package.json",
        "9758f3ab8c45e07bb9a368e32f9a8b3729623bbf47cbbb205b32d674ab2a91f0",
    ),
];

static NODE_DIGEST: OnceLock<String> = OnceLock::new();

const OPERATION_TIMEOUT: Duration = Duration::from_secs(60);

/// Locations of the pinned node binary, relay sources, and dependencies.
pub struct Pinned {
    node: OsString,
    driver: PathBuf,
    relay_src: PathBuf,
    node_modules: PathBuf,
}

/// The pinned inputs, or `None` when skipping was requested explicitly.
///
/// `SPOCKY_PINNED_NODE` names node 22.20.0. The relay sources come from
/// `PASEO_REFERENCE_ROOT`, else the `paseo-rewrite` sibling of the main
/// checkout. Dependencies come from `SPOCKY_PASEO_NODE_MODULES`, else the
/// pinned build root of `scripts/phase3/build-original.sh`. Every loaded
/// file is digest-checked by [`NodeEndpoint::spawn`].
pub fn pinned() -> Option<Pinned> {
    let Some(node) = std::env::var_os("SPOCKY_PINNED_NODE") else {
        if std::env::var("SPOCKY_ALLOW_SKIP").as_deref() == Ok("1") {
            eprintln!("SKIPPED by SPOCKY_ALLOW_SKIP: e2ee differential not run");
            return None;
        }
        panic!("set SPOCKY_PINNED_NODE (or SPOCKY_ALLOW_SKIP=1)");
    };
    // Every scenario calls `pinned`, and `SPOCKY_PINNED_NODE` is fixed for
    // the process, so the binary is hashed once.
    assert_eq!(
        NODE_DIGEST.get_or_init(|| sha256_file(Path::new(&node))),
        NODE_BINARY_SHA256,
        "SPOCKY_PINNED_NODE is not the pinned node {NODE_VERSION} binary"
    );
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let reference = std::env::var_os("PASEO_REFERENCE_ROOT").map_or_else(
        || {
            let output = Command::new("git")
                .arg("-C")
                .arg(manifest)
                .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
                .output()
                .expect("run git");
            assert!(output.status.success(), "git rev-parse failed");
            let common = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
            common
                .parent()
                .and_then(Path::parent)
                .expect("main checkout has a parent")
                .join("paseo-rewrite")
        },
        PathBuf::from,
    );
    let node_modules = std::env::var_os("SPOCKY_PASEO_NODE_MODULES").map_or_else(
        || {
            PathBuf::from(format!(
                "/private/tmp/spocky-targets/p3_slice_harness/paseo-original-{PASEO_COMMIT}/node_modules"
            ))
        },
        PathBuf::from,
    );
    Some(Pinned {
        node,
        driver: manifest.join("../../scripts/phase3/e2ee-driver.mjs"),
        relay_src: reference.join("packages/relay/src"),
        node_modules,
    })
}

/// SHA-256 of a file from the platform `shasum` or `sha256sum`.
fn sha256_file(path: &Path) -> String {
    let attempts: [(&str, &[&str]); 2] = [("shasum", &["-a", "256"]), ("sha256sum", &[])];
    for (program, args) in attempts {
        let Ok(output) = Command::new(program).args(args).arg(path).output() else {
            continue;
        };
        if output.status.success() {
            let text = String::from_utf8(output.stdout).expect("digest output is UTF-8");
            return text
                .split_whitespace()
                .next()
                .expect("digest output has a digest")
                .to_owned();
        }
    }
    panic!("no shasum or sha256sum for {}", path.display());
}

/// One channel endpoint that executes operations.
pub trait Endpoint {
    fn op(&mut self, op: &Value) -> Vec<String>;
}

/// The pinned TypeScript channel in a node child process.
pub struct NodeEndpoint {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl NodeEndpoint {
    pub fn spawn(pinned: &Pinned) -> Self {
        let mut child = Command::new(&pinned.node)
            .arg(&pinned.driver)
            .arg(&pinned.relay_src)
            .arg(&pinned.node_modules)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn pinned node");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let (sender, lines) = channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut endpoint = Self {
            child,
            stdin,
            lines,
        };
        let ready: Value =
            serde_json::from_str(&endpoint.next_line()).expect("driver ready line is JSON");
        assert_eq!(ready["ready"], true);
        assert_eq!(ready["node"], NODE_VERSION, "pinned node version");
        for (name, digest) in PINNED_DIGESTS {
            assert_eq!(ready["digests"][name], digest, "digest of pinned {name}");
        }
        endpoint
    }

    fn next_line(&mut self) -> String {
        match self.lines.recv_timeout(OPERATION_TIMEOUT) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => panic!("node driver timed out"),
            Err(RecvTimeoutError::Disconnected) => panic!("node driver exited"),
        }
    }
}

impl Endpoint for NodeEndpoint {
    fn op(&mut self, op: &Value) -> Vec<String> {
        writeln!(self.stdin, "{op}").expect("write operation");
        self.stdin.flush().expect("flush operation");
        let mut entries = Vec::new();
        loop {
            let line = self.next_line();
            if line == "." {
                return entries;
            }
            entries.push(line);
        }
    }
}

impl Drop for NodeEndpoint {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The xorshift32 byte stream the driver installs with `nacl.setPRNG`.
pub struct XorShift(pub u32);

impl RngCore for XorShift {
    fn next_u32(&mut self) -> u32 {
        impls::next_u32_via_fill(self)
    }

    fn next_u64(&mut self) -> u64 {
        impls::next_u64_via_fill(self)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for byte in dest {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            *byte = self.0.to_le_bytes()[0];
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

#[derive(Clone)]
enum Mode {
    Sync,
    Fail(String),
    Pending,
}

fn mode(value: &Value) -> Mode {
    match value["kind"].as_str().expect("mode kind") {
        "sync" => Mode::Sync,
        "throw" | "reject" => Mode::Fail(value["message"].as_str().unwrap().to_owned()),
        "pending" => Mode::Pending,
        other => panic!("unknown send mode {other}"),
    }
}

struct Shared {
    log: Vec<String>,
    mode: Mode,
    queue: VecDeque<Mode>,
    close_failure: Option<String>,
    next_id: u64,
    on_open_send: Option<String>,
    /// While a batch is delivered, sends that would settle at once report
    /// `Pending` and settle after the batch, as a send the original awaits
    /// continues only after the rest of the task.
    deferred: Option<Vec<u64>>,
}

type SharedRef = Rc<RefCell<Shared>>;

pub fn quote(text: &str) -> String {
    json_quote(&utf16(text))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

pub fn unhex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2), "even hex");
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).expect("hex"))
        .collect()
}

fn data_fields(data: &Data) -> String {
    match data {
        Data::Text(text) => format!(r#""text":{}"#, quote(text)),
        Data::Binary(bytes) => format!(r#""binary":"{}""#, hex(bytes)),
    }
}

fn error_text(error: &ChannelError) -> String {
    json_quote(&error.message())
}

struct LogTransport(SharedRef);

impl Transport for LogTransport {
    fn send(&mut self, data: Data) -> SendStatus {
        let mut shared = self.0.borrow_mut();
        let id = shared.next_id;
        shared.next_id += 1;
        let entry = format!(r#"{{"t":"wire","id":{id},{}}}"#, data_fields(&data));
        shared.log.push(entry);
        let mode = shared
            .queue
            .pop_front()
            .unwrap_or_else(|| shared.mode.clone());
        match mode {
            Mode::Sync => match &mut shared.deferred {
                Some(ids) => {
                    ids.push(id);
                    SendStatus::Pending(SendId(id))
                }
                None => SendStatus::Sent,
            },
            Mode::Fail(message) => SendStatus::Failed(TransportError(message)),
            Mode::Pending => SendStatus::Pending(SendId(id)),
        }
    }

    fn close(&mut self, code: u16, reason: &str) -> Result<(), TransportError> {
        let mut shared = self.0.borrow_mut();
        let entry = format!(
            r#"{{"t":"transport-close","code":{code},"reason":{}}}"#,
            quote(reason)
        );
        shared.log.push(entry);
        shared
            .close_failure
            .clone()
            .map_or(Ok(()), |message| Err(TransportError(message)))
    }
}

struct LogEvents(SharedRef);

impl LogEvents {
    fn push(&self, entry: String) {
        self.0.borrow_mut().log.push(entry);
    }
}

impl ChannelEvents for LogEvents {
    fn on_open(&mut self, channel: &mut dyn ChannelControl) {
        self.push(r#"{"t":"open"}"#.to_owned());
        let reentrant = self.0.borrow().on_open_send.clone();
        if let Some(text) = reentrant {
            let _ = channel.send(Data::Text(text));
        }
    }

    fn on_message(&mut self, _channel: &mut dyn ChannelControl, data: Data) {
        self.push(format!(r#"{{"t":"message",{}}}"#, data_fields(&data)));
    }

    fn on_close(&mut self, _channel: &mut dyn ChannelControl, code: u16, reason: &str) {
        self.push(format!(
            r#"{{"t":"close","code":{code},"reason":{}}}"#,
            quote(reason)
        ));
    }

    fn on_error(&mut self, _channel: &mut dyn ChannelControl, error: &ChannelError) {
        self.push(format!(
            r#"{{"t":"error","message":{}}}"#,
            error_text(error)
        ));
    }
}

type RustChannel = EncryptedChannel<LogTransport, LogEvents>;

/// The Rust port, executing the same operations as the driver.
pub struct RustEndpoint {
    shared: SharedRef,
    channel: Option<RustChannel>,
    handshake_logged: bool,
    handles: HashMap<SendId, u64>,
    next_handle: u64,
}

impl Default for RustEndpoint {
    fn default() -> Self {
        Self {
            shared: Rc::new(RefCell::new(Shared {
                log: Vec::new(),
                mode: Mode::Sync,
                queue: VecDeque::new(),
                close_failure: None,
                next_id: 1,
                on_open_send: None,
                deferred: None,
            })),
            channel: None,
            handshake_logged: false,
            handles: HashMap::new(),
            next_handle: 1,
        }
    }
}

fn op_data(op: &Value) -> Data {
    op["binary"].as_str().map_or_else(
        || Data::Text(op["text"].as_str().expect("text payload").to_owned()),
        |binary| Data::Binary(unhex(binary)),
    )
}

fn seed(op: &Value) -> XorShift {
    XorShift(u32::try_from(op["seed"].as_u64().expect("seed")).expect("u32 seed"))
}

fn secret(text: &str) -> [u8; 32] {
    unhex(text).try_into().expect("32-byte key")
}

fn describe(value: Option<&JsonValue>) -> JsString {
    match value {
        None => utf16("undefined"),
        Some(JsonValue::Null) => utf16("null"),
        Some(JsonValue::Array) => utf16("array"),
        Some(JsonValue::String(text)) => {
            let mut output = utf16("string:");
            output.extend_from_slice(text);
            output
        }
        Some(JsonValue::Bool(value)) => utf16(&format!("boolean:{value}")),
        Some(JsonValue::Number) => utf16("number"),
        Some(JsonValue::Object(_)) => utf16("object"),
    }
}

fn json_summary(value: &JsonValue) -> JsString {
    if !value.is_record() {
        return describe(Some(value));
    }
    let mut summary = utf16("record|");
    summary.extend(describe(value.get("type")));
    summary.extend(utf16("|"));
    summary.extend(describe(value.get("key")));
    summary.extend(utf16("|"));
    match value.get("capabilities") {
        Some(capabilities) if capabilities.is_record() => {
            summary.extend(utf16("record:"));
            summary.extend(describe(capabilities.get("binaryCiphertext")));
        }
        other => summary.extend(describe(other)),
    }
    summary
}

impl RustEndpoint {
    fn log(&self, entry: String) {
        self.shared.borrow_mut().log.push(entry);
    }

    fn channel(&mut self) -> &mut RustChannel {
        self.channel.as_mut().expect("channel exists")
    }

    fn log_send(&mut self, handle: u64, result: Result<(), ChannelError>) {
        let entry = match result {
            Ok(()) => format!(r#"{{"t":"sent","handle":{handle},"ok":true}}"#),
            Err(error) => format!(
                r#"{{"t":"sent","handle":{handle},"error":{}}}"#,
                error_text(&error)
            ),
        };
        self.log(entry);
    }

    #[allow(clippy::too_many_lines)]
    fn run(&mut self, op: &Value) {
        let transport = LogTransport(Rc::clone(&self.shared));
        let events = LogEvents(Rc::clone(&self.shared));
        match op["op"].as_str().expect("op name") {
            "client" => {
                let key = op["daemonKey"].as_str().unwrap();
                match EncryptedChannel::client_with_rng(transport, key, events, seed(op)) {
                    Ok(channel) => {
                        self.channel = Some(channel);
                        self.log(r#"{"t":"created","ok":true}"#.to_owned());
                    }
                    Err(error) => {
                        self.log(format!(
                            r#"{{"t":"created","error":{}}}"#,
                            error_text(&error)
                        ));
                    }
                }
            }
            "daemon" => {
                let key_pair = key_pair_from_secret(secret(op["secret"].as_str().unwrap()));
                self.channel = Some(EncryptedChannel::daemon_with_rng(
                    transport,
                    key_pair,
                    events,
                    seed(op),
                ));
            }
            "raw" => {
                let options = ChannelOptions {
                    daemon_key_pair: op["daemonSecret"]
                        .as_str()
                        .map(|text| key_pair_from_secret(secret(text))),
                    binary_ciphertext: op["binary"].as_bool().unwrap_or(false),
                };
                let mut channel = EncryptedChannel::new(
                    transport,
                    secret(op["shared"].as_str().unwrap()),
                    events,
                    options,
                    seed(op),
                );
                if op["open"].as_bool() == Some(true) {
                    channel.set_state(ChannelState::Open);
                }
                self.channel = Some(channel);
            }
            "deliver" => {
                let data = op_data(op);
                let is_binary = op["isBinary"]
                    .as_bool()
                    .unwrap_or(matches!(data, Data::Binary(_)));
                self.channel()
                    .handle_message(TransportMessage { data, is_binary });
            }
            "batch" => {
                self.shared.borrow_mut().deferred = Some(Vec::new());
                for frame in op["frames"].as_array().unwrap() {
                    let data = op_data(frame);
                    let is_binary = frame["isBinary"]
                        .as_bool()
                        .unwrap_or(matches!(data, Data::Binary(_)));
                    self.channel()
                        .handle_message(TransportMessage { data, is_binary });
                }
                let ids = self.shared.borrow_mut().deferred.take().unwrap();
                for id in ids {
                    let id = SendId(id);
                    if let Some(result) = self.channel().settle_send(id, Ok(())) {
                        let handle = self.handles.remove(&id).expect("application send");
                        self.log_send(handle, result);
                    }
                }
            }
            "send" => {
                let handle = self.next_handle;
                self.next_handle += 1;
                match self.channel().send(op_data(op)) {
                    AppSend::Settled(result) => self.log_send(handle, result),
                    AppSend::Pending(id) => {
                        self.handles.insert(id, handle);
                    }
                }
            }
            "mode" => self.shared.borrow_mut().mode = mode(op),
            "queue" => {
                self.shared.borrow_mut().queue =
                    op["modes"].as_array().unwrap().iter().map(mode).collect();
            }
            "close-mode" => {
                self.shared.borrow_mut().close_failure =
                    (op["kind"] == "throw").then(|| op["message"].as_str().unwrap().to_owned());
            }
            "settle" => {
                let id = SendId(op["id"].as_u64().unwrap());
                let result = op["error"]
                    .as_str()
                    .map_or(Ok(()), |message| Err(TransportError(message.to_owned())));
                if let Some(result) = self.channel().settle_send(id, result) {
                    let handle = self.handles.remove(&id).expect("application send");
                    self.log_send(handle, result);
                }
            }
            "tick" => self.channel().retry_tick(),
            "transport-close" => {
                let code = u16::try_from(op["code"].as_u64().unwrap()).unwrap();
                let reason = op["reason"].as_str().unwrap().to_owned();
                self.channel().handle_close(code, &reason);
            }
            "transport-error" => {
                let message = op["message"].as_str().unwrap().to_owned();
                self.channel().handle_error(TransportError(message));
            }
            "close" => {
                let code = op["code"]
                    .as_u64()
                    .map_or(NORMAL_CLOSURE_CODE, |code| u16::try_from(code).unwrap());
                let reason = op["reason"]
                    .as_str()
                    .unwrap_or(NORMAL_CLOSURE_REASON)
                    .to_owned();
                let entry = match self.channel().close(code, &reason) {
                    Ok(()) => r#"{"t":"closed","ok":true}"#.to_owned(),
                    Err(error) => format!(r#"{{"t":"closed","error":{}}}"#, quote(&error.0)),
                };
                self.log(entry);
            }
            "set-state" => {
                let state = match op["state"].as_str().unwrap() {
                    "connecting" => ChannelState::Connecting,
                    "handshaking" => ChannelState::Handshaking,
                    "open" => ChannelState::Open,
                    "closed" => ChannelState::Closed,
                    other => panic!("unknown state {other}"),
                };
                self.channel().set_state(state);
            }
            "is-open" => {
                let value = self.channel().is_open();
                self.log(format!(r#"{{"t":"is-open","value":{value}}}"#));
            }
            "wire-length" => {
                let value = self.channel().outbound_wire_byte_length(&op_data(op));
                self.log(format!(r#"{{"t":"wire-length","value":{value}}}"#));
            }
            "on-open-send" => {
                self.shared.borrow_mut().on_open_send =
                    Some(op["text"].as_str().unwrap().to_owned());
            }
            "probe-json" => {
                let entry = js_json::parse(op["text"].as_str().unwrap()).map_or_else(
                    || r#"{"t":"json","ok":false}"#.to_owned(),
                    |value| {
                        format!(
                            r#"{{"t":"json","ok":true,"summary":{}}}"#,
                            json_quote(&json_summary(&value))
                        )
                    },
                );
                self.log(entry);
            }
            "probe-json-error" => {
                let message = js_json::parse_detailed(op["text"].as_str().unwrap())
                    .err()
                    .and_then(|error| error.unexpected_token_message().map(json_quote))
                    .unwrap_or_else(|| "null".to_owned());
                self.log(format!(r#"{{"t":"json-error","message":{message}}}"#));
            }
            "probe-decode" => {
                let input = unhex(op["binary"].as_str().unwrap());
                let fatal = decode_utf8_fatal(&input)
                    .map_or_else(|_| "null".to_owned(), |text| quote(&text));
                let lossy = quote(&decode_utf8_lossy(&input));
                self.log(format!(
                    r#"{{"t":"decode","lossy":{lossy},"fatal":{fatal}}}"#
                ));
            }
            "probe-base64" => {
                let entry = match base64_to_array_buffer(op["text"].as_str().unwrap()) {
                    Ok(bytes) => format!(r#"{{"t":"base64","bytes":"{}"}}"#, hex(&bytes)),
                    Err(error) => {
                        format!(r#"{{"t":"base64","error":{}}}"#, quote(&error.to_string()))
                    }
                };
                self.log(entry);
            }
            "probe-wire-sizes" => {
                let value = op["value"].as_u64().unwrap();
                self.log(format!(
                    r#"{{"t":"wire-sizes","encrypted":{},"plaintext":{}}}"#,
                    base64_encrypted_wire_byte_length(value),
                    max_base64_encrypted_plaintext_byte_length(value)
                ));
            }
            other => panic!("unknown operation {other}"),
        }
    }
}

impl Endpoint for RustEndpoint {
    fn op(&mut self, op: &Value) -> Vec<String> {
        self.run(op);
        if !self.handshake_logged
            && let Some(result) = self
                .channel
                .as_ref()
                .and_then(EncryptedChannel::handshake_result)
        {
            self.handshake_logged = true;
            let entry = match result {
                Ok(()) => r#"{"t":"handshake","ok":true}"#.to_owned(),
                Err(error) => format!(r#"{{"t":"handshake","error":{}}}"#, error_text(error)),
            };
            self.log(entry);
        }
        std::mem::take(&mut self.shared.borrow_mut().log)
    }
}

/// Writes raw transcripts when `SPOCKY_E2EE_EVIDENCE` names a directory.
pub fn record(name: &str, side: &str, transcript: &[(String, Vec<String>)]) {
    let Some(directory) = std::env::var_os("SPOCKY_E2EE_EVIDENCE") else {
        return;
    };
    let mut text = String::new();
    for (op, entries) in transcript {
        let _ = writeln!(text, "> {op}");
        for entry in entries {
            let _ = writeln!(text, "{entry}");
        }
    }
    let path = Path::new(&directory).join(format!("{name}.{side}.txt"));
    std::fs::write(&path, text).expect("write evidence transcript");
}

/// Writes a pair transcript when `SPOCKY_E2EE_EVIDENCE` names a directory.
pub fn record_lines(name: &str, lines: &[String]) {
    let Some(directory) = std::env::var_os("SPOCKY_E2EE_EVIDENCE") else {
        return;
    };
    let mut text = lines.join("\n");
    text.push('\n');
    std::fs::write(Path::new(&directory).join(format!("{name}.txt")), text)
        .expect("write evidence transcript");
}

/// Runs `ops` on the pinned TypeScript channel and on the Rust port and
/// asserts identical entries after every operation. Returns the shared
/// transcript, or `None` when skipping was requested.
pub fn differential(name: &str, ops: &[Value]) -> Option<Vec<Vec<String>>> {
    let pinned = pinned()?;
    let mut node = NodeEndpoint::spawn(&pinned);
    let mut rust = RustEndpoint::default();
    let mut node_transcript = Vec::new();
    let mut rust_transcript = Vec::new();
    for op in ops {
        node_transcript.push((op.to_string(), node.op(op)));
        rust_transcript.push((op.to_string(), rust.op(op)));
    }
    record(name, "node", &node_transcript);
    record(name, "rust", &rust_transcript);
    for ((op, expected), (_, actual)) in node_transcript.iter().zip(&rust_transcript) {
        assert_eq!(actual, expected, "{name}: entries differ after {op}");
    }
    Some(
        node_transcript
            .into_iter()
            .map(|(_, entries)| entries)
            .collect(),
    )
}

/// Which implementation runs each side of a pair.
#[derive(Clone, Copy, Debug)]
pub enum Side {
    Node,
    Rust,
}

fn endpoint(side: Side, pinned: &Pinned) -> Box<dyn Endpoint> {
    match side {
        Side::Node => Box::new(NodeEndpoint::spawn(pinned)),
        Side::Rust => Box::new(RustEndpoint::default()),
    }
}

/// Runs a client and a daemon against each other. Each step sends an
/// operation to `"client"` or `"daemon"`; every frame either side puts on
/// the wire is then delivered to the other, in order, until both are
/// quiet. Returns the labelled transcript.
pub fn pair(client: Side, daemon: Side, pinned: &Pinned, steps: &[(&str, Value)]) -> Vec<String> {
    let mut endpoints: BTreeMap<&str, Box<dyn Endpoint>> = BTreeMap::new();
    endpoints.insert("client", endpoint(client, pinned));
    endpoints.insert("daemon", endpoint(daemon, pinned));
    let mut transcript = Vec::new();
    let mut queue: VecDeque<(&str, Value)> = steps.iter().cloned().collect();
    while let Some((target, op)) = queue.pop_front() {
        let entries = endpoints.get_mut(target).expect("known side").op(&op);
        let peer = if target == "client" {
            "daemon"
        } else {
            "client"
        };
        let mut deliveries = Vec::new();
        for entry in entries {
            let parsed: Value = serde_json::from_str(&entry).expect("entry is JSON");
            if parsed["t"] == "wire" {
                let mut deliver = serde_json::json!({ "op": "deliver" });
                if let Some(text) = parsed.get("text") {
                    deliver["text"] = text.clone();
                } else {
                    deliver["binary"] = parsed["binary"].clone();
                }
                deliveries.push((peer, deliver));
            }
            transcript.push(format!("{target} {op} {entry}"));
        }
        for delivery in deliveries.into_iter().rev() {
            queue.push_front(delivery);
        }
    }
    transcript
}
