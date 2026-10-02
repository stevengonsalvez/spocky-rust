//! JavaScript text at its boundaries: a literal U+10FFFF, escaped lone
//! surrogates, and valid pairs go in and come out as Node has them.
//!
//! Expected values were printed by node v22.20.0 for `JSON.parse(text)`:
//! `JSON.stringify`, the UTF-16 code units, and the hex of the UTF-8 Node
//! writes for `String(value)` (a lone surrogate is U+FFFD). The scheme these
//! tests pin is described in `spocky_contracts::js_value`.

use spocky_contracts::js::js_string;
use spocky_contracts::js_value::{
    JsValue, js_text, js_text_canonical, js_text_from_utf16, js_text_to_utf8, js_text_utf16, parse,
    stringify,
};

/// (JSON text, `JSON.stringify(v)`, UTF-16 units of `String(v)`, UTF-8 hex).
const STRINGS: [(&str, &str, &[u16], &str); 14] = [
    (
        "\"\\udbff\\udfff\"",
        "\"\u{10ffff}\"",
        &[0xDBFF, 0xDFFF],
        "f48fbfbf",
    ),
    (
        "\"a\\udbff\\udfffb\"",
        "\"a\u{10ffff}b\"",
        &[0x0061, 0xDBFF, 0xDFFF, 0x0062],
        "61f48fbfbf62",
    ),
    (
        "\"\\udbff\\udfff\\udb80\\udc00\"",
        "\"\u{10ffff}\u{f0000}\"",
        &[0xDBFF, 0xDFFF, 0xDB80, 0xDC00],
        "f48fbfbff3b08080",
    ),
    ("\"\\ud83d\"", "\"\\ud83d\"", &[0xD83D], "efbfbd"),
    ("\"\\ude00\"", "\"\\ude00\"", &[0xDE00], "efbfbd"),
    (
        "\"\\ud83d\\ude00\"",
        "\"\u{1f600}\"",
        &[0xD83D, 0xDE00],
        "f09f9880",
    ),
    (
        "\"\\ud83dx\\ude00\"",
        "\"\\ud83dx\\ude00\"",
        &[0xD83D, 0x0078, 0xDE00],
        "efbfbd78efbfbd",
    ),
    (
        "\"\\ude00\\ud83d\"",
        "\"\\ude00\\ud83d\"",
        &[0xDE00, 0xD83D],
        "efbfbdefbfbd",
    ),
    (
        "\"\\udbff\\udfff\\ud800\"",
        "\"\u{10ffff}\\ud800\"",
        &[0xDBFF, 0xDFFF, 0xD800],
        "f48fbfbfefbfbd",
    ),
    (
        "\"\\ud800\\udbff\\udfff\"",
        "\"\\ud800\u{10ffff}\"",
        &[0xD800, 0xDBFF, 0xDFFF],
        "efbfbdf48fbfbf",
    ),
    (
        "\"\\udb80\\udc00\"",
        "\"\u{f0000}\"",
        &[0xDB80, 0xDC00],
        "f3b08080",
    ),
    (
        "\"\\udbff\\udfff\\udbff\\udfff\"",
        "\"\u{10ffff}\u{10ffff}\"",
        &[0xDBFF, 0xDFFF, 0xDBFF, 0xDFFF],
        "f48fbfbff48fbfbf",
    ),
    (
        "\"\u{e9}\u{1f600}\u{65e5}\u{672c}\"",
        "\"\u{e9}\u{1f600}\u{65e5}\u{672c}\"",
        &[0x00E9, 0xD83D, 0xDE00, 0x65E5, 0x672C],
        "c3a9f09f9880e697a5e69cac",
    ),
    (
        "\"\\u0000\\u007f\"",
        "\"\\u0000\u{7f}\"",
        &[0x0000, 0x007F],
        "007f",
    ),
];

/// (JSON text, `JSON.stringify(v)`, UTF-8 hex of `String(v)`).
const NESTED: [(&str, &str, &str); 2] = [
    (
        "{\"\\udbff\\udfff\":\"\\ud83d\"}",
        "{\"\u{10ffff}\":\"\\ud83d\"}",
        "5b6f626a656374204f626a6563745d",
    ),
    (
        "[\"\\udbff\\udfff\",\"\\udb80\\udc00\"]",
        "[\"\u{10ffff}\",\"\u{f0000}\"]",
        "f48fbfbf2cf3b08080",
    ),
];

