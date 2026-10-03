//! Structured log sink for daemon modules.
//!
//! The pinned daemon logs through pino: `logger.info({ fields }, "message")`.
//! Modules here take a [`Logger`] with the same shape so each log call of the
//! baseline has one call here.

use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

/// Pino level numbers.
const TRACE: u8 = 10;
const INFO: u8 = 30;
const WARN: u8 = 40;
const ERROR: u8 = 50;
const FATAL: u8 = 60;

/// `REDACT_PATHS` of the daemon's `logger.ts`, in pino's path syntax. The
/// logger is created with `redact: { paths, remove: true }`: a matching key is
/// left out of the record.
pub const REDACT_PATHS: [&str; 12] = [
    "authorization",
    "Authorization",
    "headers.authorization",
    "headers.Authorization",
    "req.headers.authorization",
    "req.headers.Authorization",
    "[\"sec-websocket-protocol\"]",
    "Sec-WebSocket-Protocol",
    "headers[\"sec-websocket-protocol\"]",
    "headers.Sec-WebSocket-Protocol",
    "req.headers[\"sec-websocket-protocol\"]",
    "req.headers.Sec-WebSocket-Protocol",
];

/// The keys of a pino redact path: names separated by dots, and quoted names
/// in square brackets (`headers["sec-websocket-protocol"]`).
fn path_keys(path: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = path;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('[') {
            let quote = after.chars().next().filter(|c| matches!(c, '"' | '\''));
            let quote = quote.expect("a bracket in a redact path holds a quoted key");
            let inner = &after[1..];
            let end = inner.find(quote).expect("the quoted key is closed");
            keys.push(inner[..end].to_owned());
            rest = inner[end + 1..]
                .strip_prefix(']')
                .expect("the bracket is closed");
        } else {
            let end = rest.find(['.', '[']).unwrap_or(rest.len());
            keys.push(rest[..end].to_owned());
            rest = &rest[end..];
        }
        rest = rest.strip_prefix('.').unwrap_or(rest);
    }
    keys
}

/// Removes the keys `REDACT_PATHS` names from a list of record pairs. A path
/// goes through objects only: an array, a string or null on the way ends it,
/// and a key matches by exact spelling.
fn redact(pairs: &mut Vec<(String, Value)>) {
    let paths: Vec<Vec<String>> = REDACT_PATHS.iter().map(|path| path_keys(path)).collect();
    redact_pairs(pairs, &paths.iter().map(Vec::as_slice).collect::<Vec<_>>());
}

fn redact_pairs(pairs: &mut Vec<(String, Value)>, paths: &[&[String]]) {
    pairs.retain(|(key, _)| !paths.iter().any(|path| path.len() == 1 && path[0] == *key));
    for (key, value) in pairs.iter_mut() {
        let deeper: Vec<&[String]> = paths
            .iter()
            .filter(|path| path.len() > 1 && path[0] == *key)
            .map(|path| &path[1..])
            .collect();
        if deeper.is_empty() {
            continue;
        }
        if let Value::Object(object) = value {
            let mut inner: Vec<(String, Value)> = std::mem::take(object).into_iter().collect();
            redact_pairs(&mut inner, &deeper);
            *object = inner.into_iter().collect();
        }
    }
}

/// The level `createRootLogger` gives pino: `config.file?.level ?? config.console.level`
/// of `resolveLogConfig`. Each is the section's own level, else the global
/// `log.level`, else `info`; a file section exists only when one is configured and
/// the caller allows files (`options.file !== false`, which the worker passes as
/// false).
#[must_use]
pub fn resolve_level(
    global: Option<Level>,
    console: Option<Level>,
    file: Option<Option<Level>>,
    files_allowed: bool,
) -> Level {
    if files_allowed && let Some(file) = file {
        return file.or(global).unwrap_or(Level::Info);
    }
    console.or(global).unwrap_or(Level::Info)
}

/// A pino level name, for the threshold a destination writes at
/// (`level` in the pino options; the daemon's default is `info`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl Level {
    /// The level a config file names: `trace`, `debug`, `info`, `warn`,
    /// `error` or `fatal` (`LogLevel` in `logger.ts`).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "trace" => Some(Self::Trace),
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" => Some(Self::Warn),
            "error" => Some(Self::Error),
            "fatal" => Some(Self::Fatal),
            _ => None,
        }
    }

    const fn number(self) -> u8 {
        match self {
            Self::Trace => TRACE,
            Self::Debug => 20,
            Self::Info => INFO,
            Self::Warn => WARN,
            Self::Error => ERROR,
            Self::Fatal => FATAL,
        }
    }
}

