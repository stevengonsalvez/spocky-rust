//! Builds trace entries that render exactly as `JSON.stringify` of the driver's objects.

use spocky_crypto::js_string::{JsString, json_quote, utf16};

/// An ordered JSON object under construction. `undefined` fields are simply not added.
pub struct Entry {
    fields: Vec<(String, String)>,
}

impl Entry {
    pub fn new(kind: &str) -> Self {
        Self { fields: Vec::new() }.str("t", kind)
    }

    pub fn raw(mut self, key: &str, rendered: impl Into<String>) -> Self {
        self.fields.push((key.to_owned(), rendered.into()));
        self
    }

    pub fn str(self, key: &str, value: &str) -> Self {
        self.js(key, &utf16(value))
    }

    pub fn js(self, key: &str, value: &JsString) -> Self {
        self.raw(key, json_quote(value))
    }

    pub fn num(self, key: &str, value: impl std::fmt::Display) -> Self {
        self.raw(key, value.to_string())
    }

    pub fn bool(self, key: &str, value: bool) -> Self {
        self.raw(key, value.to_string())
    }

    pub fn opt_str(self, key: &str, value: Option<&str>) -> Self {
        match value {
            Some(value) => self.str(key, value),
            None => self,
        }
    }

    pub fn render(&self) -> String {
        let body: Vec<String> = self
            .fields
            .iter()
            .map(|(key, value)| format!("{}:{value}", json_quote(&utf16(key))))
            .collect();
        format!("{{{}}}", body.join(","))
    }
}