/// (a, b, `JSON.stringify(a + b)`, units of `a + b`, UTF-8 hex of `a + b`).
const CONCATENATED: [(&str, &str, &str, &[u16], &str); 8] = [
    (
        "\"\\ud83d\"",
        "\"\\ude00\"",
        "\"\u{1f600}\"",
        &[0xD83D, 0xDE00],
        "f09f9880",
    ),
    (
        "\"\\ude00\"",
        "\"\\ud83d\"",
        "\"\\ude00\\ud83d\"",
        &[0xDE00, 0xD83D],
        "efbfbdefbfbd",
    ),
    (
        "\"\\ud83d\"",
        "\"x\"",
        "\"\\ud83dx\"",
        &[0xD83D, 0x0078],
        "efbfbd78",
    ),
    (
        "\"\\udbff\\udfff\"",
        "\"\\udb80\\udc00\"",
        "\"\u{10ffff}\u{f0000}\"",
        &[0xDBFF, 0xDFFF, 0xDB80, 0xDC00],
        "f48fbfbff3b08080",
    ),
    (
        "\"\\udbff\\udfff\"",
        "\"\\ud83d\"",
        "\"\u{10ffff}\\ud83d\"",
        &[0xDBFF, 0xDFFF, 0xD83D],
        "f48fbfbfefbfbd",
    ),
    (
        "\"\\ud83d\"",
        "\"\\udbff\\udfff\"",
        "\"\\ud83d\u{10ffff}\"",
        &[0xD83D, 0xDBFF, 0xDFFF],
        "efbfbdf48fbfbf",
    ),
    (
        "\"\\udbff\"",
        "\"\\udfff\"",
        "\"\u{10ffff}\"",
        &[0xDBFF, 0xDFFF],
        "f48fbfbf",
    ),
    (
        "\"\\ud83d\\ude00\"",
        "\"\\ud83d\"",
        "\"\u{1f600}\\ud83d\"",
        &[0xD83D, 0xDE00, 0xD83D],
        "f09f9880efbfbd",
    ),
];