/// An error as pino's standard `err` serializer writes it: `type` (the
/// constructor name), `message`, `stack`, then the error's other enumerable
/// own properties in the order the runtime defined them (`code`, `errno`,
/// `syscall`, ... for a system error).
#[derive(Debug, Clone, PartialEq)]
pub struct LogError {
    pub name: String,
    pub message: String,
    pub stack: String,
    pub props: Vec<(String, Value)>,
}

impl LogError {
    /// The `err` object of a record.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut object = Map::new();
        object.insert("type".into(), Value::from(self.name.as_str()));
        object.insert("message".into(), Value::from(self.message.as_str()));
        object.insert("stack".into(), Value::from(self.stack.as_str()));
        for (key, value) in &self.props {
            object.insert(key.clone(), value.clone());
        }
        Value::Object(object)
    }
}

/// A log destination. `fields` are the first pino argument, `message` the second.
pub trait Logger: Send + Sync {
    /// `logger.trace(...)`. Dropped by a destination that does not write at
    /// trace, which is every one by default (the daemon's level is `info`).
    fn trace(&self, _fields: &[(&str, &str)], _message: &str) {}
    /// `logger.debug(...)`: dropped below the `debug` level, like `trace`.
    fn debug(&self, _fields: &[(&str, &str)], _message: &str) {}
    fn info(&self, fields: &[(&str, &str)], message: &str);
    fn warn(&self, fields: &[(&str, &str)], message: &str);
    fn error(&self, fields: &[(&str, &str)], message: &str);
    fn fatal(&self, fields: &[(&str, &str)], message: &str);
    /// `logger.fatal({ err, ...fields }, message)`: `err` is written as an object
    /// by a destination that can; the default has only the message to offer.
    fn fatal_with_error(&self, err: &LogError, fields: &[(&str, &str)], message: &str) {
        let mut all = vec![("err", err.message.as_str())];
        all.extend_from_slice(fields);
        self.fatal(&all, message);
    }
}

/// Discards every record.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullLogger;

impl Logger for NullLogger {
    fn info(&self, _fields: &[(&str, &str)], _message: &str) {}
    fn warn(&self, _fields: &[(&str, &str)], _message: &str) {}
    fn error(&self, _fields: &[(&str, &str)], _message: &str) {}
    fn fatal(&self, _fields: &[(&str, &str)], _message: &str) {}
}

/// Writes one pino-shaped JSON object per line: `level`, `time`, `pid`,
/// `hostname`, the bindings, the fields, then `msg`.
pub struct JsonLineLogger<W: Write + Send> {
    sink: Mutex<W>,
    hostname: String,
    bindings: Vec<(String, String)>,
    value_bindings: Vec<(String, Value)>,
    min_level: u8,
}

impl<W: Write + Send> JsonLineLogger<W> {
    #[must_use]
    pub fn new(sink: W, bindings: Vec<(String, String)>) -> Self {
        Self {
            sink: Mutex::new(sink),
            hostname: gethostname::gethostname().to_string_lossy().into_owned(),
            bindings,
            value_bindings: Vec::new(),
            min_level: INFO,
        }
    }

    /// The lowest level written; records below it are dropped, as pino's
    /// `level` option does.
    #[must_use]
    pub fn with_level(mut self, level: Level) -> Self {
        self.min_level = level.number();
        self
    }

    /// Bindings whose values are not strings, written after the string ones
    /// (`logger.child({ headers: { ... } })`).
    #[must_use]
    pub fn with_value_bindings(mut self, bindings: Vec<(String, Value)>) -> Self {
        self.value_bindings = bindings;
        self
    }

    /// `logger[level](fields, message)` with fields of any JSON type, in the
    /// order given.
    pub fn log_values(&self, level: Level, fields: &[(String, Value)], message: &str) {
        self.write_pairs(level.number(), None, fields.to_vec(), message);
    }

    fn write(&self, level: u8, err: Option<&LogError>, fields: &[(&str, &str)], message: &str) {
        let fields = fields
            .iter()
            .map(|(key, value)| ((*key).to_owned(), Value::from(*value)))
            .collect();
        self.write_pairs(level, err, fields, message);
    }

