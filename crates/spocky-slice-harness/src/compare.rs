//! Turns two side captures into `spocky-differential` observations, discovers
//! exact-value normalization rules, compares, and decides the gate verdict.
//!
//! Compared per side, in this order: readiness probe and every step (argv,
//! exit, stdout, stderr, stub request count), daemon exit status and forced
//! kill count, every stub request record, the stub consumption summary, then
//! every captured state file. Nothing is sorted except the enumeration order
//! of state files, which uses a masked canonical key so random file names do
//! not reorder them. Stderr and exit codes are always compared.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;
use spocky_differential::{
    Artifact, Comparison, DifferentialManifest, ExecutionCounts, NormalizationRule,
    NormalizationTarget, Observation, ObservationSlot, Scenario, compare_observations,
};

use crate::normalize::{
    SLICE_SHAPES, SideFacts, SideInput, Text, derived_digests, distinct_ids, mask,
    preimage_digests, receipt_key, root_slug, rules_for, value_classes,
};
use crate::side::{CapturedFile, Exit, GateSpec, SideRun, StepRun, failed_checks};

fn step_artifacts(prefix: &str, step: &StepRun, out: &mut Vec<Artifact>) {
    out.push(Artifact::new(
        format!("{prefix}/argv"),
        serde_json::to_vec(&step.argv).unwrap_or_default(),
    ));
    out.push(Artifact::new(
        format!("{prefix}/exit"),
        step.exit.render().into_bytes(),
    ));
    out.push(Artifact::new(
        format!("{prefix}/stdout"),
        step.stdout.clone(),
    ));
    out.push(Artifact::new(
        format!("{prefix}/stderr"),
        step.stderr.clone(),
    ));
    out.push(Artifact::new(
        format!("{prefix}/stub-requests"),
        step.stub_requests.to_string().into_bytes(),
    ));
}

/// Puts the keys of the Responses request body's `client_metadata` object in
/// sorted order, leaving every other byte of the record untouched.
///
/// The pinned codex 0.159.0 serializes `client_metadata` from a hash map, so
/// its key order differs between two runs of the same binary with the same
/// daemon (G1 self-check evidence `g1-20261001T145047Z`). No daemon controls
/// that order. Only this one object is reordered, and only when its exact
/// serialization occurs once in the body; otherwise the record is compared
/// raw, which fails on any order difference.
#[must_use]
pub fn canonical_client_metadata(record: &str) -> String {
    let Ok(Value::Object(mut entry)) = serde_json::from_str::<Value>(record) else {
        return record.to_owned();
    };
    let Some(Value::String(body)) = entry.get("body").cloned() else {
        return record.to_owned();
    };
    let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(&body) else {
        return record.to_owned();
    };
    let Some(Value::Object(metadata)) = parsed.get("client_metadata") else {
        return record.to_owned();
    };
    let original = Value::Object(metadata.clone()).to_string();
    if body.matches(original.as_str()).count() != 1 {
        return record.to_owned();
    }
    let mut keys: Vec<&String> = metadata.keys().collect();
    keys.sort();
    let mut sorted = serde_json::Map::new();
    for key in keys {
        sorted.insert(key.clone(), metadata[key].clone());
    }
    let canonical = body.replacen(&original, &Value::Object(sorted).to_string(), 1);
    entry.insert("body".into(), Value::String(canonical));
    Value::Object(entry).to_string()
}

/// The header and the `client_metadata` key that carry codex's turn metadata,
/// a JSON object serialized into a string.
const TURN_METADATA: &str = "x-codex-turn-metadata";

/// What removing codex's git probe does to one serialized turn metadata value.
enum Probe {
    /// There is no `workspaces` member, so nothing to remove.
    Absent,
    /// `workspaces` is exactly the probe; this is the value without it.
    Strip(String),
    /// A `workspaces` member that is not exactly the probe, or a value that
    /// does not re-serialize to the same bytes: leave the record alone.
    Odd,
}

/// Whether `workspaces` is exactly codex's git probe: one workspace path
/// mapped to an object that has a string `latest_git_commit_hash`, an
/// optional boolean `has_changes`, and nothing else.
fn probe_shaped(workspaces: &Value) -> bool {
    let Value::Object(by_path) = workspaces else {
        return false;
    };
    let mut entries = by_path.values();
    let (Some(Value::Object(probe)), None) = (entries.next(), entries.next()) else {
        return false;
    };
    matches!(probe.get("latest_git_commit_hash"), Some(Value::String(_)))
        && probe.get("has_changes").is_none_or(Value::is_boolean)
        && probe
            .keys()
            .all(|key| key == "latest_git_commit_hash" || key == "has_changes")
}

fn without_probe(text: &str) -> Probe {
    let Ok(Value::Object(mut map)) = serde_json::from_str::<Value>(text) else {
        return Probe::Absent;
    };
    let Some(workspaces) = map.get("workspaces") else {
        return Probe::Absent;
    };
    // Rewriting must change nothing but the removed member.
    let reserialized = Value::Object(map.clone()).to_string();
    if reserialized != text || !probe_shaped(workspaces) {
        return Probe::Odd;
    }
    map.shift_remove("workspaces");
    Probe::Strip(Value::Object(map).to_string())
}

/// The record with codex's git probe removed, or `None` when there is none to
/// remove or any copy of it is not exactly the probe. Codex sends its turn
/// metadata twice: as the `x-codex-turn-metadata` header and as the
/// `x-codex-turn-metadata` key of the body's `client_metadata`; the probe is
/// the `workspaces` member of that serialized object, and both copies go.
fn removed_git_probe(record: &str) -> Option<String> {
    let Ok(Value::Object(mut entry)) = serde_json::from_str::<Value>(record) else {
        return None;
    };
    let mut stripped = false;
    if let Some(Value::Array(headers)) = entry.get_mut("headers") {
        for header in headers {
            let Some([Value::String(name), Value::String(value)]) =
                header.as_array().map(Vec::as_slice)
            else {
                continue;
            };
            if name != TURN_METADATA {
                continue;
            }
            match without_probe(value) {
                Probe::Absent => {}
                Probe::Odd => return None,
                Probe::Strip(new) => {
                    *header = serde_json::json!([TURN_METADATA, new]);
                    stripped = true;
                }
            }
        }
    }
    let Some(Value::String(body)) = entry.get("body").cloned() else {
        return stripped.then(|| Value::Object(entry).to_string());
    };
    let mut new_body = body.clone();
    if let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(&body)
        && let Some(Value::Object(metadata)) = parsed.get("client_metadata")
        && let Some(Value::String(turn)) = metadata.get(TURN_METADATA)
    {
        match without_probe(turn) {
            Probe::Absent => {}
            Probe::Odd => return None,
            Probe::Strip(new) => {
                let original = Value::Object(metadata.clone()).to_string();
                if body.matches(original.as_str()).count() != 1 {
                    return None;
                }
                let mut kept = metadata.clone();
                kept.insert(TURN_METADATA.into(), Value::String(new));
                new_body = body.replacen(&original, &Value::Object(kept).to_string(), 1);
                stripped = true;
            }
        }
    }
    if !stripped {
        return None;
    }
    // The body got shorter, so lower the record's own content-length by the
    // same amount, but only when it equalled the raw body length.
    if let Some(Value::Array(headers)) = entry.get_mut("headers") {
        for header in headers {
            let declared = header.as_array().filter(|pair| {
                pair.len() == 2
                    && pair[0] == "content-length"
                    && pair[1].as_str() == Some(body.len().to_string().as_str())
            });
            if declared.is_some() {
                *header = serde_json::json!(["content-length", new_body.len().to_string()]);
            }
        }
    }
    entry.insert("body".into(), Value::String(new_body));
    Some(Value::Object(entry).to_string())
}

