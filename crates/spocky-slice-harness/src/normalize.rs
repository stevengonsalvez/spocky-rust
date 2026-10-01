//! Normalization rule discovery for the slice gates.
//!
//! Only these value classes are ever normalized, each by exact value through
//! `spocky-differential` rules (never by pattern at comparison time), in this
//! application order:
//!
//! | Class | Category | Values |
//! |---|---|---|
//! | extracted secret (for example `daemon-public-key`) | generated id | a random secret read from its known file on each side |
//! | `disposable-root` | temporary path | the side's disposable root (realpath form) |
//! | `disposable-root-tmp-alias` | temporary path | the same root through the `/tmp` symlink |
//! | `disposable-root-slug` | temporary path | the root as Paseo slugs it for per-cwd directories |
//! | `daemon-listen`, `stub-listen` | generated id | `127.0.0.1:<port>` of the daemon and the Responses stub |
//! | `sha256-<kind>-of-generated-id-<n>` | generated id | a digest verified to equal `sha256(JSON.stringify([kind, id]))` of generated id `n` |
//! | `generated-id-<n>` | generated id | the n-th distinct generated id on each side, in first-appearance order |
//! | `wall-clock` | wall clock | ISO-8601 UTC instants and Unix epoch milliseconds or seconds inside the side's run window |
//!
//! Generated ids are paired by first-appearance order across the canonical
//! text sequence, so the comparison still fails when one side reuses an id
//! where the other mints a new one, or mints them in a different order. A side
//! with a different number of distinct generated ids, a digest derived on one
//! side only, or a secret extracted on one side only fails discovery.
//! Wall-clock values outside the run window (for example fixed fixture dates)
//! stay literal and must match exactly.

use serde_json::Value;
use spocky_differential::{NormalizationCategory, NormalizationRule, NormalizationTarget};

const OWNER: &str = "p3_slice_harness";

/// Facts about one side's run that bound which literal values may be normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideFacts {
    /// Canonical disposable root, for example `/private/tmp/spocky-p3-g1-original-1`.
    pub root: String,
    pub daemon_port: u16,
    pub stub_port: u16,
    /// Inclusive wall-clock window of the run, in Unix milliseconds.
    pub window_start_ms: u64,
    pub window_end_ms: u64,
}

/// A text artifact or state file, addressed the way `spocky-differential` targets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    pub target: NormalizationTarget,
    pub text: String,
}

