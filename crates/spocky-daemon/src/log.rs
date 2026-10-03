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
    min_level: u8,
}

impl<W: Write + Send> JsonLineLogger<W> {
    #[must_use]
    pub fn new(sink: W, bindings: Vec<(String, String)>) -> Self {
        Self {
            sink: Mutex::new(sink),
            hostname: gethostname::gethostname().to_string_lossy().into_owned(),
            bindings,
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

    fn write(&self, level: u8, err: Option<&LogError>, fields: &[(&str, &str)], message: &str) {
        if level < self.min_level {
            return;
        }
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        let mut record = Map::new();
        record.insert("level".into(), Value::from(level));
        record.insert("time".into(), Value::from(u64::try_from(time).unwrap_or(0)));
        record.insert("pid".into(), Value::from(std::process::id()));
        record.insert("hostname".into(), Value::from(self.hostname.as_str()));
        for (key, value) in &self.bindings {
            record.insert(key.clone(), Value::from(value.as_str()));
        }
        if let Some(err) = err {
            record.insert("err".into(), err.to_value());
        }
        for (key, value) in fields {
            record.insert((*key).to_owned(), Value::from(*value));
        }
        record.insert("msg".into(), Value::from(message));
        if let Ok(mut sink) = self.sink.lock() {
            let _ = writeln!(sink, "{}", Value::Object(record));
        }
    }
}

impl<W: Write + Send> Logger for JsonLineLogger<W> {
    fn trace(&self, fields: &[(&str, &str)], message: &str) {
        self.write(TRACE, None, fields, message);
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