/// Whether the record carries codex's git probe and it can be removed.
fn git_probe(record: &str) -> bool {
    removed_git_probe(record).is_some()
}

/// Removes codex's git probe from one stub request record (see
/// [`removed_git_probe`]); a record without a removable probe is returned
/// unchanged.
#[must_use]
pub fn strip_git_probe(record: &str) -> String {
    removed_git_probe(record).unwrap_or_else(|| record.to_owned())
}

/// Id of the codex git probe transform.
pub const GIT_PROBE_TRANSFORM: &str = "codex-workspace-git-probe";

/// Stub request records where exactly one side carries codex's git probe, as
/// (left records, right records) to strip. A record that has it on both sides
/// is compared as is, so a different commit hash or `has_changes` still
/// fails; records are paired by position, and a different record count
/// strips nothing.
fn git_probe_strips(left: &SideRun, right: &SideRun) -> (Vec<usize>, Vec<usize>) {
    let (mut on_left, mut on_right) = (Vec::new(), Vec::new());
    if left.stub_records.len() != right.stub_records.len() {
        return (on_left, on_right);
    }
    for (index, (l, r)) in left
        .stub_records
        .iter()
        .zip(&right.stub_records)
        .enumerate()
    {
        match (git_probe(l), git_probe(r)) {
            (true, false) => on_left.push(index),
            (false, true) => on_right.push(index),
            _ => {}
        }
    }
    (on_left, on_right)
}

/// Describes the git probe transform; `None` when it changed no record.
fn git_probe_transform(left: &[usize], right: &[usize]) -> Option<Transform> {
    if left.is_empty() && right.is_empty() {
        return None;
    }
    let mut removed: Vec<String> = left
        .iter()
        .map(|index| format!("left:stub/{index:03}"))
        .collect();
    removed.extend(right.iter().map(|index| format!("right:stub/{index:03}")));
    Some(Transform {
        id: GIT_PROBE_TRANSFORM.into(),
        target: "stub request records: the workspaces member (<root>.{latest_git_commit_hash,has_changes}) of the x-codex-turn-metadata header and of client_metadata[x-codex-turn-metadata], only where exactly one side carries it; the record's content-length header is lowered by the removed body bytes".into(),
        reason: "codex 0.159.0 collects the workspace git probe asynchronously and sometimes sends its first request without it (original-vs-original runs g4-http500 and g4-retry); no daemon controls it. Where both sides carry it, the values are compared raw".into(),
        owner: "p3_slice_harness".into(),
        raw_retained: "left-*/side.json and right-*/side.json stub_records, and files/stub".into(),
        reordered: removed,
    })
}

/// The single named non-normalization transform the gate applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transform {
    pub id: String,
    pub target: String,
    pub reason: String,
    pub owner: String,
    /// Where the untransformed bytes are kept.
    pub raw_retained: String,
    /// Compared artifacts whose bytes the transform changed, as
    /// `left:<name>` or `right:<name>`.
    pub reordered: Vec<String>,
}

/// Id of the `client_metadata` key-order transform.
pub const CLIENT_METADATA_TRANSFORM: &str = "codex-client-metadata-key-order";

/// Describes the `client_metadata` transform and where it changed bytes.
#[must_use]
pub fn client_metadata_transform(left: &SideRun, right: &SideRun) -> Transform {
    let mut reordered = Vec::new();
    // Label by position: both sides run the same daemon in a self-check.
    for (position, side) in [("left", left), ("right", right)] {
        for (index, record) in side.stub_records.iter().enumerate() {
            if canonical_client_metadata(record) != *record {
                reordered.push(format!("{position}:stub/{index:03}"));
            }
        }
    }
    Transform {
        id: CLIENT_METADATA_TRANSFORM.into(),
        target: "stub request records: keys of the body's client_metadata object only".into(),
        reason:
            "the pinned codex 0.159.0 binary emits client_metadata keys in hash-map order that \
                 differs run to run with no daemon involved (four direct codex exec runs, four \
                 orders); the daemon's own codex input is compared byte for byte via codex-io"
                .into(),
        owner: "p3_slice_harness".into(),
        raw_retained: "left-*/side.json and right-*/side.json stub_records, and files/stub".into(),
        reordered,
    }
}

/// Compared artifacts of one side, in canonical order.
#[must_use]
pub fn side_artifacts(side: &SideRun) -> Vec<Artifact> {
    side_artifacts_without_probe(side, &[])
}

/// [`side_artifacts`], with the codex git probe removed from the stub request
/// records at `probe_records` (see [`strip_git_probe`]).
fn side_artifacts_without_probe(side: &SideRun, probe_records: &[usize]) -> Vec<Artifact> {
    let mut artifacts = Vec::new();
    step_artifacts("ready", &side.readiness, &mut artifacts);
    for (index, step) in side.steps.iter().enumerate() {
        step_artifacts(
            &format!("step-{:02}-{}", index + 1, step.name),
            step,
            &mut artifacts,
        );
    }
    artifacts.push(Artifact::new(
        "daemon/exit",
        side.daemon_exit.render().into_bytes(),
    ));
    artifacts.push(Artifact::new(
        "daemon/force-killed",
        side.force_killed.len().to_string().into_bytes(),
    ));
    for (index, record) in side.stub_records.iter().enumerate() {
        let record = if probe_records.contains(&index) {
            strip_git_probe(record)
        } else {
            record.clone()
        };
        artifacts.push(Artifact::new(
            format!("stub/{index:03}"),
            canonical_client_metadata(&record).into_bytes(),
        ));
    }
    artifacts.push(Artifact::new(
        "stub/summary",
        format!(
            "scripted {} of {}, unscripted {}",
            side.stub_scripted, side.script_len, side.stub_unscripted
        )
        .into_bytes(),
    ));
    artifacts
}

