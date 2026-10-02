//! String identity after JavaScript concatenation. Joining an escaped high
//! surrogate to an escaped low one in the value encoding leaves two
//! representations of one string; `===`, `JsValue` equality, object keys,
//! and Map and Set keys must treat them as the string they are.
//!
//! Expected values were printed by node v22.20.0: for `joined = a + b` and
//! `literal = c`, whether `joined === literal`, what `object[literal]` is
//! after `object[joined] = 1`, how many keys the object has after both are
//! assigned, and how many entries a `Set` has after both are added (a
//! `Map` agrees with the `Set`, and `includes` and `indexOf` with `===`).
//! `a`, `b` and `c` are the escaped contents of JSON strings.

use std::collections::HashSet;

use spocky_contracts::js::{js_string, strict_equals};
use spocky_contracts::js_value::{
    JsObject, JsValue, js_text_canonical, js_text_eq, js_text_from_utf16, parse,
};
use spocky_contracts::json::JsRecord;
use spocky_contracts::text::JsText;

/// One node run: `a + b` against the literal `c`.
struct Case {
    a: &'static str,
    b: &'static str,
    c: &'static str,
    /// `joined === literal`.
    strict: bool,
    /// `object[literal]` after `object[joined] = 1`.
    object_get: Option<i32>,
    /// The key count after both are assigned.
    object_keys: usize,
    /// The `Set` size after both are added.
    set_size: usize,
}

const CASES: [Case; 8] = [
    Case {
        a: "\\ud83d",
        b: "\\ude00",
        c: "\\ud83d\\ude00",
        strict: true,
        object_get: Some(1),
        object_keys: 1,
        set_size: 1,
    },
    Case {
        a: "\\ude00",
        b: "\\ud83d",
        c: "\\ud83d\\ude00",
        strict: false,
        object_get: None,
        object_keys: 2,
        set_size: 2,
    },
    Case {
        a: "\\ud83d",
        b: "\\ude00",
        c: "\\ud83d\\ude00x",
        strict: false,
        object_get: None,
        object_keys: 2,
        set_size: 2,
    },
    Case {
        a: "\\udbff",
        b: "\\udfff",
        c: "\\udbff\\udfff",
        strict: true,
        object_get: Some(1),
        object_keys: 1,
        set_size: 1,
    },
    Case {
        a: "\\udbff\\udfff",
        b: "",
        c: "\\udbff\\udfff",
        strict: true,
        object_get: Some(1),
        object_keys: 1,
        set_size: 1,
    },
    Case {
        a: "\\ud83d",
        b: "\\ude00\\ud83d",
        c: "\\ud83d\\ude00\\ud83d",
        strict: true,
        object_get: Some(1),
        object_keys: 1,
        set_size: 1,
    },
    Case {
        a: "\\ud83d\\ud83d",
        b: "\\ude00",
        c: "\\ud83d\\ud83d\\ude00",
        strict: true,
        object_get: Some(1),
        object_keys: 1,
        set_size: 1,
    },
    Case {
        a: "x",
        b: "y",
        c: "xy",
        strict: true,
        object_get: Some(1),
        object_keys: 1,
        set_size: 1,
    },
];

fn string(escaped: &str) -> JsValue {
    parse(&format!("\"{escaped}\"")).expect("a JSON string")
}

fn text(value: &JsValue) -> String {
    js_string(Some(value))
}

#[test]
fn concatenated_strings_equal_their_literals_like_node() {
    for Case {
        a,
        b,
        c,
        strict,
        object_get,
        object_keys,
        set_size,
    } in CASES
    {
        let (a, b, literal) = (string(a), string(b), string(c));
        let joined = JsValue::String(format!("{}{}", text(&a), text(&b)));
        let label = format!("{a:?} + {b:?} vs {literal:?}");
        // `===`, `JsValue` equality and the shared text comparison.
        assert_eq!(
            strict_equals(Some(&joined), Some(&literal)),
            strict,
            "{label}"
        );
        assert_eq!(
            strict_equals(Some(&literal), Some(&joined)),
            strict,
            "{label}"
        );
        assert_eq!(joined == literal, strict, "{label}");
        assert_eq!(
            js_text_eq(&text(&joined), &text(&literal)),
            strict,
            "{label}"
        );
        // `includes` and `indexOf`.
        let list = [joined.clone()];
        assert_eq!(list.contains(&literal), strict, "{label}");
        assert_eq!(
            list.iter().position(|item| *item == literal).is_some(),
            strict
        );
        // Object keys: `object[joined] = 1; object[literal]`, then both.
        let mut object = JsObject::new();
        object.insert(text(&joined), JsValue::Number(1.0));
        let found = object.get(&text(&literal)).and_then(JsValue::as_f64);
        assert_eq!(found, object_get.map(f64::from), "{label}");
        object.insert(text(&literal), JsValue::Number(2.0));
        assert_eq!(object.len(), object_keys, "{label}");
        // The other way round: `object[literal] = 1; object[joined]`.
        let mut reverse = JsObject::new();
        reverse.insert(text(&literal), JsValue::Number(1.0));
        assert_eq!(reverse.get(&text(&joined)).is_some(), strict, "{label}");
        // Map and Set keys are `JsText` here: equal and hash alike.
        let mut set = HashSet::new();
        set.insert(JsText::from_js(text(&joined)));
        set.insert(JsText::from_js(text(&literal)));
        assert_eq!(set.len(), set_size, "{label}");
        // Whole values: `[joined]` and `{joined: 1}` against their parses.
        let escaped = text(&literal);
        let array = JsValue::Array(vec![joined.clone()]);
        assert_eq!(
            array == JsValue::Array(vec![literal.clone()]),
            strict,
            "{label}"
        );
        let mut keyed = JsObject::new();
        keyed.insert(text(&joined), JsValue::Number(1.0));
        let mut other = JsObject::new();
        other.insert(escaped, JsValue::Number(1.0));
        assert_eq!(
            JsValue::Object(keyed) == JsValue::Object(other),
            strict,
            "{label}"
        );
    }
}