    /// One record as pino writes it: `level`, `time`, `pid`, `hostname`, the
    /// bindings, the fields, `msg`. A key that a binding and a field both
    /// carry is written twice, as pino does.
    fn write_pairs(
        &self,
        level: u8,
        err: Option<&LogError>,
        fields: Vec<(String, Value)>,
        message: &str,
    ) {
        if level < self.min_level {
            return;
        }
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        let mut pairs: Vec<(String, Value)> = vec![
            ("level".to_owned(), Value::from(level)),
            (
                "time".to_owned(),
                Value::from(u64::try_from(time).unwrap_or(0)),
            ),
            ("pid".to_owned(), Value::from(std::process::id())),
            ("hostname".to_owned(), Value::from(self.hostname.as_str())),
        ];
        // Redaction runs on the bindings (when pino creates the child) and on
        // the fields of each call, separately.
        let mut bindings: Vec<(String, Value)> = self
            .bindings
            .iter()
            .map(|(key, value)| (key.clone(), Value::from(value.as_str())))
            .collect();
        bindings.extend(self.value_bindings.iter().cloned());
        redact(&mut bindings);
        pairs.extend(bindings);
        let mut fields = fields;
        if let Some(err) = err {
            fields.insert(0, ("err".to_owned(), err.to_value()));
        }
        redact(&mut fields);
        pairs.extend(fields);
        pairs.push(("msg".to_owned(), Value::from(message)));
        let mut line = String::from("{");
        for (index, (key, value)) in pairs.iter().enumerate() {
            if index > 0 {
                line.push(',');
            }
            line.push_str(&Value::from(key.as_str()).to_string());
            line.push(':');
            line.push_str(&value.to_string());
        }
        line.push('}');
        if let Ok(mut sink) = self.sink.lock() {
            let _ = writeln!(sink, "{line}");
        }
    }
}

impl<W: Write + Send> Logger for JsonLineLogger<W> {
    fn trace(&self, fields: &[(&str, &str)], message: &str) {
        self.write(TRACE, None, fields, message);
    }
    fn debug(&self, fields: &[(&str, &str)], message: &str) {
        self.write(Level::Debug.number(), None, fields, message);
    }
    fn info(&self, fields: &[(&str, &str)], message: &str) {
        self.write(INFO, None, fields, message);
    }
    fn warn(&self, fields: &[(&str, &str)], message: &str) {
        self.write(WARN, None, fields, message);
    }
    fn error(&self, fields: &[(&str, &str)], message: &str) {
        self.write(ERROR, None, fields, message);
    }
    fn fatal(&self, fields: &[(&str, &str)], message: &str) {
        self.write(FATAL, None, fields, message);
    }
    fn fatal_with_error(&self, err: &LogError, fields: &[(&str, &str)], message: &str) {
        self.write(FATAL, Some(err), fields, message);
    }
}

#[cfg(test)]
pub mod testing {
    use super::Logger;
    use std::sync::Mutex;

