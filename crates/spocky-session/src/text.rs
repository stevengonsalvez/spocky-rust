//! JavaScript string semantics used by session logic: ECMAScript white space
//! (`String.prototype.trim`, `/\s/`) and UTF-16 slicing over JavaScript text
//! ([`spocky_store::js_value`] encoding, which keeps lone surrogates).

use std::path::Path;

use spocky_store::js_value::{js_text, js_text_from_utf16, js_text_utf16};

pub use spocky_contracts::text::{is_js_whitespace, js_trim};

/// `value.replace(/\s+/g, " ")`.
#[must_use]
pub fn collapse_js_whitespace(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut in_run = false;
    for character in value.chars() {
        if is_js_whitespace(character) {
            if !in_run {
                out.push(' ');
            }
            in_run = true;
        } else {
            out.push(character);
            in_run = false;
        }
    }
    out
}

/// `value.slice(0, units)` in UTF-16 code units; a split pair leaves a lone
/// surrogate, kept as JavaScript text.
#[must_use]
pub fn slice_utf16(value: &str, units: usize) -> String {
    let code_units: Vec<u16> = js_text_utf16(value).take(units).collect();
    js_text_from_utf16(&code_units)
}

/// A path from the operating system (the working directory, a resolved path,
/// a link target) as JavaScript text: node decodes the name bytes as UTF-8
/// with U+FFFD for the invalid ones, and a literal U+10FFFF is doubled.
#[must_use]
pub fn path_text(path: &Path) -> String {
    js_text(&path.to_string_lossy())
}

/// Bytes from outside (process output, file contents) as JavaScript text:
/// UTF-8 decoded with U+FFFD for the invalid sequences, as node does.
#[must_use]
pub fn bytes_text(bytes: &[u8]) -> String {
    js_text(&String::from_utf8_lossy(bytes))
}

/// `readFileSync(path, "utf8")` as JavaScript text.
///
/// # Errors
///
/// The error of reading the file.
pub fn read_text(path: impl AsRef<Path>) -> std::io::Result<String> {
    std::fs::read(path).map(|bytes| bytes_text(&bytes))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use spocky_contracts::text::js_length;
    use spocky_store::js_value::{JsTextUnit, js_text_units};

    use super::{bytes_text, collapse_js_whitespace, js_trim, path_text, read_text, slice_utf16};

    /// The escape U+10FFFF followed by U+F0000 would read as an encoded lone
    /// surrogate unless the escape is doubled on the way in.
    const COLLIDING: &str = "a\u{10FFFF}\u{F0000}b";

    fn is_plain_text(text: &str) -> bool {
        js_text_units(text).all(|unit| matches!(unit, JsTextUnit::Char(_)))
    }

    #[test]
    fn outside_bytes_become_javascript_text() {
        let text = bytes_text(COLLIDING.as_bytes());
        assert_eq!(text, "a\u{10FFFF}\u{10FFFF}\u{F0000}b");
        assert!(is_plain_text(&text));
        assert_eq!(bytes_text(b"a\xffb"), "a\u{FFFD}b");
    }

    #[test]
    fn outside_paths_become_javascript_text() {
        let path = Path::new(OsStr::from_bytes(COLLIDING.as_bytes()));
        assert_eq!(path_text(path), bytes_text(COLLIDING.as_bytes()));
        assert_eq!(
            path_text(Path::new(OsStr::from_bytes(b"/a/\xffb"))),
            "/a/\u{FFFD}b"
        );
    }

    #[test]
    fn file_contents_become_javascript_text() {
        let home = std::env::temp_dir().join(format!("spocky-read-text-{}", std::process::id()));
        std::fs::create_dir_all(&home).expect("home");
        let file = home.join("f");
        std::fs::write(&file, [COLLIDING.as_bytes(), b"\xff"].concat()).expect("write");
        let text = read_text(&file).expect("read");
        assert_eq!(text, "a\u{10FFFF}\u{10FFFF}\u{F0000}b\u{FFFD}");
        assert!(is_plain_text(&text));
        assert!(read_text(home.join("missing")).is_err());
        std::fs::remove_dir_all(&home).expect("clean");
    }

    #[test]
    fn whitespace_matches_ecmascript() {
        // node: " \u0085x\u0085 ".trim().length === 3, "﻿x﻿".trim() === "x"
        assert_eq!(js_trim(" \u{85}x\u{85} ").len(), "\u{85}x\u{85}".len());
        assert_eq!(js_trim("\u{feff}x\u{feff}"), "x");
        assert_eq!(js_trim("\u{180e}x"), "\u{180e}x");
        assert_eq!(collapse_js_whitespace("a \t\u{a0}b\u{85}c"), "a b\u{85}c");
    }

    #[test]
    fn utf16_slicing_keeps_lone_surrogates() {
        let text = "ab😀cd";
        assert_eq!(js_length(text), 6);
        assert_eq!(slice_utf16(text, 4), "ab😀");
        let split = slice_utf16(text, 3);
        assert_eq!(js_length(&split), 3);
        assert_ne!(split, "ab😀");
        assert_eq!(slice_utf16(text, 100), text);
    }
}
