//! `Sec-WebSocket-Protocol` header parsing.
//!
//! Source: `lib/subprotocol.js` of the `ws` library (8.20.0) that the pinned
//! daemon uses for the `/ws` upgrade.

/// `tokenChars` in `ws/lib/validation.js`: RFC 7230 token characters, ASCII only.
fn is_token_char(code: u32) -> bool {
    matches!(code,
        0x21 | 0x23..=0x27 | 0x2a | 0x2b | 0x2d | 0x2e | 0x30..=0x39 | 0x41..=0x5a
        | 0x5e..=0x7a | 0x7c | 0x7e)
}

/// `subprotocol.parse`: the protocol names in header order. Any character that
/// is not a token character, space, tab or comma, an empty entry, a trailing
/// separator, or a repeated name is an error, which `ws` answers with 400.
///
/// Node joins repeated `Sec-WebSocket-Protocol` header lines with `, ` before
/// this runs, so callers pass the joined value.
///
/// # Errors
///
/// Returns the `SyntaxError` message of the library.
pub fn parse_subprotocols(header: &str) -> Result<Vec<String>, String> {
    // The library walks UTF-16 code units; every non-token unit is an error, so
    // walking chars gives the same accept and reject decisions.
    let chars: Vec<(usize, char)> = header.char_indices().collect();
    let mut protocols: Vec<String> = Vec::new();
    let mut start: Option<usize> = None;
    let mut end: Option<usize> = None;

    for (position, &(byte_index, c)) in chars.iter().enumerate() {
        let code = c as u32;
        if end.is_none() && is_token_char(code) {
            if start.is_none() {
                start = Some(byte_index);
            }
        } else if position != 0 && (c == ' ' || c == '\t') {
            if end.is_none() && start.is_some() {
                end = Some(byte_index);
            }
        } else if c == ',' {
            let Some(begin) = start else {
                return Err(format!("Unexpected character at index {position}"));
            };
            let finish = *end.get_or_insert(byte_index);
            let protocol = &header[begin..finish];
            if protocols.iter().any(|seen| seen == protocol) {
                return Err(format!("The \"{protocol}\" subprotocol is duplicated"));
            }
            protocols.push(protocol.to_owned());
            start = None;
            end = None;
        } else {
            return Err(format!("Unexpected character at index {position}"));
        }
    }

    let Some(begin) = start.filter(|_| end.is_none()) else {
        return Err("Unexpected end of input".to_owned());
    };
    let protocol = &header[begin..];
    if protocols.iter().any(|seen| seen == protocol) {
        return Err(format!("The \"{protocol}\" subprotocol is duplicated"));
    }
    protocols.push(protocol.to_owned());
    Ok(protocols)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(header: &str) -> Vec<String> {
        parse_subprotocols(header).unwrap()
    }

    #[test]
    fn parses_comma_separated_tokens_with_optional_blanks() {
        assert_eq!(ok("a"), ["a"]);
        assert_eq!(ok("a,b"), ["a", "b"]);
        assert_eq!(ok("a , b\t,c"), ["a", "b", "c"]);
        assert_eq!(
            ok("paseo.bearer.tok_en-1, chat"),
            ["paseo.bearer.tok_en-1", "chat"]
        );
    }

    #[test]
    fn rejects_duplicates_even_across_blanks() {
        assert_eq!(
            parse_subprotocols("a, b, a"),
            Err("The \"a\" subprotocol is duplicated".to_owned())
        );
        assert_eq!(
            parse_subprotocols("a,a"),
            Err("The \"a\" subprotocol is duplicated".to_owned())
        );
    }

    #[test]
    fn rejects_empty_entries_and_trailing_separators() {
        for header in ["", " ", ",", "a,", "a,,b", ",a", "a ,", "a b", " a"] {
            assert!(parse_subprotocols(header).is_err(), "{header:?}");
        }
    }

    #[test]
    fn rejects_characters_outside_the_token_set() {
        for header in [
            "a;b", "a\"b", "a(b", "a/b", "a:b", "é", "a\u{7f}", "a=b", "a@b", "[x]",
        ] {
            assert!(parse_subprotocols(header).is_err(), "{header:?}");
        }
        assert_eq!(ok("a!#$%&'*+-.^_`|~9Z"), ["a!#$%&'*+-.^_`|~9Z"]);
    }

    #[test]
    fn reports_the_index_like_the_library() {
        assert_eq!(
            parse_subprotocols("ab;"),
            Err("Unexpected character at index 2".to_owned())
        );
        assert_eq!(
            parse_subprotocols("a,,"),
            Err("Unexpected character at index 2".to_owned())
        );
        assert_eq!(
            parse_subprotocols("a "),
            Err("Unexpected end of input".to_owned())
        );
    }

    #[test]
    fn a_repeated_header_line_joined_with_a_comma_is_one_list() {
        assert_eq!(ok("a, b"), ["a", "b"]);
        assert!(parse_subprotocols("a, a").is_err());
    }
}