    type Record = (&'static str, Vec<(String, String)>, String);

    /// Records `(level, fields, message)` for assertions.
    #[derive(Default)]
    pub struct RecordingLogger {
        pub records: Mutex<Vec<Record>>,
    }

    impl RecordingLogger {
        fn push(&self, level: &'static str, fields: &[(&str, &str)], message: &str) {
            self.records.lock().unwrap().push((
                level,
                fields
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
                message.to_owned(),
            ));
        }

        /// # Panics
        ///
        /// Panics if the record lock is poisoned.
        pub fn messages(&self) -> Vec<(&'static str, String)> {
            self.records
                .lock()
                .unwrap()
                .iter()
                .map(|(level, _, message)| (*level, message.clone()))
                .collect()
        }
    }

    impl Logger for RecordingLogger {
        fn trace(&self, fields: &[(&str, &str)], message: &str) {
            self.push("trace", fields, message);
        }
        fn debug(&self, fields: &[(&str, &str)], message: &str) {
            self.push("debug", fields, message);
        }
        fn info(&self, fields: &[(&str, &str)], message: &str) {
            self.push("info", fields, message);
        }
        fn warn(&self, fields: &[(&str, &str)], message: &str) {
            self.push("warn", fields, message);
        }
        fn error(&self, fields: &[(&str, &str)], message: &str) {
            self.push("error", fields, message);
        }
        fn fatal(&self, fields: &[(&str, &str)], message: &str) {
            self.push("fatal", fields, message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writes_one_json_line_with_bindings_fields_and_message_last() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let logger = JsonLineLogger::new(
            Shared(Arc::clone(&bytes)),
            vec![("module".to_owned(), "server-id".to_owned())],
        );
        logger.warn(&[("error", "boom")], "Failed to persist");
        let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(text.ends_with('\n'));
        let value: Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(value["level"], 40);
        assert_eq!(value["module"], "server-id");
        assert_eq!(value["error"], "boom");
        assert_eq!(value["msg"], "Failed to persist");
        let keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys.first().map(String::as_str), Some("level"));
        assert_eq!(keys.last().map(String::as_str), Some("msg"));
    }

    #[test]
    fn a_record_has_pinos_base_fields_in_order() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let logger = JsonLineLogger::new(
            Shared(Arc::clone(&bytes)),
            vec![("daemonVersion".to_owned(), "0.10.0".to_owned())],
        );
        logger.info(&[("elapsed", "750ms")], "Agent storage initialized");
        let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        let value: Value = serde_json::from_str(text.trim_end()).unwrap();
        let keys: Vec<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "level",
                "time",
                "pid",
                "hostname",
                "daemonVersion",
                "elapsed",
                "msg"
            ]
        );
        assert_eq!(
            value["hostname"],
            gethostname::gethostname().to_string_lossy().as_ref()
        );
    }

    #[test]
    fn an_error_is_written_as_pinos_err_object() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let logger = JsonLineLogger::new(Shared(Arc::clone(&bytes)), vec![]);
        let err = LogError {
            name: "Error".to_owned(),
            message: "listen EADDRINUSE: address already in use 127.0.0.1:1".to_owned(),
            stack: "Error: x\n    at y".to_owned(),
            props: vec![
                ("code".to_owned(), Value::from("EADDRINUSE")),
                ("errno".to_owned(), Value::from(-48)),
            ],
        };
        logger.fatal_with_error(&err, &[], "Daemon failed to start listening");
        let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        let value: Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(value["level"], 60);
        let keys: Vec<_> = value["err"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["type", "message", "stack", "code", "errno"]);
        assert_eq!(value["err"]["errno"], -48);
        let record_keys: Vec<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            record_keys,
            ["level", "time", "pid", "hostname", "err", "msg"]
        );
    }

    fn lines_of(logger_level: Option<Level>, emit: impl Fn(&JsonLineLogger<Shared>)) -> Vec<Value> {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let mut logger = JsonLineLogger::new(Shared(Arc::clone(&bytes)), vec![]);
        if let Some(level) = logger_level {
            logger = logger.with_level(level);
        }
        emit(&logger);
        let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn trace_is_below_the_default_level_and_written_when_asked_for() {
        let emit = |logger: &JsonLineLogger<Shared>| {
            logger.trace(&[("a", "1")], "trace");
            logger.info(&[], "info");
            logger.warn(&[], "warn");
        };
        let default = lines_of(None, emit);
        assert_eq!(
            default
                .iter()
                .map(|r| r["msg"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["info", "warn"]
        );
        let trace = lines_of(Some(Level::Trace), emit);
        assert_eq!(trace[0]["level"], 10);
        assert_eq!(trace[0]["msg"], "trace");
        assert_eq!(trace.len(), 3);
        let warn = lines_of(Some(Level::Warn), emit);
        assert_eq!(
            warn.iter()
                .map(|r| r["msg"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["warn"]
        );
    }

    #[test]
    fn a_key_in_a_binding_and_a_field_is_written_twice() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let logger = JsonLineLogger::new(
            Shared(Arc::clone(&bytes)),
            vec![("module".to_owned(), "bootstrap".to_owned())],
        );
        logger.info(&[("module", "daemon-keypair")], "Saved daemon keypair");
        let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(
            text.contains(r#""module":"bootstrap","module":"daemon-keypair","msg""#),
            "{text}"
        );
    }

    #[test]
    fn fields_of_any_json_type_keep_their_order() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let logger = JsonLineLogger::new(Shared(Arc::clone(&bytes)), vec![]);
        logger.log_values(
            Level::Info,
            &[
                (
                    "z".to_owned(),
                    serde_json::json!({"b": 1, "a": [true, null]}),
                ),
                ("a".to_owned(), serde_json::json!(2.5)),
            ],
            "typed",
        );
        let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(
            text.contains(r#""z":{"b":1,"a":[true,null]},"a":2.5,"msg":"typed""#),
            "{text}"
        );
    }

    #[test]
    fn redact_paths_parse_like_pinos() {
        let keys: Vec<Vec<String>> = REDACT_PATHS.iter().map(|path| path_keys(path)).collect();
        let text: Vec<String> = keys.iter().map(|keys| keys.join("/")).collect();
        assert_eq!(
            text,
            [
                "authorization",
                "Authorization",
                "headers/authorization",
                "headers/Authorization",
                "req/headers/authorization",
                "req/headers/Authorization",
                "sec-websocket-protocol",
                "Sec-WebSocket-Protocol",
                "headers/sec-websocket-protocol",
                "headers/Sec-WebSocket-Protocol",
                "req/headers/sec-websocket-protocol",
                "req/headers/Sec-WebSocket-Protocol",
            ]
        );
    }

    #[test]
    fn redaction_removes_keys_from_fields_and_bindings() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let logger = JsonLineLogger::new(
            Shared(Arc::clone(&bytes)),
            vec![
                ("authorization".to_owned(), "b".to_owned()),
                ("name".to_owned(), "x".to_owned()),
            ],
        )
        .with_value_bindings(vec![(
            "headers".to_owned(),
            serde_json::json!({"authorization": "c", "keep": 1}),
        )]);
        logger.log_values(
            Level::Info,
            &[
                ("Authorization".to_owned(), serde_json::json!("d")),
                (
                    "req".to_owned(),
                    serde_json::json!({"headers": {"sec-websocket-protocol": "p", "k": 2}}),
                ),
                (
                    "headers".to_owned(),
                    serde_json::json!([{"authorization": "kept"}]),
                ),
            ],
            "m",
        );
        let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(
            text.contains(r#""name":"x","headers":{"keep":1},"req":{"headers":{"k":2}},"headers":[{"authorization":"kept"}],"msg":"m""#),
            "{text}"
        );
    }

    #[test]
    fn the_logger_level_is_the_file_level_else_the_console_level() {
        use Level::{Debug, Error, Info, Trace, Warn};
        assert_eq!(resolve_level(None, None, None, true), Info);
        assert_eq!(resolve_level(Some(Warn), None, None, true), Warn);
        assert_eq!(resolve_level(Some(Warn), Some(Debug), None, true), Debug);
        assert_eq!(
            resolve_level(Some(Warn), Some(Debug), Some(None), true),
            Warn
        );
        assert_eq!(resolve_level(None, Some(Debug), Some(None), true), Info);
        assert_eq!(
            resolve_level(None, Some(Debug), Some(Some(Error)), true),
            Error
        );
        // The worker passes file: false, so a configured file section is ignored.
        assert_eq!(
            resolve_level(Some(Trace), Some(Debug), Some(Some(Error)), false),
            Debug
        );
        assert_eq!(Level::from_name("warn"), Some(Warn));
        assert_eq!(Level::from_name("silent"), None);
    }

    #[test]
    fn debug_sits_between_trace_and_info() {
        let emit = |logger: &JsonLineLogger<Shared>| {
            logger.trace(&[], "trace");
            logger.debug(&[], "debug");
            logger.info(&[], "info");
        };
        let at_debug = lines_of(Some(Level::Debug), emit);
        assert_eq!(
            at_debug
                .iter()
                .map(|r| r["msg"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["debug", "info"]
        );
        assert_eq!(at_debug[0]["level"], 20);
        let at_info = lines_of(None, emit);
        assert_eq!(
            at_info
                .iter()
                .map(|r| r["msg"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["info"]
        );
    }

    #[test]
    fn levels_match_pino() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let logger = JsonLineLogger::new(Shared(Arc::clone(&bytes)), vec![]);
        logger.info(&[], "a");
        logger.error(&[], "b");
        logger.fatal(&[], "c");
        let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        let levels: Vec<i64> = text
            .lines()
            .map(|l| {
                serde_json::from_str::<Value>(l).unwrap()["level"]
                    .as_i64()
                    .unwrap()
            })
            .collect();
        assert_eq!(levels, [30, 50, 60]);
    }
}