/// `JsRecord` keys follow the same identity as object keys: the node run's
/// `object[joined] = 1; object[literal]`, the key count after both, and the
/// reverse lookup.
#[test]
fn record_keys_are_code_unit_identical() {
    for Case {
        a,
        b,
        c,
        strict,
        object_get,
        object_keys,
        ..
    } in CASES
    {
        let (a, b, literal) = (string(a), string(b), string(c));
        let joined = format!("{}{}", text(&a), text(&b));
        let literal = text(&literal);
        let label = format!("{joined:?} vs {literal:?}");
        let mut record: JsRecord<i32> = JsRecord::new();
        record.insert(joined.clone(), 1);
        assert_eq!(record.get(&literal).copied(), object_get, "{label}");
        record.insert(literal.clone(), 2);
        assert_eq!(record.len(), object_keys, "{label}");
        let mut reverse: JsRecord<i32> = JsRecord::new();
        reverse.insert(literal, 1);
        assert_eq!(reverse.get(&joined).is_some(), strict, "{label}");
        // Stored keys are canonical, so they read as plain strings.
        for (key, _) in record.iter() {
            assert_eq!(key.as_str(), js_text_canonical(key), "{label}");
        }
    }
}

/// A stored key is canonical, so keys stay plain strings for everyone who
/// reads them.
#[test]
fn stored_keys_and_wrapped_text_are_canonical() {
    let joined = format!("{}{}", text(&string("\\ud83d")), text(&string("\\ude00")));
    assert_ne!(
        joined,
        text(&string("\\ud83d\\ude00")),
        "the join is not canonical"
    );
    let mut object = JsObject::new();
    object.insert(joined.clone(), JsValue::Null);
    let key = object.iter().next().expect("one key").0.to_owned();
    assert_eq!(key, text(&string("\\ud83d\\ude00")));
    assert_eq!(key, js_text_canonical(&joined));
    assert_eq!(JsText::from_js(joined).as_str(), key);
}

/// Seeded: any split of any code-unit sequence joins to the string of the
/// whole sequence, and sequences with different code units never compare
/// equal.
#[test]
fn equality_is_code_unit_equality() {
    struct Random(u32);
    impl Random {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            self.0 >> 8
        }

        fn units(&mut self, max: u32) -> Vec<u16> {
            const POOL: [u16; 7] = [0xD800, 0xDBFF, 0xDC00, 0xDFFF, 0x0041, 0xDB80, 0xDC00];
            let length = self.next() % max;
            (0..length)
                .map(|_| POOL[self.next() as usize % POOL.len()])
                .collect()
        }
    }
    let mut random = Random(0xC0FF_EE11);
    for _ in 0..5_000 {
        let units = random.units(8);
        let split = random.next() as usize % (units.len() + 1);
        let (left, right) = units.split_at(split);
        let joined = JsValue::String(format!(
            "{}{}",
            js_text_from_utf16(left),
            js_text_from_utf16(right)
        ));
        let whole = JsValue::String(js_text_from_utf16(&units));
        assert_eq!(joined, whole, "{units:04X?} split at {split}");
        assert!(strict_equals(Some(&joined), Some(&whole)), "{units:04X?}");
        let other_units = random.units(8);
        let other = JsValue::String(js_text_from_utf16(&other_units));
        assert_eq!(
            joined == other,
            units == other_units,
            "{units:04X?} {other_units:04X?}"
        );
    }
}