/// Generated id shapes, in the order they are scanned at each position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdShape {
    /// Lowercase hyphenated UUID (agent ids, Codex thread, turn, and window ids).
    Uuid,
    /// A fixed prefix followed by exactly `len` characters from `alphabet`.
    Prefixed {
        prefix: &'static str,
        len: usize,
        alphabet: Alphabet,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alphabet {
    LowerHex,
    /// `A-Z a-z 0-9 _ -`.
    Base64Url,
}

impl Alphabet {
    fn contains(self, byte: u8) -> bool {
        match self {
            Self::LowerHex => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
            Self::Base64Url => byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-',
        }
    }
}

fn is_lower_hex(byte: u8) -> bool {
    Alphabet::LowerHex.contains(byte)
}

/// Returns the id of `shape` starting at `at`, if one starts there and is not
/// glued to a longer run of id characters on either side.
fn id_at(text: &[u8], at: usize, shape: IdShape) -> Option<usize> {
    let boundary = |index: usize| {
        text.get(index)
            .is_none_or(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_' || *byte == b'-'))
    };
    if at > 0 && !boundary(at - 1) {
        return None;
    }
    let end = match shape {
        IdShape::Uuid => {
            let groups = [8, 4, 4, 4, 12];
            let mut index = at;
            for (group, length) in groups.iter().enumerate() {
                if group > 0 {
                    if text.get(index) != Some(&b'-') {
                        return None;
                    }
                    index += 1;
                }
                let slice = text.get(index..index + length)?;
                if !slice.iter().all(|byte| is_lower_hex(*byte)) {
                    return None;
                }
                index += length;
            }
            index
        }
        IdShape::Prefixed {
            prefix,
            len,
            alphabet,
        } => {
            if !text[at..].starts_with(prefix.as_bytes()) {
                return None;
            }
            let start = at + prefix.len();
            let slice = text.get(start..start + len)?;
            if !slice.iter().all(|byte| alphabet.contains(*byte)) {
                return None;
            }
            start + len
        }
    };
    boundary(end).then_some(end)
}

/// Lists distinct generated ids in first-appearance order across `texts`.
#[must_use]
pub fn distinct_ids(texts: &[&str], shapes: &[IdShape]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for text in texts {
        let bytes = text.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            let hit = shapes
                .iter()
                .find_map(|shape| id_at(bytes, index, *shape).map(|end| (index, end)));
            if let Some((start, end)) = hit {
                let id = &text[start..end];
                if !found.iter().any(|known| known == id) {
                    found.push(id.to_owned());
                }
                index = end;
            } else {
                index += 1;
            }
        }
    }
    found
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn digits(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fraction]Z` at `at`; returns (end, unix ms).
fn iso_at(text: &[u8], at: usize) -> Option<(usize, u64)> {
    if at > 0 && text[at - 1].is_ascii_digit() {
        return None;
    }
    let head = text.get(at..at + 19)?;
    let shape_ok = head.iter().enumerate().all(|(index, byte)| match index {
        4 | 7 => *byte == b'-',
        10 => *byte == b'T',
        13 | 16 => *byte == b':',
        _ => byte.is_ascii_digit(),
    });
    if !shape_ok {
        return None;
    }
    let year = digits(&head[0..4])?;
    let month = digits(&head[5..7])?;
    let day = digits(&head[8..10])?;
    let hour = digits(&head[11..13])?;
    let minute = digits(&head[14..16])?;
    let second = digits(&head[17..19])?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut end = at + 19;
    let mut millis = 0_i64;
    if text.get(end) == Some(&b'.') {
        let start = end + 1;
        let mut stop = start;
        while text.get(stop).is_some_and(u8::is_ascii_digit) {
            stop += 1;
        }
        if stop == start {
            return None;
        }
        let fraction = &text[start..stop];
        let mut padded = [b'0'; 3];
        for (slot, byte) in padded.iter_mut().zip(fraction) {
            *slot = *byte;
        }
        millis = digits(&padded)?;
        end = stop;
    }
    if text.get(end) != Some(&b'Z') {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let total = ((days * 24 + hour) * 60 + minute) * 60 + second;
    let unix_ms = u64::try_from(total * 1000 + millis).ok()?;
    Some((end + 1, unix_ms))
}

/// Lists distinct wall-clock literals inside `[start_ms, end_ms]`, in first
/// appearance order: ISO-8601 UTC instants, 13-digit epoch milliseconds, and
/// 10-digit epoch seconds. Digit runs of any other length are never touched.
#[must_use]
pub fn wall_clock_values(texts: &[&str], start_ms: u64, end_ms: u64) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut push = |value: &str| {
        if !found.iter().any(|known| known == value) {
            found.push(value.to_owned());
        }
    };
    for text in texts {
        let bytes = text.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if let Some((end, unix_ms)) = iso_at(bytes, index) {
                if (start_ms..=end_ms).contains(&unix_ms) {
                    push(&text[index..end]);
                }
                index = end;
                continue;
            }
            if bytes[index].is_ascii_digit() && (index == 0 || !is_word(bytes[index - 1])) {
                let mut stop = index;
                while stop < bytes.len() && bytes[stop].is_ascii_digit() {
                    stop += 1;
                }
                if stop == bytes.len() || !is_word(bytes[stop]) {
                    let run = &text[index..stop];
                    let in_window = match run.len() {
                        13 => run
                            .parse::<u64>()
                            .is_ok_and(|ms| (start_ms..=end_ms).contains(&ms)),
                        10 => run.parse::<u64>().is_ok_and(|seconds| {
                            (start_ms / 1000..=end_ms / 1000).contains(&seconds)
                        }),
                        _ => false,
                    };
                    if in_window {
                        push(run);
                    }
                }
                index = stop;
                continue;
            }
            index += 1;
        }
    }
    found
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.'
}

/// The `/tmp` alias of a `/private/tmp` root, if it has one.
#[must_use]
pub fn tmp_alias(root: &str) -> Option<String> {
    root.strip_prefix("/private").map(str::to_owned)
}

/// One value class with the exact literals to replace on each side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueClass {
    pub id: String,
    pub category: NormalizationCategory,
    pub reason: String,
    pub left: Vec<String>,
    pub right: Vec<String>,
}

/// Kinds whose `sha256(JSON.stringify([kind, id]))` digests Paseo uses as
/// content-addressed file names under `creations/`.
pub const DIGEST_KINDS: [&str; 3] = ["agent", "workspace", "create"];

/// Lowercase hex SHA-256 of `JSON.stringify([kind, id])` for plain ids.
#[must_use]
pub fn kind_digest(kind: &str, id: &str) -> String {
    use sha2::{Digest, Sha256};
    let input = format!("[{},{}]", Value::from(kind), Value::from(id));
    lower_hex(&Sha256::digest(input.as_bytes()))
}

/// Lowercase hex encoding.
#[must_use]
pub fn lower_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    hex
}

/// A digest proven to equal `sha256([kind, id])` for an id on the same side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedDigest {
    pub digest: String,
    pub kind: &'static str,
    pub id: String,
}

/// Finds every digest in `found` that is derived from another id in `found`.
#[must_use]
pub fn derived_digests(found: &[String]) -> Vec<DerivedDigest> {
    let mut derived = Vec::new();
    for id in found {
        for kind in DIGEST_KINDS {
            let digest = kind_digest(kind, id);
            if found.contains(&digest) {
                derived.push(DerivedDigest {
                    digest,
                    kind,
                    id: id.clone(),
                });
            }
        }
    }
    derived
}

/// One side's inputs to rule discovery.
#[derive(Debug, Clone)]
pub struct SideInput<'a> {
    pub facts: &'a SideFacts,
    /// Every compared text in canonical scan order.
    pub texts: Vec<&'a str>,
    /// Generated secrets read from known locations, as (class id, value).
    pub extracted: Vec<(String, String)>,
}

/// The slug Paseo derives from a disposable root path for per-cwd directories.
#[must_use]
pub fn root_slug(root: &str) -> String {
    root.trim_start_matches('/').replace('/', "-")
}

fn pair(
    id: &str,
    category: NormalizationCategory,
    reason: &str,
    left: String,
    right: String,
) -> ValueClass {
    ValueClass {
        id: id.into(),
        category,
        reason: reason.into(),
        left: vec![left],
        right: vec![right],
    }
}

/// Extracted secrets, disposable-root forms, and listen addresses.
fn fixed_classes(left: &SideInput<'_>, right: &SideInput<'_>) -> Result<Vec<ValueClass>, String> {
    let (lf, rf) = (left.facts, right.facts);
    let mut classes = Vec::new();
    let mut extracted_ids: Vec<&String> = left.extracted.iter().map(|(id, _)| id).collect();
    for (id, _) in &right.extracted {
        if !extracted_ids.contains(&id) {
            extracted_ids.push(id);
        }
    }
    for class_id in extracted_ids {
        let find = |side: &SideInput<'_>| {
            side.extracted
                .iter()
                .find(|(id, _)| id == class_id)
                .map(|(_, value)| value.clone())
        };
        let (Some(left_value), Some(right_value)) = (find(left), find(right)) else {
            return Err(format!(
                "extracted secret {class_id} exists on one side only"
            ));
        };
        classes.push(pair(
            class_id,
            NormalizationCategory::GeneratedId,
            "generated secret read from its known file",
            left_value,
            right_value,
        ));
    }
    classes.push(pair(
        "disposable-root",
        NormalizationCategory::TemporaryPath,
        "per-run disposable root directory",
        lf.root.clone(),
        rf.root.clone(),
    ));
    if let (Some(left_alias), Some(right_alias)) = (tmp_alias(&lf.root), tmp_alias(&rf.root)) {
        classes.push(pair(
            "disposable-root-tmp-alias",
            NormalizationCategory::TemporaryPath,
            "per-run disposable root through the /tmp symlink",
            left_alias,
            right_alias,
        ));
    }
    classes.push(pair(
        "disposable-root-slug",
        NormalizationCategory::TemporaryPath,
        "per-cwd directory slug of the disposable root",
        root_slug(&lf.root),
        root_slug(&rf.root),
    ));
    classes.push(pair(
        "daemon-listen",
        NormalizationCategory::GeneratedId,
        "random loopback daemon port",
        format!("127.0.0.1:{}", lf.daemon_port),
        format!("127.0.0.1:{}", rf.daemon_port),
    ));
    classes.push(pair(
        "stub-listen",
        NormalizationCategory::GeneratedId,
        "random loopback Responses stub port",
        format!("127.0.0.1:{}", lf.stub_port),
        format!("127.0.0.1:{}", rf.stub_port),
    ));

    Ok(classes)
}

/// Builds every value class for a gate, in application order.
///
/// # Errors
///
/// Returns a message when the sides mint a different number of distinct
/// generated ids, derive digests from different ids, or extract different
/// secret classes; each is a mismatch in itself.
pub fn value_classes(
    left: &SideInput<'_>,
    right: &SideInput<'_>,
    shapes: &[IdShape],
) -> Result<Vec<ValueClass>, String> {
    let (lf, rf) = (left.facts, right.facts);
    let mut classes = fixed_classes(left, right)?;

    let left_found = distinct_ids(&left.texts, shapes);
    let right_found = distinct_ids(&right.texts, shapes);
    let left_derived = derived_digests(&left_found);
    let right_derived = derived_digests(&right_found);
    let plain = |found: Vec<String>, derived: &[DerivedDigest]| -> Vec<String> {
        found
            .into_iter()
            .filter(|id| !derived.iter().any(|entry| entry.digest == *id))
            .collect()
    };
    let left_ids = plain(left_found, &left_derived);
    let right_ids = plain(right_found, &right_derived);
    if left_ids.len() != right_ids.len() {
        return Err(format!(
            "generated id count differs: left {} right {}",
            left_ids.len(),
            right_ids.len()
        ));
    }
    let mut derived_classes = Vec::new();
    let mut id_classes = Vec::new();
    for (index, (left_id, right_id)) in left_ids.iter().zip(&right_ids).enumerate() {
        let number = index + 1;
        for kind in DIGEST_KINDS {
            let from = |derived: &[DerivedDigest], id: &str| {
                derived
                    .iter()
                    .find(|entry| entry.kind == kind && entry.id == id)
                    .map(|entry| entry.digest.clone())
            };
            match (from(&left_derived, left_id), from(&right_derived, right_id)) {
                (Some(left_digest), Some(right_digest)) => derived_classes.push(pair(
                    &format!("sha256-{kind}-of-generated-id-{number}"),
                    NormalizationCategory::GeneratedId,
                    "content-addressed name verified as sha256 of [kind, id]",
                    left_digest,
                    right_digest,
                )),
                (None, None) => {}
                _ => {
                    return Err(format!(
                        "sha256 {kind} digest of generated id {number} exists on one side only"
                    ));
                }
            }
        }
        id_classes.push(pair(
            &format!("generated-id-{number}"),
            NormalizationCategory::GeneratedId,
            "generated id paired by first appearance",
            left_id.clone(),
            right_id.clone(),
        ));
    }
    if left_derived.len() != derived_classes.len() || right_derived.len() != derived_classes.len() {
        return Err("a derived digest refers to an extracted or unpaired id".into());
    }
    classes.extend(derived_classes);
    classes.extend(id_classes);

    let left_clock = wall_clock_values(&left.texts, lf.window_start_ms, lf.window_end_ms);
    let right_clock = wall_clock_values(&right.texts, rf.window_start_ms, rf.window_end_ms);
    if !left_clock.is_empty() || !right_clock.is_empty() {
        classes.push(ValueClass {
            id: "wall-clock".into(),
            category: NormalizationCategory::WallClock,
            reason: "wall-clock instant inside the run window".into(),
            left: left_clock,
            right: right_clock,
        });
    }
    Ok(classes)
}

/// Replaces every generated id in `text` with `{id}` and every derived digest
/// with `{sha256:<kind>}`, for ordering files whose names are generated.
#[must_use]
pub fn mask(text: &str, shapes: &[IdShape], derived: &[DerivedDigest]) -> String {
    let bytes = text.as_bytes();
    let mut masked = String::with_capacity(text.len());
    let mut index = 0;
    let mut copied = 0;
    while index < bytes.len() {
        if let Some(end) = shapes.iter().find_map(|shape| id_at(bytes, index, *shape)) {
            masked.push_str(&text[copied..index]);
            let id = &text[index..end];
            match derived.iter().find(|entry| entry.digest == id) {
                Some(entry) => {
                    masked.push_str("{sha256:");
                    masked.push_str(entry.kind);
                    masked.push('}');
                }
                None => masked.push_str("{id}"),
            }
            index = end;
            copied = end;
        } else {
            index += 1;
        }
    }
    masked.push_str(&text[copied..]);
    masked
}

fn replace_all(text: &str, values: &[String], token: &str) -> (String, bool) {
    let mut result = text.to_owned();
    let mut matched = false;
    for value in values {
        if result.contains(value.as_str()) {
            result = result.replace(value.as_str(), token);
            matched = true;
        }
    }
    (result, matched)
}

fn target_key(target: &NormalizationTarget) -> String {
    format!("{target:?}")
}

/// Emits one exact-value rule per (class, target) where the class occurs on
/// either side, simulating sequential application so a later class never
/// claims text an earlier class already replaced.
///
/// Values are the union of both sides' literals, longest first. A rule that
/// matches only one side makes `spocky-differential` fail the comparison with
/// a normalization miss, which is the intended mismatch signal.
#[must_use]
pub fn rules_for(classes: &[ValueClass], left: &[Text], right: &[Text]) -> Vec<NormalizationRule> {
    let mut left_state: Vec<(String, String)> = left
        .iter()
        .map(|text| (target_key(&text.target), text.text.clone()))
        .collect();
    let mut right_state: Vec<(String, String)> = right
        .iter()
        .map(|text| (target_key(&text.target), text.text.clone()))
        .collect();
    let mut targets: Vec<NormalizationTarget> = Vec::new();
    for text in left.iter().chain(right) {
        if !targets.contains(&text.target) {
            targets.push(text.target.clone());
        }
    }

    let mut rules = Vec::new();
    for class in classes {
        let mut values: Vec<String> = Vec::new();
        for value in class.left.iter().chain(&class.right) {
            if !values.contains(value) {
                values.push(value.clone());
            }
        }
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        if values.is_empty() {
            continue;
        }
        let token = format!("{{{{{:?}:{}}}}}", class.category, class.id);
        for target in &targets {
            let key = target_key(target);
            let mut hit = false;
            for state in [&mut left_state, &mut right_state] {
                if let Some((_, text)) = state.iter_mut().find(|(name, _)| *name == key) {
                    let (next, matched) = replace_all(text, &values, &token);
                    *text = next;
                    hit |= matched;
                }
            }
            if hit {
                rules.push(NormalizationRule {
                    id: format!("{}@{}", class.id, rule_suffix(target)),
                    category: class.category,
                    reason: class.reason.clone(),
                    owner: OWNER.into(),
                    target: target.clone(),
                    exact_values: values.clone(),
                });
            }
        }
    }
    rules
}

fn rule_suffix(target: &NormalizationTarget) -> String {
    match target {
        NormalizationTarget::StructuredJsonPointer(pointer) => format!("json:{pointer}"),
        NormalizationTarget::Stdout => "stdout".into(),
        NormalizationTarget::Stderr => "stderr".into(),
        NormalizationTarget::Artifact(name) => format!("artifact:{name}"),
        NormalizationTarget::State(name) => format!("state:{name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spocky_differential::{
        Artifact, ExecutionCounts, Observation, ObservationSlot, Scenario, compare_observations,
    };
    use std::collections::BTreeMap;

    const UUID_A: &str = "0199a3c4-1b2c-7d3e-8f40-123456789abc";
    const UUID_B: &str = "0199a3c4-1b2c-7d3e-8f40-cba987654321";
    const UUID_C: &str = "11111111-2222-4333-8444-555555555555";
    const UUID_D: &str = "66666666-7777-4888-9999-aaaaaaaaaaaa";

    fn wks() -> IdShape {
        IdShape::Prefixed {
            prefix: "wks_",
            len: 16,
            alphabet: Alphabet::LowerHex,
        }
    }

    fn hex64() -> IdShape {
        IdShape::Prefixed {
            prefix: "",
            len: 64,
            alphabet: Alphabet::LowerHex,
        }
    }

    #[test]
    fn kind_digest_matches_paseo_creation_digest() {
        // sha256 of the exact UTF-8 bytes ["agent","a09a900c-7425-4446-93ea-66f22d55593e"].
        assert_eq!(
            kind_digest("agent", "a09a900c-7425-4446-93ea-66f22d55593e"),
            sha256_hex(br#"["agent","a09a900c-7425-4446-93ea-66f22d55593e"]"#)
        );
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        lower_hex(&Sha256::digest(bytes))
    }

    #[test]
    fn derived_digests_pair_by_kind_and_id() {
        let left = vec![
            artifact(
                "a",
                format!(
                    "{UUID_A} {} {}",
                    kind_digest("agent", UUID_A),
                    kind_digest("create", UUID_C)
                ),
            ),
            artifact("b", UUID_C.into()),
        ];
        let right = vec![
            artifact(
                "a",
                format!(
                    "{UUID_B} {} {}",
                    kind_digest("agent", UUID_B),
                    kind_digest("create", UUID_D)
                ),
            ),
            artifact("b", UUID_D.into()),
        ];
        assert_eq!(equivalent(&left, &right), Ok(true));
    }

    #[test]
    fn derived_digest_of_a_different_kind_differs() {
        let left = vec![artifact(
            "a",
            format!("{UUID_A} {}", kind_digest("agent", UUID_A)),
        )];
        let right = vec![artifact(
            "a",
            format!("{UUID_B} {}", kind_digest("workspace", UUID_B)),
        )];
        assert!(equivalent(&left, &right).is_err());
    }

    #[test]
    fn underived_digest_is_a_plain_generated_id() {
        let digest = kind_digest("other", UUID_A);
        let left = vec![artifact("a", format!("{UUID_A} {digest}"))];
        let right = vec![artifact(
            "a",
            format!("{UUID_B} {}", kind_digest("other", UUID_B)),
        )];
        assert_eq!(equivalent(&left, &right), Ok(true));
        let classes_left = distinct_ids(&[&left[0].text], &[IdShape::Uuid, hex64()]);
        assert!(derived_digests(&classes_left).is_empty());
    }

    #[test]
    fn mask_hides_ids_and_names_derived_kinds() {
        let derived = derived_digests(&[UUID_A.to_owned(), kind_digest("agent", UUID_A)]);
        let text = format!(
            "creations/{}.claim {UUID_A} wks_0123456789abcdef",
            kind_digest("agent", UUID_A)
        );
        assert_eq!(
            mask(&text, &[IdShape::Uuid, wks(), hex64()], &derived),
            "creations/{sha256:agent}.claim {id} {id}"
        );
    }

    #[test]
    fn root_slug_matches_paseo_cwd_slug() {
        assert_eq!(
            root_slug("/private/tmp/spocky-p3-g1-0199a3c41b2c"),
            "private-tmp-spocky-p3-g1-0199a3c41b2c"
        );
    }

    #[test]
    fn extracted_secret_on_one_side_fails_discovery() {
        let left_facts = facts("/private/tmp/a", 1, 2);
        let right_facts = facts("/private/tmp/b", 3, 4);
        let left = SideInput {
            facts: &left_facts,
            texts: vec!["k"],
            extracted: vec![("daemon-public-key".into(), "k".into())],
        };
        let right = SideInput {
            facts: &right_facts,
            texts: vec!["k"],
            extracted: Vec::new(),
        };
        assert!(value_classes(&left, &right, &[IdShape::Uuid]).is_err());
    }

    #[test]
    fn finds_uuids_and_prefixed_ids_in_first_appearance_order() {
        let text = format!("a {UUID_B} b wks_0123456789abcdef c {UUID_A} {UUID_B}");
        assert_eq!(
            distinct_ids(&[&text], &[IdShape::Uuid, wks()]),
            vec![UUID_B, "wks_0123456789abcdef", UUID_A]
        );
    }

    #[test]
    fn ignores_ids_glued_to_longer_tokens_or_wrong_case() {
        let upper = UUID_A.to_uppercase();
        let text =
            format!("x{UUID_A} {UUID_A}0 wks_0123456789abcdef0 wks_0123456789ABCDEF {upper}");
        assert!(distinct_ids(&[&text], &[IdShape::Uuid, wks()]).is_empty());
    }

    #[test]
    fn wall_clock_only_inside_window() {
        // 2026-10-01T13:51:43.463Z == 1790862703463 ms.
        let inside = "2026-10-01T13:51:43.463Z";
        let fixed = "2020-01-02T03:04:05Z";
        let text = format!(
            "{inside} {fixed} 1790862703463 1790862703 179086270346 17908627034630 x1790862703463"
        );
        assert_eq!(
            wall_clock_values(&[&text], 1_790_862_700_000, 1_790_862_710_000),
            vec![inside, "1790862703463", "1790862703"]
        );
        assert!(wall_clock_values(&[&text], 1_800_000_000_000, 1_800_000_001_000).is_empty());
    }

    #[test]
    fn iso_fractions_and_seconds_parse_exactly() {
        assert_eq!(iso_at(b"1970-01-01T00:00:00Z", 0), Some((20, 0)));
        assert_eq!(iso_at(b"1970-01-01T00:00:01.5Z", 0), Some((22, 1500)));
        assert_eq!(
            iso_at(b"2000-02-29T00:00:00.123456Z", 0),
            Some((27, 951_782_400_123))
        );
        assert_eq!(iso_at(b"2000-13-01T00:00:00Z", 0), None);
        assert_eq!(iso_at(b"2000-01-01T00:00:00+01:00", 0), None);
    }

    fn facts(root: &str, daemon_port: u16, stub_port: u16) -> SideFacts {
        SideFacts {
            root: root.into(),
            daemon_port,
            stub_port,
            window_start_ms: 1_790_862_700_000,
            window_end_ms: 1_790_862_710_000,
        }
    }

    fn artifact(name: &str, text: String) -> Text {
        Text {
            target: NormalizationTarget::Artifact(name.into()),
            text,
        }
    }

    fn observation(texts: &[Text]) -> Observation {
        let artifacts = texts
            .iter()
            .map(|text| {
                let NormalizationTarget::Artifact(name) = &text.target else {
                    panic!("tests only use artifact targets");
                };
                Artifact::new(name.clone(), text.text.clone().into_bytes())
            })
            .collect();
        Observation {
            structured_output: ObservationSlot::Missing,
            stdout: ObservationSlot::Missing,
            stderr: ObservationSlot::Missing,
            exit_code: ObservationSlot::Missing,
            artifacts: ObservationSlot::Value(artifacts),
            state: ObservationSlot::Missing,
            screenshots: ObservationSlot::Missing,
            accessibility: ObservationSlot::Missing,
            performance: ObservationSlot::Missing,
            recovery: ObservationSlot::Missing,
            counts: ObservationSlot::Value(COUNTS),
            raw_failures: Vec::new(),
        }
    }

    const COUNTS: ExecutionCounts = ExecutionCounts {
        fixtures: 1,
        assertions: 1,
    };

    /// Runs the real differential comparison: `Ok(equivalent)`, or `Err` when
    /// a rule misses one side.
    fn equivalent(left: &[Text], right: &[Text]) -> Result<bool, String> {
        let rules = classes_and_rules(left, right)?;
        let scenario = Scenario {
            id: "normalize-test".into(),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            initial_files: Vec::new(),
            expected_counts: COUNTS,
        };
        compare_observations(&scenario, observation(left), observation(right), &rules)
            .map(|manifest| manifest.equivalent)
            .map_err(|error| error.to_string())
    }

    fn classes_and_rules(left: &[Text], right: &[Text]) -> Result<Vec<NormalizationRule>, String> {
        let left_facts = facts("/private/tmp/spocky-p3-g1-original-1", 41001, 42001);
        let right_facts = facts("/private/tmp/spocky-p3-g1-spocky-22", 41002, 42002);
        let left_input = SideInput {
            facts: &left_facts,
            texts: left.iter().map(|text| text.text.as_str()).collect(),
            extracted: Vec::new(),
        };
        let right_input = SideInput {
            facts: &right_facts,
            texts: right.iter().map(|text| text.text.as_str()).collect(),
            extracted: Vec::new(),
        };
        let classes = value_classes(&left_input, &right_input, &[IdShape::Uuid, wks(), hex64()])?;
        Ok(rules_for(&classes, left, right))
    }

    #[test]
    fn equivalent_sides_normalize_to_identical_text() {
        let left = vec![artifact(
            "run.stdout",
            format!(
                "{UUID_A} /private/tmp/spocky-p3-g1-original-1/project /tmp/spocky-p3-g1-original-1/x 127.0.0.1:41001 127.0.0.1:42001 2026-10-01T13:51:43.463Z {UUID_C} {UUID_A}"
            ),
        )];
        let right = vec![artifact(
            "run.stdout",
            format!(
                "{UUID_B} /private/tmp/spocky-p3-g1-spocky-22/project /tmp/spocky-p3-g1-spocky-22/x 127.0.0.1:41002 127.0.0.1:42002 2026-10-01T13:51:44.001Z {UUID_D} {UUID_B}"
            ),
        )];
        assert_eq!(equivalent(&left, &right), Ok(true));
    }

    #[test]
    fn swapped_id_identity_still_differs() {
        // Left reuses its first id; right mints a new one at the same position.
        let left = vec![artifact("a", format!("{UUID_A} {UUID_C} {UUID_A}"))];
        let right = vec![artifact("a", format!("{UUID_B} {UUID_D} {UUID_D}"))];
        assert_eq!(equivalent(&left, &right), Ok(false));
    }

    #[test]
    fn different_generated_id_counts_fail_discovery() {
        let left = vec![artifact("a", format!("{UUID_A} {UUID_C}"))];
        let right = vec![artifact("a", UUID_B.into())];
        assert!(classes_and_rules(&left, &right).is_err());
    }

    #[test]
    fn realpath_and_tmp_alias_stay_distinguishable() {
        let left = vec![artifact(
            "a",
            "/private/tmp/spocky-p3-g1-original-1/p".into(),
        )];
        let right = vec![artifact("a", "/tmp/spocky-p3-g1-spocky-22/p".into())];
        // Each class matches only one side, so the comparison itself fails.
        assert!(equivalent(&left, &right).is_err());
    }

    #[test]
    fn value_present_on_one_side_only_fails_comparison() {
        let left = vec![artifact("a", "listening on 127.0.0.1:41001".into())];
        let right = vec![artifact("a", "listening".into())];
        assert!(equivalent(&left, &right).is_err());
    }

    #[test]
    fn untouched_differences_survive_normalization() {
        let left = vec![artifact("a", format!("{UUID_A} status=completed"))];
        let right = vec![artifact("a", format!("{UUID_B} status=error"))];
        assert_eq!(equivalent(&left, &right), Ok(false));
    }

    #[test]
    fn rules_only_target_texts_where_a_class_occurs() {
        let left = vec![
            artifact("ids", UUID_A.into()),
            artifact("plain", "no generated values".into()),
        ];
        let right = vec![
            artifact("ids", UUID_B.into()),
            artifact("plain", "no generated values".into()),
        ];
        let rules = classes_and_rules(&left, &right).unwrap();
        assert_eq!(
            rules
                .iter()
                .map(|rule| (rule.id.as_str(), rule.exact_values.clone()))
                .collect::<Vec<_>>(),
            vec![(
                "generated-id-1@artifact:ids",
                vec![UUID_A.to_owned(), UUID_B.to_owned()]
            )]
        );
    }
}
