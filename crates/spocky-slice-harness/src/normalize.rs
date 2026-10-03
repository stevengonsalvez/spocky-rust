//! Normalization rule discovery for the slice gates.
//!
//! Only these value classes are ever normalized, each by exact value through
//! `spocky-differential` rules (never by pattern at comparison time), in this
//! application order. Every token names its class, so values of different
//! shapes or formats never collapse into one token.
//!
//! | Class | Category | Values |
//! |---|---|---|
//! | extracted secret (allowlist [`EXTRACTED_CLASSES`]) | generated id | a random secret read from its known file on each side |
//! | `disposable-root` | temporary path | the side's disposable root (realpath form) |
//! | `disposable-root-tmp-alias` | temporary path | the same root through the `/tmp` symlink |
//! | `disposable-root-slug` | temporary path | the root as Paseo slugs it for per-cwd directories |
//! | `daemon-listen`, `stub-listen` | generated id | `127.0.0.1:<port>` of the daemon and the Responses stub |
//! | `sha256-of-<preimage>` | generated id | a digest verified to equal `sha256` of an exact preimage the gate sent (creation fingerprints) |
//! | `sha256-<kind>-of-<id class>` | generated id | a digest verified to equal `sha256(JSON.stringify([kind, id]))` of a paired generated id |
//! | `send-receipt-key-<n>` | generated id | the file name of a Paseo send receipt `agent-requests/<key>.json`, paired by its exact content fingerprint |
//! | `generated-id-<shape>-<n>` | generated id | the n-th distinct id of one [`SLICE_SHAPES`] shape, paired by first appearance |
//! | `short7-of-<id class>` | generated id | the quoted 7-character prefix `"xxxxxxx"` of a paired UUID (`agent.id.slice(0, 7)`) |
//! | `wall-clock-<format>-<n>` | wall clock | the n-th group of instants of one format inside the run window, paired by occurrence position; a group may merge distinct literals only within the format's resolution (same millisecond for `iso-frac3` and `epoch-ms`) |
//! | `wall-clock-<format>-<field>-<n>` | wall clock | an instant that is the value of one of [`INDEPENDENT_CLOCK_FIELDS`] (`"updatedAt"`, `"attentionTimestamp"`), masked with its key as one literal and paired only with the same field, never with another field's equal instant |
//! | `codex-wall-time` | wall clock | Codex's measured tool cell duration `Wall time <d+>.<d> seconds`, applied only in Responses stub request bodies (`stub/<nnn>`) |
//! | `cli-relative-age` | wall clock | the pinned CLI's rendering of an agent's age, `"created": "just now"` or `"created": "<n> <unit> ago"`, applied only in step stdout artifacts (`.../stdout`); the createdAt instant stays compared in state and `inspect` |
//! | `stub-content-length-<n>` | wall clock | the n-th stub request's `["content-length","<n>"]` header, only where it equals that side's raw body byte length; applied only in `stub/<nnn>` |
//!
//! A class whose left and right values are identical emits no rule: the value
//! is not generated per run and must match exactly.
//!
//! Discovery fails, which fails the gate, when the sides differ in generated
//! id count, in the shape at any pairing position, in which derived digests
//! or short prefixes exist, in wall-clock occurrence count or equality
//! structure per format, in Codex wall time count, in send receipt
//! fingerprints, or in which extracted secrets exist. Any other 64-hex value
//! (for example a content hash) is never normalized and must match exactly. Wall-clock values outside
//! the run window (for example fixed fixture dates) also stay literal.

use serde_json::Value;
use spocky_differential::{NormalizationCategory, NormalizationRule, NormalizationTarget};

const OWNER: &str = "p3_slice_harness";

/// The client every side runs: the unchanged pinned Paseo CLI. Classes that
/// depend on the CLI's own rendering (`cli-relative-age`) apply only when both
/// sides recorded this identity; a Spocky CLI would need that rendering
/// proved separately with a fixed clock.
pub const PINNED_CLIENT: &str = "pinned-paseo-cli";

/// Secret classes the harness may read from known files. No other class id is
/// accepted for extracted values.
pub const EXTRACTED_CLASSES: [&str; 3] =
    ["daemon-public-key", "daemon-secret-key", "local-credential"];

/// Facts about one side's run that bound which literal values may be normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SideFacts {
    /// Canonical disposable root, for example `/private/tmp/spocky-p3-g1-0199a3c41b2`.
    pub root: String,
    pub daemon_port: u16,
    pub stub_port: u16,
    /// Inclusive wall-clock window of the run, in Unix milliseconds.
    pub window_start_ms: u64,
    pub window_end_ms: u64,
    /// The client this side ran, [`PINNED_CLIENT`] for every gate today.
    pub client: String,
}

/// A text artifact or state file, addressed the way `spocky-differential` targets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    pub target: NormalizationTarget,
    pub text: String,
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

/// A generated id shape. The name is part of every token minted for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdShape {
    /// A fixed prefix (possibly empty) followed by a lowercase hyphenated
    /// version 4, 5, or 7 UUID with the RFC 9562 variant.
    Uuid {
        name: &'static str,
        prefix: &'static str,
    },
    /// A non-empty fixed prefix followed by exactly `len` `alphabet` characters.
    Prefixed {
        name: &'static str,
        prefix: &'static str,
        len: usize,
        alphabet: Alphabet,
    },
}

impl IdShape {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Uuid { name, .. } | Self::Prefixed { name, .. } => name,
        }
    }
}

/// Every generated id shape minted on the slice path, with its source.
pub const SLICE_SHAPES: [IdShape; 9] = [
    // Codex `additional_tools` item ids (`at_` + UUID) in Responses request bodies.
    IdShape::Uuid {
        name: "codex-tools-id",
        prefix: "at_",
    },
    // Codex input message item ids (`msg_` + UUID) in Responses request bodies.
    IdShape::Uuid {
        name: "codex-message-id",
        prefix: "msg_",
    },
    // Codex `custom_tool_call_output` item ids (`ctco_` + UUID) in Responses request bodies.
    IdShape::Uuid {
        name: "codex-tool-output-id",
        prefix: "ctco_",
    },
    // codex-app-server-agent.ts:6912 `permission-${parsed.itemId}` for Codex
    // command approvals, whose item id is `exec-` + UUID.
    IdShape::Uuid {
        name: "permission-id",
        prefix: "permission-exec-",
    },
    // Agent ids, creation idempotency keys, Codex thread, turn, window, and installation ids.
    IdShape::Uuid {
        name: "uuid",
        prefix: "",
    },
    // workspace-registry-model.ts:13 `wks_${randomBytes(8).toString("hex")}`.
    IdShape::Prefixed {
        name: "workspace-id",
        prefix: "wks_",
        len: 16,
        alphabet: Alphabet::LowerHex,
    },
    // workspace-registry-model.ts:17 `prj_${randomBytes(8).toString("hex")}`.
    IdShape::Prefixed {
        name: "project-id",
        prefix: "prj_",
        len: 16,
        alphabet: Alphabet::LowerHex,
    },
    // cli/src/utils/client-id.ts:12 `cid_${randomUUID().replace(/-/g, "")}`.
    IdShape::Prefixed {
        name: "client-id",
        prefix: "cid_",
        len: 32,
        alphabet: Alphabet::LowerHex,
    },
    // server-id.ts:25-26 `srv_${randomBytes(9).toString("base64url")}`.
    IdShape::Prefixed {
        name: "server-id",
        prefix: "srv_",
        len: 12,
        alphabet: Alphabet::Base64Url,
    },
];

fn is_id_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