fn state_bytes(file: &CapturedFile) -> Vec<u8> {
    let mut bytes = format!("{}\n", file.path).into_bytes();
    bytes.extend_from_slice(&file.bytes);
    bytes
}

/// State files in canonical order: by masked path, then masked content, then
/// raw bytes. Masking hides generated ids, names verified digests by kind,
/// and hides send receipt keys, so receipts order by their fingerprint.
#[must_use]
pub fn canonical_state(
    state: &[CapturedFile],
    all_texts: &[&str],
    preimages: &[(&'static str, String)],
    root: &str,
) -> Vec<Artifact> {
    let found = distinct_ids(all_texts, &SLICE_SHAPES);
    let mut derived = derived_digests(&found, all_texts);
    derived.extend(preimage_digests(preimages, all_texts));
    let mut keyed: Vec<(String, String, Vec<u8>)> = state
        .iter()
        .map(|file| {
            let content = String::from_utf8_lossy(&file.bytes);
            let mut path = mask(&file.path, &SLICE_SHAPES, &derived);
            if let Some(key) = receipt_key(&file.path) {
                path = path.replace(key, "{send-receipt-key}");
            }
            (
                path,
                mask(&content, &SLICE_SHAPES, &derived),
                state_bytes(file),
            )
        })
        .collect();
    keyed.sort();
    // Name each file by its masked path and its occurrence number among files
    // with that masked path, never by global index: a file present on one side
    // only then shows as its own difference instead of shifting every later
    // file's name (and breaking the rules bound to those names).
    let slug = root_slug(root);
    let mut seen: Vec<(String, usize)> = Vec::new();
    keyed
        .into_iter()
        .map(|(masked_path, _, bytes)| {
            let path = masked_path
                .replace(&slug, "{root-slug}")
                .replace(root, "{root}");
            let number = if let Some((_, count)) = seen.iter_mut().find(|(known, _)| *known == path)
            {
                *count += 1;
                *count
            } else {
                seen.push((path.clone(), 1));
                1
            };
            Artifact::new(format!("state/{path}#{number}"), bytes)
        })
        .collect()
}

fn texts(artifacts: &[Artifact], state: &[Artifact]) -> Vec<Text> {
    artifacts
        .iter()
        .map(|artifact| {
            (
                NormalizationTarget::Artifact(artifact.name.clone()),
                artifact,
            )
        })
        .chain(
            state
                .iter()
                .map(|artifact| (NormalizationTarget::State(artifact.name.clone()), artifact)),
        )
        .filter_map(|(target, artifact)| {
            String::from_utf8(artifact.bytes.clone())
                .ok()
                .map(|text| Text { target, text })
        })
        .collect()
}

fn facts(side: &SideRun) -> SideFacts {
    SideFacts {
        root: side.root.trim_end_matches('/').to_owned(),
        daemon_port: side.daemon_port,
        stub_port: side.stub_port,
        window_start_ms: side.window_start_ms,
        window_end_ms: side.window_end_ms,
        client: side.client.clone(),
    }
}

fn executed_fixtures(side: &SideRun) -> u64 {
    let executed = std::iter::once(&side.readiness)
        .chain(&side.steps)
        .filter(|step| !matches!(step.exit, Exit::NotRun(_)))
        .count();
    u64::try_from(executed).unwrap_or(u64::MAX)
}

fn observation(
    artifacts: Vec<Artifact>,
    state: Vec<Artifact>,
    counts: ExecutionCounts,
) -> Observation {
    Observation {
        structured_output: ObservationSlot::Missing,
        stdout: ObservationSlot::Missing,
        stderr: ObservationSlot::Missing,
        exit_code: ObservationSlot::Missing,
        artifacts: ObservationSlot::Value(artifacts),
        state: ObservationSlot::Value(state),
        screenshots: ObservationSlot::Missing,
        accessibility: ObservationSlot::Missing,
        performance: ObservationSlot::Missing,
        recovery: ObservationSlot::Missing,
        counts: ObservationSlot::Value(counts),
        raw_failures: Vec::new(),
    }
}

/// The gate verdict plus everything needed to audit it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    pub gate: String,
    pub left: String,
    pub right: String,
    pub pass: bool,
    pub differences: Vec<Comparison>,
    pub discovery_error: Option<String>,
    pub comparison_error: Option<String>,
    pub check_failures: Vec<String>,
    pub survivors: Vec<String>,
    pub harness_errors: Vec<String>,
    pub expected_counts: ExecutionCounts,
    pub rule_count: usize,
    pub transforms: Vec<Transform>,
    /// Whether the normalized comparison ran. When false, `differences` is
    /// empty because nothing was compared, not because nothing differed.
    pub compared: bool,
    /// `compared`, or `skipped: <reason>` when discovery or comparison failed.
    pub comparison: String,
    /// `not applied`, or how many original references and stable codex
    /// invocation pairs the right side was checked against.
    pub order_check: String,
    /// Stable codex invocation pairs the right side inverted. Any entry fails.
    pub order_inversions: Vec<String>,
}

/// Fewest original observations a stable codex order is derived from.
pub const MIN_ORDER_REFERENCES: usize = 3;

/// Codex invocation label pairs `(before, after)` whose relative order every
/// reference observation agrees on. Only labels present in every reference
/// are considered; labels are unique within one observation.
#[must_use]
pub fn stable_pairs(references: &[&[String]]) -> Vec<(String, String)> {
    let Some((first, rest)) = references.split_first() else {
        return Vec::new();
    };
    let position = |order: &[String], label: &String| order.iter().position(|known| known == label);
    let common: Vec<&String> = first
        .iter()
        .filter(|label| rest.iter().all(|order| order.contains(label)))
        .collect();
    let mut pairs = Vec::new();
    for (index, before) in common.iter().enumerate() {
        for after in &common[index + 1..] {
            let agreed = rest
                .iter()
                .all(|order| position(order, before) < position(order, after));
            if agreed {
                pairs.push(((*before).clone(), (*after).clone()));
            }
        }
    }
    pairs
}

impl Verdict {
    /// Checks the right side's codex invocation order against the stable
    /// pairs of `references` (original observations) and fails the verdict
    /// on any inversion. Fewer than [`MIN_ORDER_REFERENCES`] references is a
    /// harness error, never a silent skip.
    pub fn apply_order_check(&mut self, references: &[&[String]], observed: &[String]) {
        if references.len() < MIN_ORDER_REFERENCES {
            self.harness_errors.push(format!(
                "codex order check needs {MIN_ORDER_REFERENCES} original observations, got {}",
                references.len()
            ));
            self.pass = false;
            return;
        }
        let pairs = stable_pairs(references);
        let position = |label: &String| observed.iter().position(|known| known == label);
        self.order_inversions = pairs
            .iter()
            .filter(|(before, after)| {
                matches!((position(before), position(after)), (Some(a), Some(b)) if b < a)
            })
            .map(|(before, after)| {
                format!(
                    "codex {before} ran before {after} in all {} originals, after it on the {} side",
                    references.len(),
                    self.right
                )
            })
            .collect();
        self.order_check = format!(
            "applied: {} original references, {} stable pairs",
            references.len(),
            pairs.len()
        );
        if !self.order_inversions.is_empty() {
            self.pass = false;
        }
    }
}

/// Comparison output: the verdict and, when comparison ran, the manifest.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub verdict: Verdict,
    pub manifest: Option<DifferentialManifest>,
    pub rules: Vec<NormalizationRule>,
    pub transforms: Vec<Transform>,
}

