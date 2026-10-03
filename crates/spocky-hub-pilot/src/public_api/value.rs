//! Construction helpers and request body decoding over [`spocky_contracts::js_value`].
//!
//! The public API reads and writes JavaScript values: object key order, number text and string
//! escapes are observable. Parsing, printing, cloning and dropping live in `js_value`, which keeps
//! deep nesting off the Rust stack. Strings are JavaScript text as `js_value` defines it: a lone
//! surrogate is held as an escape pair, and the same encoding is used for strings that cross the
//! operation boundary in either direction.

use spocky_contracts::js_value::{self, JsObject, JsValue, js_text};

/// The value type of the public API.
pub type Json = JsValue;

/// Builders and accessors the public API needs on [`JsValue`].
pub trait JsValueExt: Sized {
    /// An object from fixed pairs, keeping the given order.
    fn object<const N: usize>(fields: [(&str, Self); N]) -> Self;
    /// An object from pairs built at run time; a repeated key keeps its first position.
    fn from_pairs(fields: Vec<(String, Self)>) -> Self;
    fn string(value: &str) -> Self;
    /// Hub numbers are JavaScript doubles, so integers above 2^53 lose precision there too.
    fn integer(value: i64) -> Self;
    /// The `typeof`-style name zod prints in `received` clauses.
    fn type_name(&self) -> &'static str;
    /// `JSON.stringify(value)`.
    fn stringify(&self) -> String;
}

impl JsValueExt for JsValue {
    fn object<const N: usize>(fields: [(&str, Self); N]) -> Self {
        Self::from_pairs(
            fields
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        )
    }

    fn from_pairs(fields: Vec<(String, Self)>) -> Self {
        let mut object = JsObject::new();
        for (key, value) in fields {
            object.insert(key, value);
        }
        Self::Object(object)
    }

    fn string(value: &str) -> Self {
        // Rust text enters the value domain through `js_text`, which doubles a literal U+10FFFF so
        // it cannot read as the lone surrogate escape.
        Self::String(js_text(value))
    }

    #[allow(clippy::cast_precision_loss)]
    fn integer(value: i64) -> Self {
        Self::Number(value as f64)
    }

    fn type_name(&self) -> &'static str {
        match self {
            Self::Undefined => "undefined",
            Self::Null => "null",
            Self::Bool(_) => "boolean",
            Self::Number(_) => "number",
            Self::String(_) => "string",
            Self::Array(_) => "array",
            Self::Object(_) => "object",
        }
    }

    fn stringify(&self) -> String {
        js_value::stringify(self)
    }
}

/// Decodes a request body the way `Request.json()` does: the body reader drops one leading byte
/// order mark, the lossy UTF-8 decode drops one more, then `JSON.parse` runs. Observed on the
/// baseline runtime: two leading marks parse, three do not.
#[must_use]
pub fn decode_request_json(body: &[u8]) -> Option<JsValue> {
    let body = body.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(body);
    let decoded = String::from_utf8_lossy(body);
    let decoded = decoded.strip_prefix('\u{feff}').unwrap_or(&decoded);
    js_value::parse(decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::{JsValueExt as _, Json, decode_request_json};

    #[test]
    fn objects_follow_javascript_key_order_and_numbers_print_like_javascript() {
        let parsed =
            decode_request_json(b"{\"b\":1,\"2\":2,\"a\":1e21,\"1\":4,\"b\":-0}").expect("parses");
        assert_eq!(parsed.stringify(), "{\"1\":4,\"2\":2,\"b\":0,\"a\":1e+21}");
        assert_eq!(Json::integer(100).stringify(), "100");
    }

    #[test]
    fn rust_text_with_the_escape_character_stays_text() {
        // Read raw, U+10FFFF followed by U+F0000 would decode as one lone surrogate.
        let value = Json::string("\u{10FFFF}\u{F0000}");
        let Json::String(text) = &value else {
            unreachable!("string builds a string");
        };
        assert_eq!(spocky_contracts::text::js_length(text), 4);
    }

    #[test]
    fn body_decoding_drops_at_most_two_byte_order_marks() {
        let marks = |count: usize| [[0xef, 0xbb, 0xbf].repeat(count), b"{}".to_vec()].concat();
        assert!(decode_request_json(&marks(2)).is_some());
        assert!(decode_request_json(&marks(3)).is_none());
    }
}