fn hex(text: &str) -> String {
    use std::fmt::Write as _;
    text.bytes().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn units_of(value: &JsValue) -> Vec<u16> {
    js_text_utf16(value.as_str().expect("string")).collect()
}

#[test]
fn strings_parse_and_leave_like_node() {
    for (text, json, units, utf8) in STRINGS {
        let value = parse(text).expect(text);
        assert_eq!(stringify(&value), json, "{text}");
        assert_eq!(units_of(&value), units, "{text}");
        assert_eq!(
            hex(&js_text_to_utf8(&js_string(Some(&value)))),
            utf8,
            "{text}"
        );
        // The canonical form is the one UTF-16 decoding gives.
        assert_eq!(
            js_text_canonical(value.as_str().expect("string")),
            js_text_from_utf16(units),
            "{text}"
        );
    }
}

#[test]
fn arrays_and_objects_leave_like_node() {
    for (text, json, utf8) in NESTED {
        let value = parse(text).expect(text);
        assert_eq!(stringify(&value), json, "{text}");
        assert_eq!(
            hex(&js_text_to_utf8(&js_string(Some(&value)))),
            utf8,
            "{text}"
        );
    }
}

#[test]
fn concatenation_is_javascript_concatenation() {
    for (left, right, json, units, utf8) in CONCATENATED {
        let (left, right) = (parse(left).expect(left), parse(right).expect(right));
        let joined = JsValue::String(format!(
            "{}{}",
            js_string(Some(&left)),
            js_string(Some(&right))
        ));
        assert_eq!(stringify(&joined), json, "{left:?} + {right:?}");
        assert_eq!(units_of(&joined), units, "{left:?} + {right:?}");
        assert_eq!(hex(&js_text_to_utf8(&js_string(Some(&joined)))), utf8);
        assert_eq!(
            js_text_canonical(joined.as_str().expect("string")),
            js_text_from_utf16(units)
        );
    }
}

/// A literal U+10FFFF beside the characters an escape is made of must stay
/// itself when text enters from outside, however it is spelled.
#[test]
fn outside_text_with_the_escape_character_is_never_read_as_an_escape() {
    // U+10FFFF then U+F0000 (the encoding of a lone high surrogate U+D800),
    // then two U+10FFFF, then U+10FFFF at the end.
    let outside = "a\u{10FFFF}\u{F0000}\u{10FFFF}\u{10FFFF}b\u{10FFFF}";
    let expected_units: Vec<u16> = outside.encode_utf16().collect();
    let entered = JsValue::String(js_text(outside));
    assert_eq!(units_of(&entered), expected_units);
    assert_eq!(js_text_to_utf8(entered.as_str().expect("string")), outside);
    // The same text through serde_json, as a value and as an object key.
    let source = serde_json::json!({ outside: [outside] });
    let value = JsValue::from(&source);
    let key = value
        .as_object()
        .expect("object")
        .iter()
        .next()
        .expect("key")
        .0;
    assert_eq!(js_text_to_utf8(key), outside);
    let item = value
        .get(&js_text(outside))
        .and_then(JsValue::as_array)
        .expect("array");
    assert_eq!(js_text_to_utf8(item[0].as_str().expect("string")), outside);
    assert_eq!(stringify(&entered), format!("\"{outside}\""));
}

/// `From<&serde_json::Value>` is `JSON.parse` of the same text, case by
/// case: both reviewer examples, every table case `serde_json` can read (it
/// rejects lone surrogates), and a literal U+10FFFF in every position.
#[test]
fn serde_json_values_equal_parsed_values() {
    let literal = [
        "\"x\u{10FFFF}\"",
        "\"\u{10FFFF}\u{F0000}\"",
        "\"\u{10FFFF}\"",
        "\"\u{10FFFF}\u{10FFFF}\"",
        "\"\u{F0000}\u{10FFFF}\u{F07FF}\"",
        "{\"\u{10FFFF}\u{F0000}\":[\"x\u{10FFFF}\",\"\u{10FFFF}\u{F0000}\"]}",
    ];
    let tables = STRINGS
        .iter()
        .map(|(text, ..)| *text)
        .chain(NESTED.iter().map(|(text, ..)| *text));
    let mut compared = 0;
    for text in literal.into_iter().chain(tables) {
        let Ok(source) = serde_json::from_str::<serde_json::Value>(text) else {
            continue;
        };
        let (converted, parsed) = (JsValue::from(&source), parse(text).expect(text));
        assert_eq!(converted, parsed, "{text}");
        assert_eq!(stringify(&converted), stringify(&parsed), "{text}");
        compared += 1;
    }
    // serde_json read all six literals and the table cases without lone
    // surrogates, so the comparison is not vacuous.
    assert!(compared >= 6 + 8, "compared {compared} cases");
}

/// Every sequence of UTF-16 code units survives the escape scheme, and every
/// valid text survives a trip in and out.
#[test]
fn the_escape_scheme_is_injective_and_round_trips() {
    let mut state: u32 = 0x9E37_79B9;
    let mut next = move || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        state >> 8
    };
    // Code units and characters that stress the escape: surrogates of both
    // halves, the pair that encodes U+10FFFF, and the private-use range the
    // escape payloads live in.
    let unit_pool: [u16; 9] = [
        0xD800, 0xDBFF, 0xDC00, 0xDFFF, 0xDB80, 0xDC00, 0x0041, 0x00E9, 0x20AC,
    ];
    for _ in 0..5_000 {
        let length = next() % 9;
        let units: Vec<u16> = (0..length)
            .map(|_| match next() % 10 {
                9 => 0x10 + (next() % 0x400) as u16,
                index => unit_pool[index as usize],
            })
            .collect();
        let text = js_text_from_utf16(&units);
        assert_eq!(
            js_text_utf16(&text).collect::<Vec<_>>(),
            units,
            "{units:04X?}"
        );
        // Concatenating two texts is concatenating their units.
        let other_units: Vec<u16> = units.iter().rev().copied().collect();
        let other = js_text_from_utf16(&other_units);
        let joined: Vec<u16> = units.iter().chain(&other_units).copied().collect();
        assert_eq!(
            js_text_utf16(&format!("{text}{other}")).collect::<Vec<_>>(),
            joined
        );
        assert_eq!(
            js_text_canonical(&format!("{text}{other}")),
            js_text_from_utf16(&joined)
        );
    }
    let character_pool = [
        'a',
        'é',
        '\u{10FFFF}',
        '\u{F0000}',
        '\u{F07FF}',
        '\u{F0800}',
        '😀',
    ];
    for _ in 0..5_000 {
        let length = next() % 8;
        let outside: String = (0..length)
            .map(|_| character_pool[(next() as usize) % character_pool.len()])
            .collect();
        assert_eq!(js_text_to_utf8(&js_text(&outside)), outside);
        assert_eq!(
            js_text_utf16(&js_text(&outside)).collect::<Vec<_>>(),
            outside.encode_utf16().collect::<Vec<_>>()
        );
    }
}