/// Collects per-side lines prefixed with the side's daemon label.
fn labelled(
    left: &SideRun,
    right: &SideRun,
    lines: impl Fn(&SideRun) -> Vec<String>,
) -> Vec<String> {
    [left, right]
        .iter()
        .flat_map(|side| {
            lines(side)
                .into_iter()
                .map(move |line| format!("{}: {line}", side.kind.label()))
        })
        .collect()
}

/// Compared artifacts, canonical state, and execution counts of one side.
fn prepare_side(
    gate: &GateSpec,
    side: &SideRun,
    probe_records: &[usize],
) -> (Vec<Artifact>, Vec<Artifact>, ExecutionCounts) {
    let artifacts = side_artifacts_without_probe(side, probe_records);
    let raw_texts: Vec<String> = artifacts
        .iter()
        .map(|artifact| String::from_utf8_lossy(&artifact.bytes).into_owned())
        .chain(
            side.state
                .iter()
                .map(|file| String::from_utf8_lossy(&state_bytes(file)).into_owned()),
        )
        .collect();
    let raw_refs: Vec<&str> = raw_texts.iter().map(String::as_str).collect();
    let state = canonical_state(
        &side.state,
        &raw_refs,
        &side.preimages,
        side.root.trim_end_matches('/'),
    );
    let counts = ExecutionCounts {
        fixtures: executed_fixtures(side),
        assertions: u64::try_from(gate.checks.len() - failed_checks(gate, side).len()).unwrap_or(0),
    };
    (artifacts, state, counts)
}

/// Compares two sides of one gate.
#[must_use]
pub fn compare_sides(gate: &GateSpec, left: &SideRun, right: &SideRun) -> Outcome {
    let expected_counts = ExecutionCounts {
        fixtures: u64::try_from(gate.steps.len() + 1).unwrap_or(u64::MAX),
        assertions: u64::try_from(gate.checks.len()).unwrap_or(u64::MAX),
    };
    let mut check_failures = failed_checks(gate, left);
    check_failures.extend(failed_checks(gate, right));
    let survivors = labelled(left, right, |side| {
        side.survivors
            .iter()
            .map(|pid| format!("pid {pid}"))
            .collect()
    });
    let harness_errors = labelled(left, right, |side| side.harness_errors.clone());
    let (left_probe, right_probe) = git_probe_strips(left, right);
    let (left_artifacts, left_state, left_counts) = prepare_side(gate, left, &left_probe);
    let (right_artifacts, right_state, right_counts) = prepare_side(gate, right, &right_probe);
    let left_texts = texts(&left_artifacts, &left_state);
    let right_texts = texts(&right_artifacts, &right_state);
    let (left_facts, right_facts) = (facts(left), facts(right));
    let left_input = SideInput {
        facts: &left_facts,
        texts: left_texts.iter().map(|text| text.text.as_str()).collect(),
        extracted: left.extracted.clone(),
        preimages: left.preimages.clone(),
    };
    let right_input = SideInput {
        facts: &right_facts,
        texts: right_texts.iter().map(|text| text.text.as_str()).collect(),
        extracted: right.extracted.clone(),
        preimages: right.preimages.clone(),
    };

    let scenario = Scenario {
        id: format!("phase3-{}", gate.id),
        arguments: Vec::new(),
        environment: BTreeMap::new(),
        initial_files: Vec::new(),
        expected_counts,
    };
    let (manifest, rules, discovery_error, comparison_error) =
        match value_classes(&left_input, &right_input, &SLICE_SHAPES) {
            Err(error) => (None, Vec::new(), Some(error), None),
            Ok(classes) => {
                let rules = rules_for(&classes, &left_texts, &right_texts);
                match compare_observations(
                    &scenario,
                    observation(left_artifacts, left_state, left_counts),
                    observation(right_artifacts, right_state, right_counts),
                    &rules,
                ) {
                    Ok(manifest) => (Some(manifest), rules, None, None),
                    Err(error) => (None, rules, None, Some(error.to_string())),
                }
            }
        };
    let differences = manifest
        .as_ref()
        .map(|manifest| manifest.differences.clone())
        .unwrap_or_default();
    let equivalent = manifest
        .as_ref()
        .is_some_and(|manifest| manifest.equivalent);
    let pass = equivalent
        && differences.is_empty()
        && check_failures.is_empty()
        && survivors.is_empty()
        && harness_errors.is_empty();
    let mut transforms = vec![client_metadata_transform(left, right)];
    transforms.extend(git_probe_transform(&left_probe, &right_probe));
    let compared = manifest.is_some();
    let comparison = match (&discovery_error, &comparison_error) {
        (Some(error), _) => format!("skipped: rule discovery failed: {error}"),
        (None, Some(error)) => format!("skipped: comparison failed: {error}"),
        (None, None) if compared => "compared".to_owned(),
        (None, None) => "skipped: no comparison manifest".to_owned(),
    };
    Outcome {
        verdict: Verdict {
            gate: gate.id.to_owned(),
            left: left.kind.label().to_owned(),
            right: right.kind.label().to_owned(),
            pass,
            differences,
            discovery_error,
            comparison_error,
            check_failures,
            survivors,
            harness_errors,
            expected_counts,
            rule_count: rules.len(),
            transforms: transforms.clone(),
            compared,
            comparison,
            order_check: "not applied".into(),
            order_inversions: Vec::new(),
        },
        manifest,
        rules,
        transforms,
    }
}

