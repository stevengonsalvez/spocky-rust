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
const INFO: u8 = 30;
const WARN: u8 = 40;
const ERROR: u8 = 50;
const FATAL: u8 = 60;

/// A log destination. `fields` are the first pino argument, `message` the second.
pub trait Logger: Send + Sync {
    fn info(&self, fields: &[(&str, &str)], message: &str);
    fn warn(&self, fields: &[(&str, &str)], message: &str);
    fn error(&self, fields: &[(&str, &str)], message: &str);
    fn fatal(&self, fields: &[(&str, &str)], message: &str);
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

/// Writes one pino-shaped JSON object per line: `level`, `time`, `pid`, the
/// bindings, the fields, then `msg`.
pub struct JsonLineLogger<W: Write + Send> {
    sink: Mutex<W>,
    bindings: Vec<(String, String)>,
}

impl<W: Write + Send> JsonLineLogger<W> {
    #[must_use]
    pub fn new(sink: W, bindings: Vec<(String, String)>) -> Self {
        Self {
            sink: Mutex::new(sink),
            bindings,
        }
    }

    fn write(&self, level: u8, fields: &[(&str, &str)], message: &str) {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        let mut record = Map::new();
        record.insert("level".into(), Value::from(level));
        record.insert("time".into(), Value::from(u64::try_from(time).unwrap_or(0)));
        record.insert("pid".into(), Value::from(std::process::id()));
        for (key, value) in &self.bindings {
            record.insert(key.clone(), Value::from(value.as_str()));
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
    fn info(&self, fields: &[(&str, &str)], message: &str) {
        self.write(INFO, fields, message);
    }
    fn warn(&self, fields: &[(&str, &str)], message: &str) {
        self.write(WARN, fields, message);
    }
    fn error(&self, fields: &[(&str, &str)], message: &str) {
        self.write(ERROR, fields, message);
    }
    fn fatal(&self, fields: &[(&str, &str)], message: &str) {
        self.write(FATAL, fields, message);
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