/// Returns the end of an id of `shape` starting at `at`, if one starts there
/// and is not glued to a longer run of id characters on either side.
fn id_at(text: &[u8], at: usize, shape: IdShape) -> Option<usize> {
    if at > 0 && is_id_byte(text[at - 1]) {
        return None;
    }
    let end = match shape {
        IdShape::Uuid { prefix, .. } => {
            if !text[at..].starts_with(prefix.as_bytes()) {
                return None;
            }
            let start = at + prefix.len();
            let mut index = start;
            for (group, length) in [8, 4, 4, 4, 12].into_iter().enumerate() {
                if group > 0 {
                    if text.get(index) != Some(&b'-') {
                        return None;
                    }
                    index += 1;
                }
                let slice = text.get(index..index + length)?;
                if !slice.iter().all(|byte| Alphabet::LowerHex.contains(*byte)) {
                    return None;
                }
                index += length;
            }
            // Generated UUIDs only: version 4, 5, or 7 with the RFC 9562
            // variant. Constants such as the nil and max UUID never match.
            if !matches!(text[start + 14], b'4' | b'5' | b'7')
                || !matches!(text[start + 19], b'8' | b'9' | b'a' | b'b')
            {
                return None;
            }
            index
        }
        IdShape::Prefixed {
            prefix,
            len,
            alphabet,
            ..
        } => {
            if prefix.is_empty() || !text[at..].starts_with(prefix.as_bytes()) {
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
    text.get(end)
        .is_none_or(|byte| !is_id_byte(*byte))
        .then_some(end)
}

/// A generated id with the shape it matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundId {
    pub shape: &'static str,
    pub value: String,
}

/// Lists distinct generated ids in first-appearance order across `texts`.
/// At each position the first matching shape in `shapes` wins.
#[must_use]
pub fn distinct_ids(texts: &[&str], shapes: &[IdShape]) -> Vec<FoundId> {
    let mut found: Vec<FoundId> = Vec::new();
    for text in texts {
        let bytes = text.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            let hit = shapes
                .iter()
                .find_map(|shape| id_at(bytes, index, *shape).map(|end| (shape.name(), end)));
            if let Some((shape, end)) = hit {
                let value = &text[index..end];
                if !found.iter().any(|known| known.value == value) {
                    found.push(FoundId {
                        shape,
                        value: value.to_owned(),
                    });
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

/// Parses `YYYY-MM-DDTHH:MM:SS[.fraction]Z` at `at`; returns
/// (end, unix ms, fraction digit count).
fn iso_at(text: &[u8], at: usize) -> Option<(usize, u64, usize)> {
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
    let mut fraction_digits = 0;
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
        fraction_digits = stop - start;
        end = stop;
    }
    if text.get(end) != Some(&b'Z') {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let total = ((days * 24 + hour) * 60 + minute) * 60 + second;
    let unix_ms = u64::try_from(total * 1000 + millis).ok()?;
    Some((end + 1, unix_ms, fraction_digits))
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.'
}

/// One wall-clock literal occurrence: its format name (`iso-frac3`,
/// `iso-frac0`, `epoch-ms`, `epoch-s`, ...), text, and instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instant {
    pub format: String,
    pub value: String,
    pub unix_ms: u64,
}

impl Instant {
    /// The format's resolution in milliseconds: two instants closer than
    /// this can print identically on one side and differently on the other.
    #[must_use]
    pub fn resolution_ms(&self) -> u64 {
        // A per-field format is `<base>-<field>`; the base decides the unit.
        let base = self
            .format
            .match_indices('-')
            .nth(1)
            .map_or(self.format.as_str(), |(index, _)| &self.format[..index]);
        match base {
            "epoch-s" | "iso-frac0" => 1000,
            "iso-frac1" => 100,
            "iso-frac2" => 10,
            _ => 1,
        }
    }
}

/// JSON keys whose instants are masked each on its own. Pinned-vs-pinned runs
/// show the two are one value in one run and 5 ms apart in the next
/// (`updatedAt` 08:59:40.153 with `attentionTimestamp` 08:59:40.153 on one
/// daemon, 09:00:55.371 and 09:00:55.376 on the other), so their equality is
/// daemon timing, not contract. Every other wall-clock pairing stays.
pub const INDEPENDENT_CLOCK_FIELDS: [&str; 2] = ["updatedAt", "attentionTimestamp"];

/// One `"<field>": "<iso instant>"` occurrence in a text.
struct FieldSpan {
    field: &'static str,
    key_start: usize,
    value_start: usize,
    value_end: usize,
    unix_ms: u64,
    fraction: usize,
}

/// Every [`INDEPENDENT_CLOCK_FIELDS`] key holding a quoted ISO instant, in
/// text order. The key must be unescaped, followed by a colon and the quoted
/// instant, with only whitespace between.
fn field_spans(text: &str) -> Vec<FieldSpan> {
    let bytes = text.as_bytes();
    let skip_space = |mut at: usize| {
        while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
            at += 1;
        }
        at
    };
    let mut spans = Vec::new();
    for field in INDEPENDENT_CLOCK_FIELDS {
        let key = format!("\"{field}\"");
        let mut from = 0;
        while let Some(found) = text[from..].find(&key) {
            let key_start = from + found;
            let after_key = key_start + key.len();
            from = after_key;
            let colon = skip_space(after_key);
            if bytes.get(colon) != Some(&b':') {
                continue;
            }
            let quote = skip_space(colon + 1);
            if bytes.get(quote) != Some(&b'"') {
                continue;
            }
            let value_start = quote + 1;
            let Some((value_end, unix_ms, fraction)) = iso_at(bytes, value_start) else {
                continue;
            };
            if bytes.get(value_end) != Some(&b'"') {
                continue;
            }
            spans.push(FieldSpan {
                field,
                key_start,
                value_start,
                value_end,
                unix_ms,
                fraction,
            });
        }
    }
    spans.sort_by_key(|span| span.key_start);
    spans
}

/// The in-window instants held by [`INDEPENDENT_CLOCK_FIELDS`] keys, in scan
/// order with repeats. Each literal is the key through its closing quote, so
/// a replacement can only ever touch that field. The format is
/// `iso-frac<n>-<field>`; the generic scan ([`wall_clock_values`]) skips these.
#[must_use]
pub fn independent_clock_values(texts: &[&str], start_ms: u64, end_ms: u64) -> Vec<Instant> {
    let mut found = Vec::new();
    for text in texts {
        for span in field_spans(text) {
            if (start_ms..=end_ms).contains(&span.unix_ms) {
                found.push(Instant {
                    format: format!("iso-frac{}-{}", span.fraction, span.field),
                    value: text[span.key_start..=span.value_end].to_owned(),
                    unix_ms: span.unix_ms,
                });
            }
        }
    }
    found
}

/// Lists every wall-clock literal occurrence inside `[start_ms, end_ms]`, in
/// scan order with repeats: ISO-8601 UTC instants, 13-digit epoch
/// milliseconds, and 10-digit epoch seconds. Digit runs of any other length
/// are never touched. Instants held by [`INDEPENDENT_CLOCK_FIELDS`] keys are
/// left to [`independent_clock_values`].
#[must_use]
pub fn wall_clock_values(texts: &[&str], start_ms: u64, end_ms: u64) -> Vec<Instant> {
    let mut found: Vec<Instant> = Vec::new();
    for text in texts {
        let owned: Vec<usize> = field_spans(text)
            .iter()
            .map(|span| span.value_start)
            .collect();
        let bytes = text.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if let Some((end, unix_ms, fraction)) = iso_at(bytes, index) {
                if (start_ms..=end_ms).contains(&unix_ms) && !owned.contains(&index) {
                    found.push(Instant {
                        format: format!("iso-frac{fraction}"),
                        value: text[index..end].to_owned(),
                        unix_ms,
                    });
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
                    let number = run.parse::<u64>().ok();
                    let parsed = match (run.len(), number) {
                        (13, Some(ms)) if (start_ms..=end_ms).contains(&ms) => {
                            Some(("epoch-ms", ms))
                        }
                        (10, Some(seconds))
                            if (start_ms / 1000..=end_ms / 1000).contains(&seconds) =>
                        {
                            Some(("epoch-s", seconds * 1000))
                        }
                        _ => None,
                    };
                    if let Some((format, unix_ms)) = parsed {
                        found.push(Instant {
                            format: format.to_owned(),
                            value: run.to_owned(),
                            unix_ms,
                        });
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

/// The index of a literal on one side, adding it on first sight.
fn node_index<'a>(
    nodes: &mut Vec<(bool, &'a Instant)>,
    is_right: bool,
    instant: &'a Instant,
) -> usize {
    nodes
        .iter()
        .position(|(side, known)| *side == is_right && known.value == instant.value)
        .unwrap_or_else(|| {
            nodes.push((is_right, instant));
            nodes.len() - 1
        })
}

/// One format's occurrences, in scan order.
fn of_format<'a>(instants: &'a [Instant], format: &str) -> Vec<&'a Instant> {
    instants
        .iter()
        .filter(|instant| instant.format == format)
        .collect()
}

/// Pairs one format's occurrences by position and returns one class per
/// connected group of literals, in first-appearance order.
///
/// Equality structure is kept: two occurrences that share a literal on one
/// side must also share a group on the other. The only tolerated difference
/// is a merge within the format's resolution (two instants in the same
/// millisecond print identically on one side), so every group's distinct
/// values on each side must lie within one resolution unit.
fn wall_clock_classes(
    format: &str,
    left: &[&Instant],
    right: &[&Instant],
) -> Result<Vec<ValueClass>, String> {
    if left.len() != right.len() {
        return Err(format!(
            "wall-clock format {format} occurrence count differs: left {} right {}",
            left.len(),
            right.len()
        ));
    }
    // Union-find over literals; left nodes first, right nodes after.
    let mut nodes: Vec<(bool, &Instant)> = Vec::new();
    let mut parent: Vec<usize> = Vec::new();
    let find = |parent: &mut Vec<usize>, mut index: usize| {
        while parent[index] != index {
            parent[index] = parent[parent[index]];
            index = parent[index];
        }
        index
    };
    for (left_instant, right_instant) in left.iter().zip(right) {
        let a = node_index(&mut nodes, false, left_instant);
        let b = node_index(&mut nodes, true, right_instant);
        while parent.len() < nodes.len() {
            parent.push(parent.len());
        }
        let (root_a, root_b) = (find(&mut parent, a), find(&mut parent, b));
        parent[root_b] = root_a;
    }
    let mut groups: Vec<(usize, Vec<&Instant>, Vec<&Instant>)> = Vec::new();
    for (index, (is_right, instant)) in nodes.iter().enumerate() {
        let root = find(&mut parent, index);
        let position =
            if let Some(position) = groups.iter().position(|(known, _, _)| *known == root) {
                position
            } else {
                groups.push((root, Vec::new(), Vec::new()));
                groups.len() - 1
            };
        if *is_right {
            groups[position].2.push(instant);
        } else {
            groups[position].1.push(instant);
        }
    }
    let mut classes = Vec::new();
    for (number, (_, left_values, right_values)) in groups.into_iter().enumerate() {
        for values in [&left_values, &right_values] {
            let low = values
                .iter()
                .map(|instant| instant.unix_ms)
                .min()
                .unwrap_or(0);
            let high = values
                .iter()
                .map(|instant| instant.unix_ms)
                .max()
                .unwrap_or(0);
            let resolution = values.first().map_or(1, |instant| instant.resolution_ms());
            if high - low > resolution {
                return Err(format!(
                    "wall-clock format {format} equality structure differs: instants {} ms apart \
                     share one literal on the other side",
                    high - low
                ));
            }
        }
        classes.push(ValueClass {
            id: format!("wall-clock-{format}-{}", number + 1),
            category: NormalizationCategory::WallClock,
            reason: "wall-clock instant inside the run window, paired by position; merges only within the format's resolution".into(),
            left: left_values.iter().map(|instant| instant.value.clone()).collect(),
            right: right_values.iter().map(|instant| instant.value.clone()).collect(),
            scope: Scope::Every,
        });
    }
    Ok(classes)
}

/// The `/tmp` alias of a `/private/tmp` root, if it has one.
#[must_use]
pub fn tmp_alias(root: &str) -> Option<String> {
    root.strip_prefix("/private").map(str::to_owned)
}

/// The slug Paseo derives from a disposable root path for per-cwd directories.
#[must_use]
pub fn root_slug(root: &str) -> String {
    root.trim_start_matches('/').replace('/', "-")
}

/// Kinds whose `sha256(JSON.stringify([kind, id]))` digests Paseo uses as
/// content-addressed file names under `creations/` (creation/index.ts:265,353).
pub const DIGEST_KINDS: [&str; 3] = ["agent", "workspace", "create"];

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

/// Lowercase hex SHA-256 of `JSON.stringify([kind, id])` for plain ids.
#[must_use]
pub fn kind_digest(kind: &str, id: &str) -> String {
    use sha2::{Digest, Sha256};
    let input = format!("[{},{}]", Value::from(kind), Value::from(id));
    lower_hex(&Sha256::digest(input.as_bytes()))
}

/// A digest present in the texts and proven to equal `sha256([kind, id])`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedDigest {
    pub digest: String,
    pub kind: &'static str,
    pub id: String,
}

/// Finds every `sha256([kind, id])` of a found id that occurs in `texts`.
#[must_use]
pub fn derived_digests(found: &[FoundId], texts: &[&str]) -> Vec<DerivedDigest> {
    let mut derived = Vec::new();
    for id in found {
        for kind in DIGEST_KINDS {
            let digest = kind_digest(kind, &id.value);
            if texts.iter().any(|text| text.contains(digest.as_str())) {
                derived.push(DerivedDigest {
                    digest,
                    kind,
                    id: id.value.clone(),
                });
            }
        }
    }
    derived
}

/// Lowercase hex SHA-256 of a UTF-8 string.
#[must_use]
pub fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    lower_hex(&Sha256::digest(text.as_bytes()))
}

/// Digests of known preimages that occur in `texts`, named by preimage.
#[must_use]
pub fn preimage_digests(
    preimages: &[(&'static str, String)],
    texts: &[&str],
) -> Vec<DerivedDigest> {
    preimages
        .iter()
        .map(|(name, preimage)| (name, sha256_hex(preimage)))
        .filter(|(_, digest)| texts.iter().any(|text| text.contains(digest.as_str())))
        .map(|(name, digest)| DerivedDigest {
            digest,
            kind: name,
            id: String::new(),
        })
        .collect()
}

/// Pairs verified preimage digests by name.
fn preimage_classes(
    left: &SideInput<'_>,
    right: &SideInput<'_>,
) -> Result<Vec<ValueClass>, String> {
    let names = |side: &SideInput<'_>| -> Vec<&'static str> {
        side.preimages.iter().map(|(name, _)| *name).collect()
    };
    if names(left) != names(right) {
        return Err("sides declare different digest preimages".into());
    }
    let left_found = preimage_digests(&left.preimages, &left.texts);
    let right_found = preimage_digests(&right.preimages, &right.texts);
    let mut classes = Vec::new();
    for name in names(left) {
        let find = |found: &[DerivedDigest]| {
            found
                .iter()
                .find(|entry| entry.kind == name)
                .map(|entry| entry.digest.clone())
        };
        match (find(&left_found), find(&right_found)) {
            (Some(left_digest), Some(right_digest)) => classes.push(pair(
                &format!("sha256-of-{name}"),
                NormalizationCategory::GeneratedId,
                "digest verified as sha256 of the exact preimage the gate sent",
                left_digest,
                right_digest,
            )),
            (None, None) => {}
            _ => return Err(format!("sha256 of {name} occurs on one side only")),
        }
    }
    Ok(classes)
}

/// The JSON string form of a UUID's 7-character prefix, `"xxxxxxx"`. Only
/// this quoted form is normalized, so the replacement is bounded by quotes and
/// never reaches into other text.
fn quoted_short(uuid: &str) -> String {
    format!("\"{}\"", &uuid[..7])
}

const WALL_TIME_PREFIX: &str = "Wall time ";
const WALL_TIME_SUFFIX: &str = " seconds";

/// Every `Wall time \d+\.\d seconds` occurrence, in scan order with repeats.
/// Codex prints this measured duration of a code mode cell in the tool output
/// it sends back to the Responses API.
#[must_use]
pub fn wall_times(texts: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    for text in texts {
        let mut rest = *text;
        while let Some(at) = rest.find(WALL_TIME_PREFIX) {
            let after = &rest[at + WALL_TIME_PREFIX.len()..];
            let whole = after.bytes().take_while(u8::is_ascii_digit).count();
            let tail = &after[whole..];
            let matched = whole > 0
                && tail.as_bytes().first() == Some(&b'.')
                && tail.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
                && tail[2..].starts_with(WALL_TIME_SUFFIX);
            if matched {
                let end = at + WALL_TIME_PREFIX.len() + whole + 2 + WALL_TIME_SUFFIX.len();
                found.push(rest[at..end].to_owned());
                rest = &rest[end..];
            } else {
                rest = after;
            }
        }
    }
    found
}

/// One class for every Codex wall time, applied only in stub request bodies.
/// Discovery counts occurrences in every text, so a one-sided occurrence
/// anywhere fails; the comparison then checks the count and position of the
/// replaced tokens in each stub request body.
fn wall_time_class(
    left: &SideInput<'_>,
    right: &SideInput<'_>,
) -> Result<Option<ValueClass>, String> {
    let (left_times, right_times) = (wall_times(&left.texts), wall_times(&right.texts));
    if left_times.len() != right_times.len() {
        return Err(format!(
            "codex wall time occurrence count differs: left {} right {}",
            left_times.len(),
            right_times.len()
        ));
    }
    Ok((!left_times.is_empty()).then(|| ValueClass {
        id: "codex-wall-time".into(),
        category: NormalizationCategory::WallClock,
        reason: "Codex measured tool cell duration in a stub request body".into(),
        left: left_times,
        right: right_times,
        scope: Scope::StubRequests,
    }))
}

/// Every `"created": "<age>"` the pinned CLI printed, in scan order with
/// repeats, where `<age>` is exactly what the pinned `relativeTime`
/// (`packages/cli/src/commands/agent/ls.ts:38-47`) prints: `just now`,
/// `<digits> minutes ago`, `<digits> hours ago` or `<digits> days ago`. It is
/// always plural, even for 1, and never counts seconds, so `1 minute ago`,
/// `5 seconds ago` and every other rendering stay unmasked.
#[must_use]
pub fn relative_ages(texts: &[&str]) -> Vec<String> {
    const KEY: &str = "\"created\": \"";
    let mut found = Vec::new();
    for text in texts {
        let mut rest = *text;
        while let Some(at) = rest.find(KEY) {
            let after = &rest[at + KEY.len()..];
            let Some(close) = after.find('"') else {
                break;
            };
            let age = &after[..close];
            let is_age = age == "just now"
                || age.strip_suffix(" ago").is_some_and(|counted| {
                    counted.split_once(' ').is_some_and(|(count, unit)| {
                        !count.is_empty()
                            && count.bytes().all(|byte| byte.is_ascii_digit())
                            && matches!(unit, "minutes" | "hours" | "days")
                    })
                });
            if is_age {
                found.push(format!("{KEY}{age}\""));
            }
            rest = &after[close + 1..];
        }
    }
    found
}

/// One class for every rendered age, applied only in step stdout artifacts.
/// The pinned CLI renders it on both sides; a Spocky CLI would need its age
/// rendering proved separately with a fixed clock. Discovery counts the
/// occurrences in every text, so a one-sided occurrence fails; a target that
/// holds the age on one side only gets no rule and still differs.
fn relative_age_class(
    left: &SideInput<'_>,
    right: &SideInput<'_>,
) -> Result<Option<ValueClass>, String> {
    // The age text is the pinned CLI's rendering. Without that identity on
    // both sides there is no class: the ages stay raw and differ.
    if left.facts.client != PINNED_CLIENT || right.facts.client != PINNED_CLIENT {
        return Ok(None);
    }
    let (left_ages, right_ages) = (relative_ages(&left.texts), relative_ages(&right.texts));
    if left_ages.len() != right_ages.len() {
        return Err(format!(
            "cli relative age occurrence count differs: left {} right {}",
            left_ages.len(),
            right_ages.len()
        ));
    }
    Ok((!left_ages.is_empty()).then(|| ValueClass {
        id: "cli-relative-age".into(),
        category: NormalizationCategory::WallClock,
        reason: "pinned CLI relative age text of createdAt in step stdout".into(),
        left: left_ages,
        right: right_ages,
        scope: Scope::StepStdout,
    }))
}

/// The `["content-length","<n>"]` header literal of each Responses stub
/// request record, in record order, when it equals the UTF-8 byte length of
/// that record's raw body and the record's `seq` equals its position (so it
/// is the `stub/<seq>` artifact). Any other record yields `None`, so it gets
/// no class and its header stays literal.
fn verified_content_lengths(texts: &[&str]) -> Vec<Option<String>> {
    texts
        .iter()
        .filter_map(|text| match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(record))
                if record.get("method").is_some_and(Value::is_string)
                    && record.get("headers").is_some_and(Value::is_array) =>
            {
                Some(record)
            }
            _ => None,
        })
        .enumerate()
        .map(|(position, record)| {
            if record.get("seq").and_then(Value::as_u64) != u64::try_from(position).ok() {
                return None;
            }
            let body = record.get("body")?.as_str()?;
            let length = body.len().to_string();
            let declared = record["headers"].as_array()?.iter().any(|header| {
                header.as_array().is_some_and(|pair| {
                    pair.len() == 2
                        && pair[0] == "content-length"
                        && pair[1].as_str() == Some(length.as_str())
                })
            });
            declared.then(|| format!(r#"["content-length","{length}"]"#))
        })
        .collect()
}

/// Pairs each stub request's verified content-length header by record
/// position, applied only in that record's own `stub/<seq>` artifact.
/// Derived from `codex-wall-time`: the header varies only because an
/// approved clock value in the body varied in length, and the body itself is
/// still compared after normalization.
fn content_length_classes(left: &SideInput<'_>, right: &SideInput<'_>) -> Vec<ValueClass> {
    verified_content_lengths(&left.texts)
        .into_iter()
        .zip(verified_content_lengths(&right.texts))
        .enumerate()
        .filter_map(|(index, pair)| match pair {
            (Some(left_header), Some(right_header)) => Some(ValueClass {
                id: format!("stub-content-length-{}", index + 1),
                category: NormalizationCategory::WallClock,
                reason: "stub request content-length verified equal to the raw body byte length; varies only with codex-wall-time".into(),
                left: vec![left_header],
                right: vec![right_header],
                scope: Scope::Artifact(format!("stub/{index:03}")),
            }),
            _ => None,
        })
        .collect()
}

/// The key of a Paseo send receipt path `<dir>/agent-requests/<key>.json`,
/// where the key is 64 lower hex.
///
/// At the pinned commit only `MessageReceipts` writes this directory
/// (message-receipts/index.ts:27, key `sha256(JSON.stringify(["send",
/// agentId, messageId]))`); creation only reads legacy receipts there
/// (creation/index.ts:219-221). The `messageId` is a client
/// `crypto.randomUUID()` (daemon-client.ts:3444) that no captured artifact
/// holds, so the gate cannot verify the key. The receipts differential
/// (lane `p3_message_receipts`, `crates/spocky-message-receipts` tests) proves
/// the key derivation byte for byte against the pinned daemon with fixed
/// message ids, so masking the key here hides no derivation difference.
#[must_use]
pub fn receipt_key(path: &str) -> Option<&str> {
    let (directory, file) = path.rsplit_once('/')?;
    let key = file.strip_suffix(".json")?;
    (directory.rsplit('/').next() == Some("agent-requests")
        && key.len() == 64
        && key.bytes().all(|byte| Alphabet::LowerHex.contains(byte)))
    .then_some(key)
}

/// Send receipts among state texts (`<path>\n<content>`), as (key, fingerprint).
fn send_receipts(texts: &[&str]) -> Result<Vec<(String, String)>, String> {
    let mut receipts = Vec::new();
    for text in texts {
        let Some((path, content)) = text.split_once('\n') else {
            continue;
        };
        let Some(key) = receipt_key(path) else {
            continue;
        };
        let fingerprint = serde_json::from_str::<Value>(content)
            .ok()
            .and_then(|json| json.get("fingerprint")?.as_str().map(str::to_owned))
            .ok_or_else(|| format!("send receipt {path} has no fingerprint"))?;
        receipts.push((key.to_owned(), fingerprint));
    }
    Ok(receipts)
}

/// Pairs send receipt keys by their exact content fingerprint. Discovery
/// fails unless both sides hold the same set of distinct fingerprints, so a
/// receipt on one side only, or with different content, is never paired.
/// Only the file name `agent-requests/<key>.json` is replaced, and only in
/// receipt state files; the same hex anywhere else stays literal.
fn receipt_classes(left: &SideInput<'_>, right: &SideInput<'_>) -> Result<Vec<ValueClass>, String> {
    let (left_receipts, right_receipts) =
        (send_receipts(&left.texts)?, send_receipts(&right.texts)?);
    let fingerprints = |receipts: &[(String, String)]| {
        let mut sorted: Vec<String> = receipts.iter().map(|(_, print)| print.clone()).collect();
        sorted.sort_unstable();
        sorted
    };
    let left_prints = fingerprints(&left_receipts);
    if left_prints != fingerprints(&right_receipts) {
        return Err(format!(
            "send receipt fingerprints differ: left {} right {}",
            left_receipts.len(),
            right_receipts.len()
        ));
    }
    if left_prints.windows(2).any(|window| window[0] == window[1]) {
        return Err("two send receipts share a fingerprint".into());
    }
    Ok(left_receipts
        .iter()
        .filter_map(|(left_key, print)| {
            let (right_key, _) = right_receipts.iter().find(|(_, other)| other == print)?;
            Some((left_key, right_key))
        })
        .enumerate()
        .map(|(index, (left_key, right_key))| ValueClass {
            id: format!("send-receipt-key-{}", index + 1),
            category: NormalizationCategory::GeneratedId,
            reason: "send receipt file name over a client random message id, paired by exact content fingerprint".into(),
            left: vec![format!("agent-requests/{left_key}.json")],
            right: vec![format!("agent-requests/{right_key}.json")],
            scope: Scope::ReceiptNames,
        })
        .collect())
}

/// The targets a value class may be applied in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every compared text.
    Every,
    /// Only Responses stub request bodies, the `stub/<nnn>` artifacts.
    StubRequests,
    /// Only step stdout artifacts, named `step-<nn>-<name>/stdout`.
    StepStdout,
    /// Only the one artifact with this name.
    Artifact(String),
    /// Only send receipt state files, named
    /// `state/<dir>/agent-requests/{send-receipt-key}.json#<n>`.
    ReceiptNames,
}

impl Scope {
    fn admits(&self, target: &NormalizationTarget) -> bool {
        match self {
            Self::Every => true,
            Self::StepStdout => matches!(
                target,
                NormalizationTarget::Artifact(name) if name.ends_with("/stdout")
            ),
            Self::Artifact(only) => {
                matches!(target, NormalizationTarget::Artifact(name) if name == only)
            }
            Self::ReceiptNames => matches!(
                target,
                NormalizationTarget::Artifact(name) | NormalizationTarget::State(name)
                    if name.contains("agent-requests/{send-receipt-key}.json#")
            ),
            Self::StubRequests => matches!(
                target,
                NormalizationTarget::Artifact(name)
                    if name.strip_prefix("stub/").is_some_and(|number| {
                        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
                    })
            ),
        }
    }
}

/// One value class with the exact literals to replace on each side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueClass {
    pub id: String,
    pub category: NormalizationCategory,
    pub reason: String,
    pub left: Vec<String>,
    pub right: Vec<String>,
    pub scope: Scope,
}

/// One side's inputs to rule discovery.
#[derive(Debug, Clone)]
pub struct SideInput<'a> {
    pub facts: &'a SideFacts,
    /// Every compared text in canonical scan order.
    pub texts: Vec<&'a str>,
    /// Generated secrets read from known files, as (class id, value).
    pub extracted: Vec<(String, String)>,
    /// Exact `JSON.stringify` preimages the gate knows were hashed with
    /// SHA-256 (for example creation request fingerprints), as (name, preimage).
    pub preimages: Vec<(&'static str, String)>,
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
        scope: Scope::Every,
    }
}

/// Extracted secrets (allowlisted), disposable-root forms, and listen addresses.
fn fixed_classes(left: &SideInput<'_>, right: &SideInput<'_>) -> Result<Vec<ValueClass>, String> {
    let (lf, rf) = (left.facts, right.facts);
    let mut classes = Vec::new();
    for (class_id, _) in left.extracted.iter().chain(&right.extracted) {
        if !EXTRACTED_CLASSES.contains(&class_id.as_str()) {
            return Err(format!("extracted class {class_id} is not allowlisted"));
        }
    }
    for class_id in EXTRACTED_CLASSES {
        let find = |side: &SideInput<'_>| -> Vec<String> {
            side.extracted
                .iter()
                .filter(|(id, _)| id == class_id)
                .map(|(_, value)| value.clone())
                .collect()
        };
        match (find(left).as_slice(), find(right).as_slice()) {
            ([], []) => {}
            ([left_value], [right_value]) => classes.push(pair(
                class_id,
                NormalizationCategory::GeneratedId,
                "generated secret read from its known file",
                left_value.clone(),
                right_value.clone(),
            )),
            _ => {
                return Err(format!(
                    "extracted secret {class_id} differs in presence or count"
                ));
            }
        }
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

/// Paired classes derived from one generated id: its digests and short prefix.
fn derived_classes(
    class: &str,
    left: (&SideInput<'_>, &FoundId, &[DerivedDigest]),
    right: (&SideInput<'_>, &FoundId, &[DerivedDigest]),
    digests: &mut Vec<ValueClass>,
    shorts: &mut Vec<ValueClass>,
) -> Result<(), String> {
    for kind in DIGEST_KINDS {
        let from = |derived: &[DerivedDigest], id: &str| {
            derived
                .iter()
                .find(|entry| entry.kind == kind && entry.id == id)
                .map(|entry| entry.digest.clone())
        };
        match (from(left.2, &left.1.value), from(right.2, &right.1.value)) {
            (Some(left_digest), Some(right_digest)) => digests.push(pair(
                &format!("sha256-{kind}-of-{class}"),
                NormalizationCategory::GeneratedId,
                "content-addressed name verified as sha256 of [kind, id]",
                left_digest,
                right_digest,
            )),
            (None, None) => {}
            _ => {
                return Err(format!(
                    "sha256 {kind} digest of {class} exists on one side only"
                ));
            }
        }
    }
    if left.1.shape == "uuid" {
        let (left_short, right_short) = (quoted_short(&left.1.value), quoted_short(&right.1.value));
        let present =
            |side: &SideInput<'_>, short: &str| side.texts.iter().any(|text| text.contains(short));
        match (present(left.0, &left_short), present(right.0, &right_short)) {
            (true, true) => shorts.push(pair(
                &format!("short7-of-{class}"),
                NormalizationCategory::GeneratedId,
                "quoted 7-character prefix of a paired UUID",
                left_short,
                right_short,
            )),
            (false, false) => {}
            _ => return Err(format!("short prefix of {class} exists on one side only")),
        }
    }
    Ok(())
}

/// Pairs generated ids, their verified digests, and their standalone short
/// prefixes. Returns (digest classes, id classes, short-prefix classes).
fn id_classes(
    left: &SideInput<'_>,
    right: &SideInput<'_>,
    shapes: &[IdShape],
) -> Result<[Vec<ValueClass>; 3], String> {
    let left_ids = distinct_ids(&left.texts, shapes);
    let right_ids = distinct_ids(&right.texts, shapes);
    if left_ids.len() != right_ids.len() {
        return Err(format!(
            "generated id count differs: left {} right {}",
            left_ids.len(),
            right_ids.len()
        ));
    }
    let left_derived = derived_digests(&left_ids, &left.texts);
    let right_derived = derived_digests(&right_ids, &right.texts);
    let mut digests = Vec::new();
    let mut ids = Vec::new();
    let mut shorts = Vec::new();
    let mut per_shape: Vec<(&str, usize)> = Vec::new();
    for (index, (left_id, right_id)) in left_ids.iter().zip(&right_ids).enumerate() {
        if left_id.shape != right_id.shape {
            return Err(format!(
                "generated id {} has shape {} on the left and {} on the right",
                index + 1,
                left_id.shape,
                right_id.shape
            ));
        }
        let number = if let Some((_, count)) = per_shape
            .iter_mut()
            .find(|(shape, _)| *shape == left_id.shape)
        {
            *count += 1;
            *count
        } else {
            per_shape.push((left_id.shape, 1));
            1
        };
        let class = format!("generated-id-{}-{number}", left_id.shape);
        derived_classes(
            &class,
            (left, left_id, &left_derived),
            (right, right_id, &right_derived),
            &mut digests,
            &mut shorts,
        )?;
        ids.push(pair(
            &class,
            NormalizationCategory::GeneratedId,
            "generated id paired by first appearance and shape",
            left_id.value.clone(),
            right_id.value.clone(),
        ));
    }
    Ok([digests, ids, shorts])
}

/// Builds every value class for a gate, in application order.
///
/// # Errors
///
/// Returns a message for any presence, count, shape, or format difference
/// between the sides listed in the module documentation.
pub fn value_classes(
    left: &SideInput<'_>,
    right: &SideInput<'_>,
    shapes: &[IdShape],
) -> Result<Vec<ValueClass>, String> {
    let mut classes = fixed_classes(left, right)?;
    classes.extend(preimage_classes(left, right)?);
    classes.extend(receipt_classes(left, right)?);
    let [digests, ids, shorts] = id_classes(left, right, shapes)?;
    classes.extend(digests);
    classes.extend(ids);
    classes.extend(shorts);

    let (lf, rf) = (left.facts, right.facts);
    // Field-owned instants first: their literals include the key, so the
    // generic classes below can no longer claim a part of them.
    let left_fields = independent_clock_values(&left.texts, lf.window_start_ms, lf.window_end_ms);
    let right_fields = independent_clock_values(&right.texts, rf.window_start_ms, rf.window_end_ms);
    let mut field_formats: Vec<&str> = Vec::new();
    for instant in left_fields.iter().chain(&right_fields) {
        if !field_formats.contains(&instant.format.as_str()) {
            field_formats.push(&instant.format);
        }
    }
    for format in field_formats {
        classes.extend(wall_clock_classes(
            format,
            &of_format(&left_fields, format),
            &of_format(&right_fields, format),
        )?);
    }
    let left_clock = wall_clock_values(&left.texts, lf.window_start_ms, lf.window_end_ms);
    let right_clock = wall_clock_values(&right.texts, rf.window_start_ms, rf.window_end_ms);
    let mut formats: Vec<&str> = Vec::new();
    for instant in left_clock.iter().chain(&right_clock) {
        if !formats.contains(&instant.format.as_str()) {
            formats.push(&instant.format);
        }
    }
    for format in formats {
        classes.extend(wall_clock_classes(
            format,
            &of_format(&left_clock, format),
            &of_format(&right_clock, format),
        )?);
    }
    classes.extend(wall_time_class(left, right)?);
    classes.extend(relative_age_class(left, right)?);
    classes.extend(content_length_classes(left, right));
    Ok(classes)
}

/// Replaces verified derived digests with `{sha256:<kind>}` and every other
/// generated id with `{<shape>}`, for ordering files whose names are generated.
#[must_use]
pub fn mask(text: &str, shapes: &[IdShape], derived: &[DerivedDigest]) -> String {
    let mut text = text.to_owned();
    for entry in derived {
        text = text.replace(&entry.digest, &format!("{{sha256:{}}}", entry.kind));
    }
    let bytes = text.as_bytes();
    let mut masked = String::with_capacity(text.len());
    let mut index = 0;
    let mut copied = 0;
    while index < bytes.len() {
        if let Some((shape, end)) = shapes
            .iter()
            .find_map(|shape| id_at(bytes, index, *shape).map(|end| (shape.name(), end)))
        {
            masked.push_str(&text[copied..index]);
            masked.push('{');
            masked.push_str(shape);
            masked.push('}');
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

fn rule_suffix(target: &NormalizationTarget) -> String {
    match target {
        NormalizationTarget::StructuredJsonPointer(pointer) => format!("json:{pointer}"),
        NormalizationTarget::Stdout => "stdout".into(),
        NormalizationTarget::Stderr => "stderr".into(),
        NormalizationTarget::Artifact(name) => format!("artifact:{name}"),
        NormalizationTarget::State(name) => format!("state:{name}"),
    }
}

/// Emits one exact-value rule per (class, target) where the class occurs on
/// either side, simulating sequential application so a later class never
/// claims text an earlier class already replaced.
///
/// Values are the union of both sides' literals, longest first. A class that
/// occurs in a target on one side only gets no rule there, so the raw value
/// stays and the comparison reports that target as different.
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
    // Only targets both sides have. A text present on one side only is a
    // plain difference the comparison reports; a rule there would turn it
    // into a normalization miss and hide which artifact differs.
    let mut targets: Vec<NormalizationTarget> = Vec::new();
    for text in left {
        if right.iter().any(|other| other.target == text.target) && !targets.contains(&text.target)
        {
            targets.push(text.target.clone());
        }
    }

    let mut rules = Vec::new();
    for class in classes {
        // A value both sides hold identically is not generated per run; it
        // stays literal and must match byte for byte.
        if class.left == class.right {
            continue;
        }
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
        for target in targets.iter().filter(|target| class.scope.admits(target)) {
            let key = target_key(target);
            // A rule only where the class occurs on BOTH sides of this target.
            // A value on one side only stays raw and shows as a content
            // difference; a rule there would abort the whole comparison with
            // a normalization miss and hide every other difference.
            let next: Vec<Option<String>> = [&left_state, &right_state]
                .iter()
                .map(|state| {
                    state
                        .iter()
                        .find(|(name, _)| *name == key)
                        .and_then(|(_, text)| {
                            let (next, matched) = replace_all(text, &values, &token);
                            matched.then_some(next)
                        })
                })
                .collect();
            let hit = next.iter().all(Option::is_some);
            if hit {
                for (state, next) in [&mut left_state, &mut right_state].into_iter().zip(next) {
                    if let (Some((_, text)), Some(next)) =
                        (state.iter_mut().find(|(name, _)| *name == key), next)
                    {
                        *text = next;
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use spocky_differential::{
        Artifact, ExecutionCounts, Observation, ObservationSlot, Scenario, compare_observations,
    };
    use std::collections::BTreeMap;

    const UUID_A: &str = "0199a3c4-1b2c-7d3e-8f40-123456789abc";
    const UUID_B: &str = "0299a3c4-1b2c-7d3e-8f40-cba987654321";
    const UUID_C: &str = "11111111-2222-4333-8444-555555555555";
    const UUID_D: &str = "66666666-7777-4888-9999-aaaaaaaaaaaa";
    const WKS_A: &str = "wks_0123456789abcdef";
    const WKS_B: &str = "wks_fedcba9876543210";

    fn facts(root: &str, daemon_port: u16, stub_port: u16) -> SideFacts {
        SideFacts {
            client: PINNED_CLIENT.to_owned(),
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

    const COUNTS: ExecutionCounts = ExecutionCounts {
        fixtures: 1,
        assertions: 1,
    };

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

    fn input<'a>(
        facts: &'a SideFacts,
        texts: &'a [Text],
        extracted: Vec<(String, String)>,
    ) -> SideInput<'a> {
        SideInput {
            facts,
            texts: texts.iter().map(|text| text.text.as_str()).collect(),
            extracted,
            preimages: texts
                .iter()
                .filter_map(|text| {
                    text.text
                        .split_once("preimage=")
                        .map(|(_, preimage)| ("request", preimage.to_owned()))
                })
                .collect(),
        }
    }

    /// Runs discovery and the real differential comparison: `Ok(equivalent)`,
    /// or `Err` when discovery fails or a rule misses one side.
    fn equivalent_with(
        left: &[Text],
        right: &[Text],
        left_extracted: Vec<(String, String)>,
        right_extracted: Vec<(String, String)>,
    ) -> Result<bool, String> {
        let left_facts = facts("/private/tmp/spocky-p3-g1-0000000000a", 41001, 42001);
        let right_facts = facts("/private/tmp/spocky-p3-g1-0000000000b", 41002, 42002);
        let classes = value_classes(
            &input(&left_facts, left, left_extracted),
            &input(&right_facts, right, right_extracted),
            &SLICE_SHAPES,
        )?;
        let rules = rules_for(&classes, left, right);
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

    fn equivalent(left: &[Text], right: &[Text]) -> Result<bool, String> {
        equivalent_with(left, right, Vec::new(), Vec::new())
    }

    fn one(text: String) -> Vec<Text> {
        vec![artifact("a", text)]
    }

    #[test]
    fn every_slice_shape_matches_its_own_ids_only() {
        let cases = [
            ("codex-tools-id", "at_48e072de-185d-5a9f-845b-30056570557f"),
            (
                "codex-message-id",
                "msg_01a0f7f3-0f0a-73c0-8794-b52e6b1eb1d4",
            ),
            (
                "codex-tool-output-id",
                "ctco_01a0faad-0606-77d2-9304-cfbdd75d8a33",
            ),
            (
                "permission-id",
                "permission-exec-3a081679-7069-4583-9c6b-933077179007",
            ),
            ("uuid", UUID_A),
            ("workspace-id", WKS_A),
            ("project-id", "prj_ac1ccce5517ad5ef"),
            ("client-id", "cid_c5f12a43fd4a45a886205cc6a41030d7"),
            ("server-id", "srv_mxf4gW1rH0OU"),
        ];
        for (shape, value) in cases {
            let quoted = format!("\"{value}\"");
            assert_eq!(
                distinct_ids(&[&quoted], &SLICE_SHAPES),
                vec![FoundId {
                    shape,
                    value: value.to_owned()
                }],
                "{shape}"
            );
            let glued = format!("x{value} {value}x {value}_");
            assert!(
                distinct_ids(&[&glued], &SLICE_SHAPES).is_empty(),
                "{shape} glued"
            );
            let truncated = &value[..value.len() - 1];
            assert!(
                distinct_ids(&[truncated], &SLICE_SHAPES).is_empty(),
                "{shape} truncated"
            );
        }
        let upper = UUID_A.to_uppercase();
        assert!(distinct_ids(&[&upper], &SLICE_SHAPES).is_empty());
        let hex64 = kind_digest("agent", UUID_A);
        assert!(distinct_ids(&[&hex64], &SLICE_SHAPES).is_empty());
    }

    #[test]
    fn finds_ids_in_first_appearance_order() {
        let text = format!("a {UUID_B} b {WKS_A} c {UUID_A} {UUID_B}");
        let values: Vec<String> = distinct_ids(&[&text], &SLICE_SHAPES)
            .into_iter()
            .map(|id| id.value)
            .collect();
        assert_eq!(values, vec![UUID_B, WKS_A, UUID_A]);
    }

    #[test]
    fn wall_clock_only_inside_window_with_format() {
        // 2026-10-01T13:51:43.463Z == 1790862703463 ms.
        let text = "2026-10-01T13:51:43.463Z 2020-01-02T03:04:05Z 1790862703463 1790862703 \
                    179086270346 17908627034630 x1790862703463 2026-10-01T13:51:44Z \
                    2026-10-01T13:51:43.463Z";
        let found = wall_clock_values(&[text], 1_790_862_700_000, 1_790_862_710_000);
        let triples: Vec<(&str, &str, u64)> = found
            .iter()
            .map(|instant| {
                (
                    instant.format.as_str(),
                    instant.value.as_str(),
                    instant.unix_ms,
                )
            })
            .collect();
        assert_eq!(
            triples,
            vec![
                ("iso-frac3", "2026-10-01T13:51:43.463Z", 1_790_862_703_463),
                ("epoch-ms", "1790862703463", 1_790_862_703_463),
                ("epoch-s", "1790862703", 1_790_862_703_000),
                ("iso-frac0", "2026-10-01T13:51:44Z", 1_790_862_704_000),
                ("iso-frac3", "2026-10-01T13:51:43.463Z", 1_790_862_703_463),
            ]
        );
        assert!(wall_clock_values(&[text], 1_800_000_000_000, 1_800_000_001_000).is_empty());
    }

    #[test]
    fn iso_fractions_and_seconds_parse_exactly() {
        assert_eq!(iso_at(b"1970-01-01T00:00:00Z", 0), Some((20, 0, 0)));
        assert_eq!(iso_at(b"1970-01-01T00:00:01.5Z", 0), Some((22, 1500, 1)));
        assert_eq!(
            iso_at(b"2000-02-29T00:00:00.123456Z", 0),
            Some((27, 951_782_400_123, 6))
        );
        assert_eq!(iso_at(b"2000-13-01T00:00:00Z", 0), None);
        assert_eq!(iso_at(b"2000-01-01T00:00:00+01:00", 0), None);
    }

    #[test]
    fn equivalent_sides_normalize_to_identical_text() {
        let left = one(format!(
            "{UUID_A} {WKS_A} /private/tmp/spocky-p3-g1-0000000000a/project /tmp/spocky-p3-g1-0000000000a/x \
             private-tmp-spocky-p3-g1-0000000000a-project 127.0.0.1:41001 127.0.0.1:42001 \
             2026-10-01T13:51:43.463Z {UUID_C} {UUID_A} short=\"{}\"",
            &UUID_A[..7]
        ));
        let right = one(format!(
            "{UUID_B} {WKS_B} /private/tmp/spocky-p3-g1-0000000000b/project /tmp/spocky-p3-g1-0000000000b/x \
             private-tmp-spocky-p3-g1-0000000000b-project 127.0.0.1:41002 127.0.0.1:42002 \
             2026-10-01T13:51:44.001Z {UUID_D} {UUID_B} short=\"{}\"",
            &UUID_B[..7]
        ));
        assert_eq!(equivalent(&left, &right), Ok(true));
    }

    #[test]
    fn swapped_id_identity_still_differs() {
        // Left reuses its first id; right mints a new one at the same position.
        let left = one(format!("{UUID_A} {UUID_C} {UUID_A}"));
        let right = one(format!("{UUID_B} {UUID_D} {UUID_D}"));
        assert_eq!(equivalent(&left, &right), Ok(false));
    }

    #[test]
    fn different_generated_id_counts_fail_discovery() {
        let left = one(format!("{UUID_A} {UUID_C}"));
        let right = one(UUID_B.into());
        assert!(equivalent(&left, &right).is_err());
    }

    #[test]
    fn id_shape_mismatch_at_a_position_fails_discovery() {
        // A workspace id where the other side has a UUID never shares a token.
        let left = one(format!("id={UUID_A}"));
        let right = one(format!("id={WKS_B}"));
        let error = equivalent(&left, &right).unwrap_err();
        assert!(error.contains("shape"), "{error}");
    }

    #[test]
    fn wall_clock_format_mismatch_fails_discovery() {
        let left = one("at 2026-10-01T13:51:43.463Z".into());
        let right = one("at 2026-10-01T13:51:43.463123Z".into());
        let error = equivalent(&left, &right).unwrap_err();
        assert!(error.contains("iso-frac"), "{error}");
        let left = one("at 1790862703463".into());
        let right = one("at 1790862703".into());
        assert!(equivalent(&left, &right).is_err());
    }

    #[test]
    fn wall_clock_outside_window_must_match_exactly() {
        let left = one("fixture 2020-01-02T03:04:05Z".into());
        let right = one("fixture 2020-01-02T03:04:06Z".into());
        assert_eq!(equivalent(&left, &right), Ok(false));
    }

    #[test]
    fn realpath_and_tmp_alias_stay_distinguishable() {
        let left = one("/private/tmp/spocky-p3-g1-0000000000a/p".into());
        let right = one("/tmp/spocky-p3-g1-0000000000b/p".into());
        // Each form occurs on one side only: no rule, so the raw paths differ.
        assert_eq!(equivalent(&left, &right), Ok(false));
    }

    #[test]
    fn value_present_on_one_side_only_fails_comparison() {
        let left = one("listening on 127.0.0.1:41001".into());
        let right = one("listening".into());
        // No rule for a one-sided value; the comparison runs and differs.
        assert_eq!(equivalent(&left, &right), Ok(false));
    }

    #[test]
    fn untouched_differences_survive_normalization() {
        let left = one(format!("{UUID_A} status=completed"));
        let right = one(format!("{UUID_B} status=error"));
        assert_eq!(equivalent(&left, &right), Ok(false));
    }

    #[test]
    fn kind_digest_matches_paseo_creation_digest() {
        use sha2::{Digest, Sha256};
        assert_eq!(
            kind_digest("agent", "a09a900c-7425-4446-93ea-66f22d55593e"),
            lower_hex(&Sha256::digest(
                br#"["agent","a09a900c-7425-4446-93ea-66f22d55593e"]"#
            ))
        );
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
    fn derived_digest_of_a_different_kind_fails() {
        let left = one(format!("{UUID_A} {}", kind_digest("agent", UUID_A)));
        let right = one(format!("{UUID_B} {}", kind_digest("workspace", UUID_B)));
        assert!(equivalent(&left, &right).is_err());
    }

    #[test]
    fn underived_digest_is_never_normalized() {
        // Content hashes that are not sha256([kind, id]) of a paired id must
        // match byte for byte; differing ones are a mismatch.
        let left = one(format!("{UUID_A} {}", kind_digest("other", UUID_A)));
        let right = one(format!("{UUID_B} {}", kind_digest("other", UUID_B)));
        assert_eq!(equivalent(&left, &right), Ok(false));
        let same = kind_digest("other", UUID_C);
        let left = one(format!("{UUID_A} {same}"));
        let right = one(format!("{UUID_B} {same}"));
        assert_eq!(equivalent(&left, &right), Ok(true));
    }

    #[test]
    fn preimage_digest_is_verified_not_assumed() {
        let fingerprint = |preimage: &str| sha256_hex(preimage);
        let left_pre = format!("{{\"workspaceId\":\"{WKS_A}\"}}");
        let right_pre = format!("{{\"workspaceId\":\"{WKS_B}\"}}");
        let left = vec![
            artifact("a", format!("{WKS_A} fp={}", fingerprint(&left_pre))),
            artifact("p", format!("preimage={left_pre}")),
        ];
        let right = vec![
            artifact("a", format!("{WKS_B} fp={}", fingerprint(&right_pre))),
            artifact("p", format!("preimage={right_pre}")),
        ];
        assert_eq!(equivalent(&left, &right), Ok(true));
        // A fingerprint computed over a different request is not normalized.
        let wrong = vec![
            artifact("a", format!("{WKS_B} fp={}", fingerprint("{}"))),
            artifact("p", format!("preimage={right_pre}")),
        ];
        assert!(equivalent(&left, &wrong).is_err());
    }

    #[test]
    fn short_prefix_is_derived_and_checked() {
        let left = one(format!("{UUID_A} short \"{}\"", &UUID_A[..7]));
        let right = one(format!("{UUID_B} short \"{}\"", &UUID_B[..7]));
        assert_eq!(equivalent(&left, &right), Ok(true));
        // A short id that is not the prefix of the paired UUID stays literal.
        let wrong = one(format!("{UUID_B} short \"{}\"", &UUID_D[..7]));
        assert!(equivalent(&left, &wrong).is_err());
        // An eight-character prefix is a different format and is not derived.
        let long = one(format!("{UUID_B} short \"{}\"", &UUID_B[..8]));
        assert!(equivalent(&left, &long).is_err());
        // Unquoted prefixes are never normalized, so the replace stays bounded.
        let left = one(format!("{UUID_A} short {}", &UUID_A[..7]));
        let right = one(format!("{UUID_B} short {}", &UUID_B[..7]));
        assert_eq!(equivalent(&left, &right), Ok(false));
    }

    #[test]
    fn constant_uuids_are_never_normalized() {
        let nil = "00000000-0000-0000-0000-000000000000";
        let max = "ffffffff-ffff-ffff-ffff-ffffffffffff";
        let v1 = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";
        assert!(distinct_ids(&[nil, max, v1], &SLICE_SHAPES).is_empty());
        // A differing constant UUID must fail the gate.
        let left = one(format!("{UUID_A} parent {nil}"));
        let right = one(format!("{UUID_B} parent {max}"));
        assert_eq!(equivalent(&left, &right), Ok(false));
        let wrong_variant = "0199a3c4-1b2c-7d3e-cf40-123456789abc";
        assert!(distinct_ids(&[wrong_variant], &SLICE_SHAPES).is_empty());
    }

    #[test]
    fn identical_values_on_both_sides_emit_no_rule() {
        let shared = UUID_C;
        let left = one(format!("{UUID_A} {shared}"));
        let right = one(format!("{UUID_B} {shared}"));
        let left_facts = facts("/private/tmp/a", 1, 2);
        let right_facts = facts("/private/tmp/b", 3, 4);
        let classes = value_classes(
            &input(&left_facts, &left, Vec::new()),
            &input(&right_facts, &right, Vec::new()),
            &SLICE_SHAPES,
        )
        .unwrap();
        let rules = rules_for(&classes, &left, &right);
        assert!(
            rules
                .iter()
                .all(|rule| !rule.exact_values.contains(&shared.to_owned()))
        );
        assert_eq!(equivalent(&left, &right), Ok(true));
    }

    #[test]
    fn wall_clock_pairs_instants_and_keeps_equality_structure() {
        let early = "2026-10-01T13:51:43.463Z";
        let late = "2026-10-01T13:51:44.001Z";
        let left = one(format!("c={early} u={late}"));
        let right = one("c=2026-10-01T13:51:45.100Z u=2026-10-01T13:51:46.200Z".into());
        assert_eq!(equivalent(&left, &right), Ok(true));
        // createdAt == updatedAt on one side only, far apart on the other: fails.
        let equal = one(format!("c={early} u={early}"));
        let error = equivalent(&equal, &right).unwrap_err();
        assert!(error.contains("equality structure"), "{error}");
        // Same-millisecond merge: the other side's instants are 1 ms apart.
        let adjacent = one("c=2026-10-01T13:51:45.100Z u=2026-10-01T13:51:45.101Z".into());
        assert_eq!(equivalent(&equal, &adjacent), Ok(true));
        // A different number of occurrences fails.
        let three = one(format!("c={early} u={late} again={late}"));
        assert!(equivalent(&left, &three).is_err());
        // Reusing the first instant versus the second is a mismatch.
        let mixed = one(format!("c={early} u={late} again={early}"));
        let other = one(format!("c={early} u={late} again={late}"));
        assert!(equivalent(&mixed, &other).is_err());
        // A format the other side lacks fails discovery.
        let seconds = one("c=2026-10-01T13:51:45Z u=2026-10-01T13:51:46Z".into());
        assert!(equivalent(&left, &seconds).is_err());
    }

    #[test]
    fn mask_hides_ids_by_shape_and_names_derived_kinds() {
        let found = distinct_ids(&[UUID_A], &SLICE_SHAPES);
        let digest = kind_digest("agent", UUID_A);
        let text = format!("creations/{digest}.claim {UUID_A} {WKS_A}");
        let derived = derived_digests(&found, &[&text]);
        assert_eq!(
            mask(&text, &SLICE_SHAPES, &derived),
            "creations/{sha256:agent}.claim {uuid} {workspace-id}"
        );
    }

    #[test]
    fn root_slug_matches_paseo_cwd_slug() {
        assert_eq!(
            root_slug("/private/tmp/spocky-p3-g1-0199a3c41b2"),
            "private-tmp-spocky-p3-g1-0199a3c41b2"
        );
    }

    #[test]
    fn extracted_secrets_are_allowlisted_and_paired() {
        let left = one("pub=".into());
        let right = one("bup=".into());
        let secret = |value: &str| vec![("daemon-public-key".to_owned(), value.to_owned())];
        assert_eq!(
            equivalent_with(&left, &right, secret("pub="), secret("bup=")),
            Ok(true)
        );
        assert!(equivalent_with(&left, &right, secret("pub="), Vec::new()).is_err());
        let other = vec![("anything".to_owned(), "pub=".to_owned())];
        let error = equivalent_with(&left, &right, other.clone(), other).unwrap_err();
        assert!(error.contains("allowlisted"), "{error}");
    }

    #[test]
    fn one_sided_targets_get_no_rule_and_still_differ() {
        let left = vec![
            artifact("shared", UUID_A.into()),
            artifact("extra", UUID_A.into()),
        ];
        let right = vec![artifact("shared", UUID_B.into())];
        let left_facts = facts("/private/tmp/a", 1, 2);
        let right_facts = facts("/private/tmp/b", 3, 4);
        let classes = value_classes(
            &input(&left_facts, &left, Vec::new()),
            &input(&right_facts, &right, Vec::new()),
            &SLICE_SHAPES,
        )
        .unwrap();
        let rules = rules_for(&classes, &left, &right);
        assert_eq!(
            rules
                .iter()
                .map(|rule| rule.id.as_str())
                .collect::<Vec<_>>(),
            vec!["generated-id-uuid-1@artifact:shared"]
        );
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
        let left_facts = facts("/private/tmp/a", 1, 2);
        let right_facts = facts("/private/tmp/b", 3, 4);
        let classes = value_classes(
            &input(&left_facts, &left, Vec::new()),
            &input(&right_facts, &right, Vec::new()),
            &SLICE_SHAPES,
        )
        .unwrap();
        let rules = rules_for(&classes, &left, &right);
        assert_eq!(
            rules
                .iter()
                .map(|rule| (rule.id.as_str(), rule.exact_values.clone()))
                .collect::<Vec<_>>(),
            vec![(
                "generated-id-uuid-1@artifact:ids",
                vec![UUID_A.to_owned(), UUID_B.to_owned()]
            )]
        );
    }

    #[test]
    fn wall_time_matches_only_the_exact_form() {
        let text = "Wall time 3.4 seconds\nWall time 12.0 seconds Wall time 3 seconds \
                    Wall time 3.45 seconds Wall time .4 seconds Wall time 3.4 second";
        assert_eq!(
            wall_times(&[text]),
            vec!["Wall time 3.4 seconds", "Wall time 12.0 seconds"]
        );
    }

    #[test]
    fn codex_wall_time_normalizes_only_in_stub_requests() {
        let body =
            |time: &str| format!(r#"{{"output":"Script completed\nWall time {time} seconds"}}"#);
        let left = vec![artifact("stub/001", body("3.4"))];
        let right = vec![artifact("stub/001", body("3.9"))];
        assert_eq!(equivalent(&left, &right), Ok(true));
        let left = vec![artifact("step-01-logs/stdout", body("3.4"))];
        let right = vec![artifact("step-01-logs/stdout", body("3.9"))];
        assert_eq!(equivalent(&left, &right), Ok(false));
    }

    #[test]
    fn one_sided_codex_wall_time_fails_discovery() {
        let left = vec![artifact("stub/001", "Wall time 3.4 seconds".into())];
        let right = vec![artifact("stub/001", "no timing".into())];
        assert!(equivalent(&left, &right).is_err());
    }

    #[test]
    fn extra_codex_wall_time_in_one_body_still_differs() {
        let left = vec![
            artifact(
                "stub/001",
                "Wall time 3.4 seconds Wall time 5.0 seconds".into(),
            ),
            artifact("stub/002", "none".into()),
        ];
        let right = vec![
            artifact("stub/001", "Wall time 3.9 seconds".into()),
            artifact("stub/002", "none Wall time 7.5 seconds".into()),
        ];
        assert_eq!(equivalent(&left, &right), Ok(false));
    }

    fn receipt(number: usize, key: &str, fingerprint: &str) -> Text {
        artifact(
            &format!("state/paseo-home/agent-requests/{{send-receipt-key}}.json#{number}"),
            format!(
                "paseo-home/agent-requests/{key}.json\n{{\"fingerprint\":\"{fingerprint}\",\"state\":\"completed\"}}"
            ),
        )
    }

    #[test]
    fn receipt_key_is_only_an_agent_requests_hex64_name() {
        let key = sha256_hex("k");
        assert_eq!(
            receipt_key(&format!("paseo-home/agent-requests/{key}.json")),
            Some(key.as_str())
        );
        assert_eq!(
            receipt_key(&format!("paseo-home/creations/{key}.json")),
            None
        );
        assert_eq!(receipt_key(&format!("agent-requests/{key}.txt")), None);
        assert_eq!(
            receipt_key(&format!("agent-requests/{}.json", &key[1..])),
            None
        );
        let upper = key.to_uppercase();
        assert_eq!(receipt_key(&format!("agent-requests/{upper}.json")), None);
    }

    #[test]
    fn send_receipt_keys_pair_by_exact_fingerprint() {
        let (one_print, two_print) = (sha256_hex("f1"), sha256_hex("f2"));
        let left = vec![
            receipt(1, &sha256_hex("k1"), &one_print),
            receipt(2, &sha256_hex("k2"), &two_print),
        ];
        let right = vec![
            receipt(1, &sha256_hex("k3"), &one_print),
            receipt(2, &sha256_hex("k4"), &two_print),
        ];
        assert_eq!(equivalent(&left, &right), Ok(true));
        // Right lists the receipts in the other order: keys still pair by
        // fingerprint, so each artifact keeps its content difference.
        let swapped = vec![
            receipt(1, &sha256_hex("k4"), &two_print),
            receipt(2, &sha256_hex("k3"), &one_print),
        ];
        assert_eq!(equivalent(&left, &swapped), Ok(false));
    }

    #[test]
    fn send_receipt_on_one_side_only_fails_discovery() {
        let print = sha256_hex("f1");
        let left = vec![
            receipt(1, &sha256_hex("k1"), &print),
            receipt(2, &sha256_hex("k2"), &sha256_hex("f2")),
        ];
        let right = vec![receipt(1, &sha256_hex("k3"), &print)];
        let error = equivalent(&left, &right).unwrap_err();
        assert!(
            error.contains("send receipt fingerprints differ"),
            "{error}"
        );
        let changed = vec![
            receipt(1, &sha256_hex("k3"), &print),
            receipt(2, &sha256_hex("k4"), &sha256_hex("f3")),
        ];
        assert!(equivalent(&left, &changed).is_err());
    }

    fn stub_record(seq: usize, body: &str, length: usize) -> String {
        serde_json::json!({
            "seq": seq,
            "method": "POST",
            "path": "/v1/responses",
            "headers": [["content-length", length.to_string()]],
            "body": body,
        })
        .to_string()
    }

    fn timed_body(time: &str) -> String {
        format!(r#"{{"output":"Wall time {time} seconds"}}"#)
    }

    #[test]
    fn verified_stub_content_length_normalizes() {
        let (long, short) = (timed_body("11.1"), timed_body("4.7"));
        let left = vec![artifact("stub/000", stub_record(0, &long, long.len()))];
        let right = vec![artifact("stub/000", stub_record(0, &short, short.len()))];
        assert_eq!(equivalent(&left, &right), Ok(true));
    }

    #[test]
    fn stub_content_length_not_matching_its_body_stays_a_difference() {
        let (long, short) = (timed_body("11.1"), timed_body("4.7"));
        let left = vec![artifact("stub/000", stub_record(0, &long, long.len() + 5))];
        let right = vec![artifact("stub/000", stub_record(0, &short, short.len()))];
        assert_eq!(equivalent(&left, &right), Ok(false));
    }

    #[test]
    fn stub_content_length_outside_stub_stays_literal() {
        let (long, short) = (timed_body("11.1"), timed_body("4.7"));
        let header = |length: usize| format!(r#"["content-length","{length}"]"#);
        let left = vec![
            artifact("stub/000", stub_record(0, &long, long.len())),
            artifact("copy", header(long.len())),
        ];
        let right = vec![
            artifact("stub/000", stub_record(0, &short, short.len())),
            artifact("copy", header(short.len())),
        ];
        assert_eq!(equivalent(&left, &right), Ok(false));
    }

    #[test]
    fn send_receipt_key_inside_file_contents_stays_literal() {
        let print = sha256_hex("f1");
        let (left_key, right_key) = (sha256_hex("k1"), sha256_hex("k3"));
        let with_note = |key: &str| {
            let mut text = receipt(1, key, &print);
            text.text = text
                .text
                .replace("\"state\"", &format!("\"note\":\"{key}\",\"state\""));
            text
        };
        assert_eq!(
            equivalent(
                &[receipt(1, &left_key, &print)],
                &[receipt(1, &right_key, &print)]
            ),
            Ok(true)
        );
        assert_eq!(
            equivalent(&[with_note(&left_key)], &[with_note(&right_key)]),
            Ok(false)
        );
    }

    #[test]
    fn unverified_header_with_a_verified_number_in_another_record_differs() {
        let (long, short) = (timed_body("11.1"), timed_body("4.7"));
        let same = timed_body("5.0");
        // Record 1 carries the numbers record 0 verified, but its own body
        // does not match them, so its headers must stay a difference.
        let left = vec![
            artifact("stub/000", stub_record(0, &long, long.len())),
            artifact("stub/001", stub_record(1, &same, long.len())),
        ];
        let right = vec![
            artifact("stub/000", stub_record(0, &short, short.len())),
            artifact("stub/001", stub_record(1, &same, short.len())),
        ];
        assert_eq!(equivalent(&left, &right), Ok(false));
        let fixed = vec![
            left[0].clone(),
            artifact("stub/001", stub_record(1, &same, same.len())),
        ];
        let fixed_right = vec![
            right[0].clone(),
            artifact("stub/001", stub_record(1, &same, same.len())),
        ];
        assert_eq!(equivalent(&fixed, &fixed_right), Ok(true));
    }

    #[test]
    fn relative_age_matches_only_the_exact_renderings() {
        // The four pinned renderings, plural even for 1.
        let pinned = r#"{"created": "just now"} {"created": "1 minutes ago"}
            {"created": "12 hours ago"} {"created": "3 days ago"}"#;
        assert_eq!(
            relative_ages(&[pinned]),
            [
                r#""created": "just now""#,
                r#""created": "1 minutes ago""#,
                r#""created": "12 hours ago""#,
                r#""created": "3 days ago""#,
            ]
        );
        // Everything else is not masked: seconds, singular forms, other units,
        // padding, other keys and malformed counts.
        let other = r#"{"created": "5 seconds ago"} {"created": "1 second ago"}
            {"created": "1 minute ago"} {"created": "2 hour ago"} {"created": "1 day ago"}
            {"created": "yesterday"} {"created": "3 weeks ago"} {"created": " 3 days ago"}
            {"created": "just now!"} {"created":"just now"} {"created": "x minutes ago"}
            {"created": "7  hours ago"} {"created": "-1 days ago"} {"created": "1.5 hours ago"}"#;
        assert!(relative_ages(&[other]).is_empty());
    }

    fn listing(age: &str) -> String {
        format!("[\n  {{\n    \"status\": \"error\",\n    \"created\": \"{age}\"\n  }}\n]\n")
    }

    #[test]
    fn relative_age_normalizes_only_in_step_stdout() {
        let left = vec![artifact("step-04-ls/stdout", listing("just now"))];
        let right = vec![artifact("step-04-ls/stdout", listing("1 minutes ago"))];
        assert_eq!(equivalent(&left, &right), Ok(true));
        // The same text in any other artifact stays a difference.
        let left = vec![artifact("step-04-ls/stderr", listing("just now"))];
        let right = vec![artifact("step-04-ls/stderr", listing("1 minutes ago"))];
        assert_eq!(equivalent(&left, &right), Ok(false));
        // Only the age is masked: other content still decides.
        let left = vec![artifact("step-04-ls/stdout", listing("just now"))];
        let right = vec![artifact(
            "step-04-ls/stdout",
            listing("1 minutes ago").replace("error", "idle"),
        )];
        assert_eq!(equivalent(&left, &right), Ok(false));
    }

    #[test]
    fn relative_age_applies_only_to_the_pinned_client_on_both_sides() {
        let left = vec![artifact("step-04-ls/stdout", listing("just now"))];
        let right = vec![artifact("step-04-ls/stdout", listing("1 minutes ago"))];
        let classes = |left_client: &str, right_client: &str| {
            let mut left_facts = facts("/private/tmp/spocky-p3-g1-0000000000a", 41001, 42001);
            let mut right_facts = facts("/private/tmp/spocky-p3-g1-0000000000b", 41002, 42002);
            left_facts.client = left_client.into();
            right_facts.client = right_client.into();
            value_classes(
                &input(&left_facts, &left, Vec::new()),
                &input(&right_facts, &right, Vec::new()),
                &SLICE_SHAPES,
            )
            .unwrap()
            .into_iter()
            .any(|class| class.id == "cli-relative-age")
        };
        assert!(classes(PINNED_CLIENT, PINNED_CLIENT));
        // A side that did not run the pinned CLI gets no class, so its age
        // text stays raw and the comparison reports it.
        assert!(!classes(PINNED_CLIENT, "spocky-cli"));
        assert!(!classes("spocky-cli", PINNED_CLIENT));
        assert!(!classes("spocky-cli", "spocky-cli"));
    }

    #[test]
    fn a_one_sided_or_extra_relative_age_fails() {
        let aged = artifact("step-04-ls/stdout", listing("just now"));
        let plain = artifact("step-04-ls/stdout", "[]\n".into());
        // One side has an age the other lacks: discovery fails.
        assert!(equivalent(std::slice::from_ref(&aged), std::slice::from_ref(&plain)).is_err());
        // Equal totals in different artifacts: the artifact holding it on
        // one side only gets no rule and still differs.
        let left = vec![aged.clone(), artifact("step-05-ls/stdout", "[]\n".into())];
        let right = vec![plain, artifact("step-05-ls/stdout", listing("2 days ago"))];
        assert_eq!(equivalent(&left, &right), Ok(false));
        // Two on one side, one on the other.
        let left = vec![artifact(
            "step-04-ls/stdout",
            format!("{}{}", listing("just now"), listing("just now")),
        )];
        assert!(equivalent(&left, &[aged]).is_err());
    }

    fn state(updated: &str, attention: &str) -> String {
        format!(
            "{{\"updatedAt\":\"{updated}\",\"attentionTimestamp\":\"{attention}\",\"createdAt\":\"2026-10-01T13:51:42.000Z\"}}"
        )
    }

    #[test]
    fn updated_at_and_attention_timestamp_are_masked_independently() {
        // One value on the left, 5 ms apart on the right (the pinned-vs-pinned
        // pattern): no cross-field pairing, so it is equivalent.
        let left = one(state(
            "2026-10-01T13:51:43.153Z",
            "2026-10-01T13:51:43.153Z",
        ));
        let right = one(state(
            "2026-10-01T13:51:45.371Z",
            "2026-10-01T13:51:45.376Z",
        ));
        assert_eq!(equivalent(&left, &right), Ok(true));
        // And the reverse: apart on the left, one value on the right.
        assert_eq!(equivalent(&right, &left), Ok(true));
    }

    #[test]
    fn other_clock_fields_keep_their_equality_pairing() {
        // The same shape with createdAt and lastUserMessageAt must still fail:
        // only the two ruled fields are decoupled.
        let pair = |first: &str, second: &str| {
            one(format!(
                "{{\"createdAt\":\"{first}\",\"lastUserMessageAt\":\"{second}\"}}"
            ))
        };
        let left = pair("2026-10-01T13:51:43.153Z", "2026-10-01T13:51:43.153Z");
        let right = pair("2026-10-01T13:51:45.371Z", "2026-10-01T13:51:45.376Z");
        let error = equivalent(&left, &right).unwrap_err();
        assert!(error.contains("equality structure"), "{error}");
    }

    #[test]
    fn a_ruled_field_keeps_its_own_equality_structure_and_count() {
        let two = |first: &str, second: &str| {
            one(format!(
                "{{\"updatedAt\":\"{first}\"}} {{\"updatedAt\":\"{second}\"}}"
            ))
        };
        // Inside one field, an equal pair against a distant pair still fails.
        let equal = two("2026-10-01T13:51:43.153Z", "2026-10-01T13:51:43.153Z");
        let apart = two("2026-10-01T13:51:45.100Z", "2026-10-01T13:51:46.200Z");
        assert!(equivalent(&equal, &apart).is_err());
        // A different number of occurrences fails.
        let one_only = one(state(
            "2026-10-01T13:51:43.153Z",
            "2026-10-01T13:51:43.153Z",
        ));
        let missing = one("{\"updatedAt\":\"2026-10-01T13:51:45.371Z\"}".into());
        assert!(equivalent(&one_only, &missing).is_err());
    }

    #[test]
    fn ruled_fields_mask_only_a_quoted_instant_after_an_unescaped_key() {
        let texts = [
            "{\"updatedAt\": \"2026-10-01T13:51:43.153Z\"}",
            "{\"attentionTimestamp\" :\"2026-10-01T13:51:44.000Z\"}",
            "{\"updatedAt\":null} {\"updatedAt\":\"yesterday\"} {\\\"updatedAt\\\":\\\"2026-10-01T13:51:45.000Z\\\"}",
            "Updated at 2026-10-01T13:51:46.000Z and \"updatedAtX\":\"2026-10-01T13:51:47.000Z\"",
        ];
        let (start, end) = (1_790_862_700_000, 1_790_862_710_000);
        let fields: Vec<String> = independent_clock_values(&texts, start, end)
            .iter()
            .map(|instant| instant.value.clone())
            .collect();
        assert_eq!(
            fields,
            [
                "\"updatedAt\": \"2026-10-01T13:51:43.153Z\"",
                "\"attentionTimestamp\" :\"2026-10-01T13:51:44.000Z\"",
            ]
        );
        // The generic scan leaves exactly those two out and keeps the rest.
        let generic: Vec<String> = wall_clock_values(&texts, start, end)
            .iter()
            .map(|instant| instant.value.clone())
            .collect();
        assert_eq!(
            generic,
            [
                "2026-10-01T13:51:45.000Z",
                "2026-10-01T13:51:46.000Z",
                "2026-10-01T13:51:47.000Z",
            ]
        );
    }

    #[test]
    fn a_ruled_field_outside_the_run_window_stays_literal() {
        let late = one(state(
            "2026-10-01T14:00:00.000Z",
            "2026-10-01T14:00:00.000Z",
        ));
        let later = one(state(
            "2026-10-01T14:00:01.000Z",
            "2026-10-01T14:00:01.000Z",
        ));
        assert_eq!(equivalent(&late, &later), Ok(false));
    }
}