/// Names of compared artifacts whose normalized bytes differ, for reports.
#[must_use]
pub fn differing_artifacts(manifest: &DifferentialManifest) -> Vec<String> {
    let mut names = Vec::new();
    for (slot_left, slot_right) in [
        (
            &manifest.original.normalized.artifacts,
            &manifest.rust.normalized.artifacts,
        ),
        (
            &manifest.original.normalized.state,
            &manifest.rust.normalized.state,
        ),
    ] {
        if let (ObservationSlot::Value(left), ObservationSlot::Value(right)) =
            (slot_left, slot_right)
        {
            // Matched by name, so one missing file never misaligns the rest.
            for artifact in left {
                match right.iter().find(|other| other.name == artifact.name) {
                    Some(other) if other.bytes == artifact.bytes => {}
                    Some(_) => names.push(artifact.name.clone()),
                    None => names.push(format!("{} (left only)", artifact.name)),
                }
            }
            for artifact in right {
                if !left.iter().any(|other| other.name == artifact.name) {
                    names.push(format!("{} (right only)", artifact.name));
                }
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::PINNED_CLI_PROGRAM;
    use crate::side::{Check, DaemonKind, HomeOrigin, StepSpec};
    use crate::stub::Script;

    const AGENT_LEFT: &str = "0199a3c4-1b2c-7d3e-8f40-123456789abc";
    const AGENT_RIGHT: &str = "0299a3c4-1b2c-7d3e-8f40-cba987654321";

    fn gate() -> GateSpec {
        gate_with(vec![Check::AllExitZero, Check::DaemonExit(0)])
    }

    fn gate_with(checks: Vec<Check>) -> GateSpec {
        GateSpec {
            id: "t",
            script: Script {
                responses: Vec::new(),
            },
            steps: vec![StepSpec {
                name: "run",
                args: Vec::new(),
                capture: None,
                wait_for_stub_requests: None,
                daemon_restart: false,
                disconnect_at_stub_requests: None,
                node_script: None,
            }],
            checks,
            preimages: |_| Vec::new(),
            codex_present: true,
            home_origin: HomeOrigin::Same,
        }
    }

    fn step(name: &str, stdout: String) -> StepRun {
        StepRun {
            name: name.into(),
            argv: vec!["run".into()],
            stdout: stdout.into_bytes(),
            stderr: b"Using workspace\n".to_vec(),
            exit: Exit::Code(0),
            stub_requests: 0,
        }
    }

    fn side(kind: DaemonKind, root: &str, agent: &str, port: u16) -> SideRun {
        SideRun {
            kind,
            root: root.into(),
            session: String::new(),
            daemon_port: port,
            stub_port: port + 1,
            daemon_pid: None,
            window_start_ms: 1_790_862_700_000,
            window_end_ms: 1_790_862_710_000,
            readiness_attempts: 1,
            readiness: step("ready", "[]\n".into()),
            steps: vec![step(
                "run",
                format!("{{\"agentId\":\"{agent}\",\"cwd\":\"{root}/project\"}}\n"),
            )],
            daemon_exit: Exit::Code(0),
            stub_records: Vec::new(),
            stub_scripted: 0,
            stub_unscripted: 0,
            script_len: 0,
            state: vec![CapturedFile {
                path: format!("paseo-home/agents/{agent}.json"),
                bytes: format!("{{\"id\":\"{agent}\",\"createdAt\":\"2020-01-02T03:04:05.000Z\"}}")
                    .into_bytes(),
            }],
            uncompared: Vec::new(),
            extracted: Vec::new(),
            preimages: Vec::new(),
            force_killed: Vec::new(),
            survivors: Vec::new(),
            harness_errors: Vec::new(),
            observed_pids: Vec::new(),
            codex_order: Vec::new(),
            client: format!("/paseo/{PINNED_CLI_PROGRAM}"),
        }
    }

    fn pair() -> (SideRun, SideRun) {
        (
            side(
                DaemonKind::Original,
                "/private/tmp/spocky-p3-t-0000000000a",
                AGENT_LEFT,
                41000,
            ),
            side(
                DaemonKind::Spocky,
                "/private/tmp/spocky-p3-t-0000000000b",
                AGENT_RIGHT,
                42000,
            ),
        )
    }

    #[test]
    fn equivalent_sides_pass() {
        let (left, right) = pair();
        let verdict = compare_sides(&gate(), &left, &right).verdict;
        assert!(verdict.pass, "{verdict:?}");
    }

    #[test]
    fn stderr_difference_fails() {
        let (left, mut right) = pair();
        right.steps[0].stderr = b"Using workspace!\n".to_vec();
        assert!(!compare_sides(&gate(), &left, &right).verdict.pass);
    }

    #[test]
    fn exit_code_difference_fails() {
        let (left, mut right) = pair();
        right.daemon_exit = Exit::Code(143);
        let verdict = compare_sides(&gate(), &left, &right).verdict;
        assert!(!verdict.pass);
        assert!(!verdict.check_failures.is_empty());
    }

    #[test]
    fn extra_state_file_fails() {
        let (left, mut right) = pair();
        right.state.push(CapturedFile {
            path: "paseo-home/extra.json".into(),
            bytes: b"{}".to_vec(),
        });
        let verdict = compare_sides(&gate(), &left, &right).verdict;
        assert!(!verdict.pass);
        assert!(!verdict.differences.is_empty());
    }

    #[test]
    fn identical_failures_on_both_sides_still_fail() {
        let (mut left, mut right) = pair();
        for side in [&mut left, &mut right] {
            side.steps[0].exit = Exit::NotRun("daemon not ready".into());
        }
        let verdict = compare_sides(&gate(), &left, &right).verdict;
        assert!(!verdict.pass);
        assert_eq!(
            verdict
                .differences
                .iter()
                .map(|difference| difference.path.as_str())
                .collect::<Vec<_>>(),
            vec!["counts:expected"]
        );
    }

    /// Verdict with no positive checks, so only the comparison can fail it.
    fn compared_only(left: &SideRun, right: &SideRun) -> Verdict {
        compare_sides(&gate_with(Vec::new()), left, right).verdict
    }

    fn with_record(mut side: SideRun, body: &str) -> SideRun {
        side.stub_records.push(
            serde_json::json!({"seq": 0, "method": "POST", "path": "/v1/responses", "headers": [], "body": body, "scripted": 0})
                .to_string(),
        );
        side.stub_scripted = 1;
        side.script_len = 1;
        side
    }

    fn with_codex_input(mut side: SideRun, stdin: &str) -> SideRun {
        let mut bytes = b"argv:\napp-server\nstdin:\n".to_vec();
        bytes.extend_from_slice(stdin.as_bytes());
        side.state.push(CapturedFile {
            path: "codex-io/invocation".into(),
            bytes,
        });
        side
    }

    #[test]
    fn codex_input_key_order_swap_fails() {
        let rpc = |params: &str| {
            format!("{{\"id\":8,\"method\":\"thread/start\",\"params\":{params}}}\n")
        };
        let (left, right) = pair();
        let ordered = rpc(r#"{"model":"m","approvalPolicy":"never"}"#);
        let same = compared_only(
            &with_codex_input(left.clone(), &ordered),
            &with_codex_input(right.clone(), &ordered),
        );
        assert!(same.pass, "{same:?}");
        let swapped = rpc(r#"{"approvalPolicy":"never","model":"m"}"#);
        let verdict = compared_only(
            &with_codex_input(left, &ordered),
            &with_codex_input(right, &swapped),
        );
        assert!(differs_at(&verdict, "state"), "{verdict:?}");
    }

    #[test]
    fn nested_key_order_swaps_in_stub_bodies_fail() {
        let cases = [
            // Inside an input item.
            (
                r#"{"input":[{"type":"message","role":"user"}],"client_metadata":{"a":"1"}}"#,
                r#"{"input":[{"role":"user","type":"message"}],"client_metadata":{"a":"1"}}"#,
            ),
            // Inside a client_metadata value: only top-level metadata keys reorder.
            (
                r#"{"client_metadata":{"a":{"q":1,"p":2},"b":"2"}}"#,
                r#"{"client_metadata":{"b":"2","a":{"p":2,"q":1}}}"#,
            ),
        ];
        for (left_body, right_body) in cases {
            let (left, right) = pair();
            let verdict = compared_only(
                &with_record(left, left_body),
                &with_record(right, right_body),
            );
            assert!(
                differs_at(&verdict, "artifacts"),
                "{left_body} vs {right_body}"
            );
        }
    }

    #[test]
    fn skipped_comparison_never_reads_as_zero_differences() {
        // One generated id on the left, none on the right: discovery fails.
        let (left, mut right) = pair();
        right.steps[0].stdout = format!("{{\"cwd\":\"{}/project\"}}\n", right.root).into_bytes();
        right.state.clear();
        let verdict = compared_only(&left, &right);
        assert!(!verdict.pass);
        assert!(!verdict.compared);
        assert!(verdict.differences.is_empty());
        assert!(
            verdict.comparison.starts_with("skipped: "),
            "{}",
            verdict.comparison
        );
        let equal = compared_only(&pair().0, &pair().0);
        assert!(equal.compared);
        assert_eq!(equal.comparison, "compared");
    }

    #[test]
    fn extra_state_file_does_not_shift_other_names() {
        let (mut left, right) = pair();
        for index in 0..2 {
            left.state.push(CapturedFile {
                path: "codex-io/invocation".into(),
                bytes: format!("argv:\n--version\nstdin:\n{index}").into_bytes(),
            });
        }
        let outcome = compare_sides(&gate_with(Vec::new()), &left, &right);
        assert_eq!(outcome.verdict.comparison, "compared");
        assert!(differs_at(&outcome.verdict, "state"));
        let differing = differing_artifacts(outcome.manifest.as_ref().unwrap());
        assert_eq!(
            differing,
            vec![
                "state/codex-io/invocation#1 (left only)".to_owned(),
                "state/codex-io/invocation#2 (left only)".to_owned(),
            ]
        );
    }

    #[test]
    fn send_receipts_pair_by_fingerprint_under_masked_names() {
        use crate::normalize::sha256_hex;
        let receipt = |key: &str, print: &str| CapturedFile {
            path: format!("paseo-home/agent-requests/{}.json", sha256_hex(key)),
            bytes: format!("{{\"fingerprint\":\"{}\"}}", sha256_hex(print)).into_bytes(),
        };
        let (mut left, mut right) = pair();
        left.state
            .extend([receipt("k1", "f1"), receipt("k2", "f2")]);
        right
            .state
            .extend([receipt("k3", "f2"), receipt("k4", "f1")]);
        let outcome = compare_sides(&gate_with(Vec::new()), &left, &right);
        assert!(outcome.verdict.pass, "{:?}", outcome.verdict);
        left.state.push(receipt("k5", "f3"));
        let outcome = compare_sides(&gate_with(Vec::new()), &left, &right);
        assert!(!outcome.verdict.pass);
        assert!(
            outcome
                .verdict
                .discovery_error
                .is_some_and(|error| error.contains("send receipt fingerprints differ"))
        );
    }

    #[test]
    fn comparison_alone_passes_equivalent_sides() {
        let (left, right) = pair();
        let body = r#"{"model":"m","input":"hi"}"#;
        assert!(compared_only(&with_record(left, body), &with_record(right, body)).pass);
    }

    #[test]
    fn comparison_alone_fails_step_exit_code_difference() {
        let (left, mut right) = pair();
        right.steps[0].exit = Exit::Code(1);
        assert!(!compared_only(&left, &right).pass);
    }

    #[test]
    fn comparison_alone_fails_stub_body_difference() {
        let (left, right) = pair();
        let left = with_record(left, r#"{"model":"m","input":"hi"}"#);
        let right = with_record(right, r#"{"model":"m","input":"ho"}"#);
        assert!(!compared_only(&left, &right).pass);
    }

    #[test]
    fn comparison_alone_fails_body_key_order_swap() {
        let (left, right) = pair();
        let left = with_record(left, r#"{"model":"m","input":"hi"}"#);
        let right = with_record(right, r#"{"input":"hi","model":"m"}"#);
        assert!(!compared_only(&left, &right).pass);
    }

    #[test]
    fn client_metadata_transform_is_named_and_touches_only_that_object() {
        let (left, right) = pair();
        let left = with_record(
            left,
            r#"{"model":"m","client_metadata":{"b":"2","a":"1"},"z":1}"#,
        );
        let right = with_record(
            right,
            r#"{"model":"m","client_metadata":{"a":"1","b":"2"},"z":1}"#,
        );
        let outcome = compare_sides(&gate_with(Vec::new()), &left, &right);
        assert!(outcome.verdict.pass);
        assert_eq!(outcome.verdict.transforms.len(), 1);
        let transform = &outcome.verdict.transforms[0];
        assert_eq!(transform.id, CLIENT_METADATA_TRANSFORM);
        assert_eq!(transform.reordered, vec!["left:stub/000".to_owned()]);
        // A self-check runs the same daemon on both sides; labels stay distinct.
        let (left, mut right) = pair();
        right.kind = DaemonKind::Original;
        let left = with_record(left, r#"{"client_metadata":{"b":"2","a":"1"}}"#);
        let right = with_record(right, r#"{"client_metadata":{"d":"2","c":"1"}}"#);
        assert_eq!(
            client_metadata_transform(&left, &right).reordered,
            vec!["left:stub/000".to_owned(), "right:stub/000".to_owned()]
        );
        // The same metadata reorder plus a key-order swap elsewhere in the body fails.
        let (left, right) = pair();
        let left = with_record(
            left,
            r#"{"model":"m","client_metadata":{"b":"2","a":"1"},"z":1}"#,
        );
        let right = with_record(
            right,
            r#"{"client_metadata":{"a":"1","b":"2"},"model":"m","z":1}"#,
        );
        assert!(!compared_only(&left, &right).pass);
    }

    #[test]
    fn comparison_alone_fails_missing_state_file() {
        let (left, mut right) = pair();
        right.state.clear();
        let verdict = compared_only(&left, &right);
        assert!(!verdict.pass);
        // The state difference itself fails, not rule discovery.
        assert_eq!(verdict.discovery_error, None);
        assert_eq!(verdict.comparison_error, None);
        assert!(
            verdict
                .differences
                .iter()
                .any(|difference| difference.path == "state")
        );
    }

    fn differs_at(verdict: &Verdict, path: &str) -> bool {
        !verdict.pass
            && verdict.discovery_error.is_none()
            && verdict.comparison_error.is_none()
            && verdict
                .differences
                .iter()
                .any(|difference| difference.path == path)
    }

    #[test]
    fn comparison_alone_fails_stdout_key_order_swap() {
        let (left, mut right) = pair();
        right.steps[0].stdout = format!(
            "{{\"cwd\":\"{}/project\",\"agentId\":\"{AGENT_RIGHT}\"}}\n",
            right.root
        )
        .into_bytes();
        assert!(differs_at(&compared_only(&left, &right), "artifacts"));
    }

    #[test]
    fn comparison_alone_fails_state_key_order_swap() {
        let (left, mut right) = pair();
        right.state[0].bytes =
            format!("{{\"createdAt\":\"2020-01-02T03:04:05.000Z\",\"id\":\"{AGENT_RIGHT}\"}}")
                .into_bytes();
        assert!(differs_at(&compared_only(&left, &right), "state"));
    }

    #[test]
    fn comparison_alone_fails_state_content_difference() {
        let (left, mut right) = pair();
        right.state[0].bytes = format!(
            "{{\"id\":\"{AGENT_RIGHT}\",\"createdAt\":\"2020-01-02T03:04:05.000Z\",\"x\":1}}"
        )
        .into_bytes();
        assert!(differs_at(&compared_only(&left, &right), "state"));
    }

    #[test]
    fn survivor_or_harness_error_fails() {
        let (left, mut right) = pair();
        right.survivors.push(4242);
        assert!(!compare_sides(&gate(), &left, &right).verdict.pass);
        let (mut left, right) = pair();
        left.harness_errors.push("tmux session survived".into());
        assert!(!compare_sides(&gate(), &left, &right).verdict.pass);
    }

    fn record(body: &str) -> String {
        serde_json::json!({"seq": 0, "method": "POST", "body": body}).to_string()
    }

    #[test]
    fn client_metadata_order_is_canonical_and_nothing_else_moves() {
        let left = record(r#"{"model":"m","client_metadata":{"b":"2","a":"1"},"z":1,"y":2}"#);
        let right = record(r#"{"model":"m","client_metadata":{"a":"1","b":"2"},"z":1,"y":2}"#);
        assert_eq!(
            canonical_client_metadata(&left),
            canonical_client_metadata(&right)
        );
        // Top-level body key order still differs after canonicalization.
        let reordered = record(r#"{"model":"m","client_metadata":{"a":"1","b":"2"},"y":2,"z":1}"#);
        assert_ne!(
            canonical_client_metadata(&left),
            canonical_client_metadata(&reordered)
        );
        // A metadata value difference survives.
        let changed = record(r#"{"model":"m","client_metadata":{"a":"1","b":"3"},"z":1,"y":2}"#);
        assert_ne!(
            canonical_client_metadata(&left),
            canonical_client_metadata(&changed)
        );
        // Nested objects inside metadata values keep their order.
        let nested_left = record(r#"{"client_metadata":{"a":{"q":1,"p":2}}}"#);
        let nested_right = record(r#"{"client_metadata":{"a":{"p":2,"q":1}}}"#);
        assert_ne!(
            canonical_client_metadata(&nested_left),
            canonical_client_metadata(&nested_right)
        );
    }

    #[test]
    fn records_without_client_metadata_are_unchanged() {
        for raw in [
            "not json",
            r#"{"body":"not json"}"#,
            &record(r#"{"model":"m"}"#),
        ] {
            assert_eq!(canonical_client_metadata(raw), raw);
        }
    }

    fn order(labels: &[&str]) -> Vec<String> {
        labels.iter().map(|label| (*label).to_owned()).collect()
    }

    #[test]
    fn stable_pairs_keep_only_orders_every_original_agrees_on() {
        let one = order(&["--version#1", "app-server#1", "goals#1", "--version#2"]);
        let two = order(&["--version#1", "app-server#1", "--version#2", "goals#1"]);
        let three = order(&["--version#1", "app-server#1", "goals#1", "--version#2"]);
        let pairs = stable_pairs(&[&one, &two, &three]);
        assert!(pairs.contains(&("app-server#1".into(), "goals#1".into())));
        assert!(pairs.contains(&("--version#1".into(), "--version#2".into())));
        // goals and the second --version swap between originals: not stable.
        assert!(!pairs.contains(&("goals#1".into(), "--version#2".into())));
        assert!(!pairs.contains(&("--version#2".into(), "goals#1".into())));
    }

    #[test]
    fn order_check_fails_an_inverted_stable_pair_only() {
        let (left, right) = pair();
        let mut verdict = compare_sides(&gate_with(Vec::new()), &left, &right).verdict;
        assert!(verdict.pass);
        let original = order(&["--version#1", "app-server#1", "goals#1", "--version#2"]);
        let racy = order(&["--version#1", "app-server#1", "--version#2", "goals#1"]);
        let references: [&[String]; 3] = [&original, &racy, &original];
        let mut kept = verdict.clone();
        kept.apply_order_check(&references, &racy);
        assert!(kept.pass, "{:?}", kept.order_inversions);
        assert!(
            kept.order_check
                .starts_with("applied: 3 original references")
        );
        verdict.apply_order_check(
            &references,
            &order(&["--version#1", "goals#1", "app-server#1", "--version#2"]),
        );
        assert!(!verdict.pass);
        assert_eq!(verdict.order_inversions.len(), 1);
        assert!(verdict.order_inversions[0].contains("app-server#1 ran before goals#1"));
    }

    #[test]
    fn order_check_without_three_originals_fails_closed() {
        let (left, right) = pair();
        let mut verdict = compare_sides(&gate_with(Vec::new()), &left, &right).verdict;
        let original = order(&["app-server#1"]);
        verdict.apply_order_check(&[&original, &original], &original);
        assert!(!verdict.pass);
        assert!(verdict.harness_errors[0].contains("needs 3 original observations"));
    }

    const PROBE: &str =
        r#","workspaces":{"/r/project":{"latest_git_commit_hash":"600188d7","has_changes":false}}"#;

    /// Codex's serialized turn metadata, with `workspaces` or without.
    fn turn_metadata(workspaces: &str) -> String {
        format!(
            r#"{{"installation_id":"i","sandbox":"none"{workspaces},"turn_started_at_unix_ms":1}}"#
        )
    }

    /// A stub record shaped like codex 0.159.0's: the turn metadata is a
    /// header and a `client_metadata` value, both serialized strings.
    fn probe_record(side: SideRun, workspaces: &str, declared: Option<usize>) -> SideRun {
        let mut side = side;
        let turn = turn_metadata(workspaces);
        let body = serde_json::json!({
            "model": "m",
            "client_metadata": {"a": "1", TURN_METADATA: turn, "z": "9"},
            "tail": 1,
        })
        .to_string();
        let length = declared.unwrap_or(body.len());
        side.stub_records.push(
            serde_json::json!({"seq": 0, "method": "POST", "path": "/v1/responses", "headers": [["x-codex-beta-features", "f"], [TURN_METADATA, turn], ["content-length", length.to_string()]], "body": body, "scripted": 0})
                .to_string(),
        );
        side.stub_scripted = 1;
        side.script_len = 1;
        side
    }

    #[test]
    fn a_git_probe_on_one_side_only_is_removed_and_named() {
        let (left, right) = pair();
        let left = probe_record(left, PROBE, None);
        let right = probe_record(right, "", None);
        let outcome = compare_sides(&gate_with(Vec::new()), &left, &right);
        assert!(outcome.verdict.pass, "{:?}", outcome.verdict.differences);
        let ids: Vec<&str> = outcome
            .verdict
            .transforms
            .iter()
            .map(|t| t.id.as_str())
            .collect();
        assert_eq!(ids, [CLIENT_METADATA_TRANSFORM, GIT_PROBE_TRANSFORM]);
        assert_eq!(outcome.verdict.transforms[1].reordered, ["left:stub/000"]);
        // Either side may be the one carrying it.
        let (left, right) = pair();
        let outcome = compare_sides(
            &gate_with(Vec::new()),
            &probe_record(left, "", None),
            &probe_record(right, PROBE, None),
        );
        assert!(outcome.verdict.pass);
        assert_eq!(outcome.verdict.transforms[1].reordered, ["right:stub/000"]);
    }

    #[test]
    fn a_git_probe_on_both_sides_is_compared_and_a_different_value_fails() {
        let other = PROBE.replace("600188d7", "deadbeef");
        let (left, right) = pair();
        let outcome = compare_sides(
            &gate_with(Vec::new()),
            &probe_record(left, PROBE, None),
            &probe_record(right, &other, None),
        );
        assert!(!outcome.verdict.pass);
        assert_eq!(outcome.verdict.transforms.len(), 1);
        // The same value on both sides passes with no probe transform.
        let (left, right) = pair();
        let outcome = compare_sides(
            &gate_with(Vec::new()),
            &probe_record(left, PROBE, None),
            &probe_record(right, PROBE, None),
        );
        assert!(outcome.verdict.pass);
        assert_eq!(outcome.verdict.transforms.len(), 1);
        // has_changes is compared too.
        let changed = PROBE.replace("false", "true");
        let (left, right) = pair();
        let outcome = compare_sides(
            &gate_with(Vec::new()),
            &probe_record(left, PROBE, None),
            &probe_record(right, &changed, None),
        );
        assert!(!outcome.verdict.pass);
    }

    #[test]
    fn only_the_exact_probe_shape_is_removed() {
        for odd in [
            r#","workspaces":{"/r/project":{"latest_git_commit_hash":"600188d7","has_changes":false,"extra":1}}"#,
            r#","workspaces":{"/r/a":{"latest_git_commit_hash":"1"},"/r/b":{"latest_git_commit_hash":"2"}}"#,
            r#","workspaces":{"/r/project":{"has_changes":false}}"#,
            r#","workspaces":{"/r/project":{"latest_git_commit_hash":7}}"#,
            r#","workspaces":"x""#,
        ] {
            let record = probe_record(pair().0, odd, None).stub_records.remove(0);
            assert_eq!(strip_git_probe(&record), record, "{odd}");
            let (left, right) = pair();
            let outcome = compare_sides(
                &gate_with(Vec::new()),
                &probe_record(left, odd, None),
                &probe_record(right, "", None),
            );
            assert!(!outcome.verdict.pass, "{odd}");
        }
    }

    #[test]
    fn removing_the_probe_changes_only_the_two_turn_metadata_copies_and_the_length() {
        let record = probe_record(pair().0, PROBE, None).stub_records.remove(0);
        let stripped: Value = serde_json::from_str(&strip_git_probe(&record)).unwrap();
        let clean = probe_record(pair().0, "", None).stub_records.remove(0);
        // The result is byte for byte the record codex would have sent without it.
        assert_eq!(strip_git_probe(&record), clean);
        let body = stripped["body"].as_str().unwrap();
        assert_eq!(
            stripped["headers"][2][1].as_str().unwrap(),
            body.len().to_string()
        );
        // A header that did not match the raw body length is left alone.
        let odd = probe_record(pair().0, PROBE, Some(7))
            .stub_records
            .remove(0);
        let value: Value = serde_json::from_str(&strip_git_probe(&odd)).unwrap();
        assert_eq!(value["headers"][2][1].as_str().unwrap(), "7");
        // A probe in only one of the two copies is removed from that copy.
        let mut one_copy: Value = serde_json::from_str(&record).unwrap();
        one_copy["headers"][1] = serde_json::json!([TURN_METADATA, turn_metadata("")]);
        let one_copy = one_copy.to_string();
        let stripped: Value = serde_json::from_str(&strip_git_probe(&one_copy)).unwrap();
        assert_eq!(
            stripped["headers"][1][1].as_str().unwrap(),
            turn_metadata("")
        );
        assert!(!stripped["body"].as_str().unwrap().contains("workspaces"));
    }
}
