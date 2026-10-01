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
use spocky_differential::{
    Artifact, Comparison, DifferentialManifest, ExecutionCounts, NormalizationRule,
    NormalizationTarget, Observation, ObservationSlot, Scenario, compare_observations,
};

use crate::normalize::{
    Alphabet, IdShape, SideFacts, SideInput, Text, derived_digests, distinct_ids, mask, rules_for,
    value_classes,
};
use crate::side::{CapturedFile, Exit, GateSpec, SideRun, StepRun, failed_checks};

/// Generated id shapes Paseo and Codex mint on the G1 path.
pub const ID_SHAPES: [IdShape; 6] = [
    IdShape::Uuid,
    IdShape::Prefixed {
        prefix: "wks_",
        len: 16,
        alphabet: Alphabet::LowerHex,
    },
    IdShape::Prefixed {
        prefix: "prj_",
        len: 16,
        alphabet: Alphabet::LowerHex,
    },
    IdShape::Prefixed {
        prefix: "cid_",
        len: 32,
        alphabet: Alphabet::LowerHex,
    },
    IdShape::Prefixed {
        prefix: "srv_",
        len: 12,
        alphabet: Alphabet::Base64Url,
    },
    IdShape::Prefixed {
        prefix: "",
        len: 64,
        alphabet: Alphabet::LowerHex,
    },
];

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

/// Compared artifacts of one side, in canonical order.
#[must_use]
pub fn side_artifacts(side: &SideRun) -> Vec<Artifact> {
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
        artifacts.push(Artifact::new(
            format!("stub/{index:03}"),
            record.clone().into_bytes(),
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
/// raw bytes. Masking hides generated ids and names verified digests by kind.
#[must_use]
pub fn canonical_state(state: &[CapturedFile], all_texts: &[&str]) -> Vec<Artifact> {
    let found = distinct_ids(all_texts, &ID_SHAPES);
    let derived = derived_digests(&found);
    let mut keyed: Vec<(String, String, Vec<u8>)> = state
        .iter()
        .map(|file| {
            let content = String::from_utf8_lossy(&file.bytes);
            (
                mask(&file.path, &ID_SHAPES, &derived),
                mask(&content, &ID_SHAPES, &derived),
                state_bytes(file),
            )
        })
        .collect();
    keyed.sort();
    keyed
        .into_iter()
        .enumerate()
        .map(|(index, (_, _, bytes))| Artifact::new(format!("state/{index:03}"), bytes))
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
}

/// Comparison output: the verdict and, when comparison ran, the manifest.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub verdict: Verdict,
    pub manifest: Option<DifferentialManifest>,
    pub rules: Vec<NormalizationRule>,
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
) -> (Vec<Artifact>, Vec<Artifact>, ExecutionCounts) {
    let artifacts = side_artifacts(side);
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
    let state = canonical_state(&side.state, &raw_refs);
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
    let (left_artifacts, left_state, left_counts) = prepare_side(gate, left);
    let (right_artifacts, right_state, right_counts) = prepare_side(gate, right);
    let left_texts = texts(&left_artifacts, &left_state);
    let right_texts = texts(&right_artifacts, &right_state);
    let (left_facts, right_facts) = (facts(left), facts(right));
    let left_input = SideInput {
        facts: &left_facts,
        texts: left_texts.iter().map(|text| text.text.as_str()).collect(),
        extracted: left.extracted.clone(),
    };
    let right_input = SideInput {
        facts: &right_facts,
        texts: right_texts.iter().map(|text| text.text.as_str()).collect(),
        extracted: right.extracted.clone(),
    };

    let scenario = Scenario {
        id: format!("phase3-{}", gate.id),
        arguments: Vec::new(),
        environment: BTreeMap::new(),
        initial_files: Vec::new(),
        expected_counts,
    };
    let (manifest, rules, discovery_error, comparison_error) =
        match value_classes(&left_input, &right_input, &ID_SHAPES) {
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
        },
        manifest,
        rules,
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
            for index in 0..left.len().max(right.len()) {
                match (left.get(index), right.get(index)) {
                    (Some(a), Some(b)) if a == b => {}
                    (Some(a), Some(b)) if a.name == b.name => names.push(a.name.clone()),
                    (Some(a), Some(b)) => names.push(format!("{} vs {}", a.name, b.name)),
                    (Some(a), None) => names.push(format!("{} (left only)", a.name)),
                    (None, Some(b)) => names.push(format!("{} (right only)", b.name)),
                    (None, None) => {}
                }
            }
        }
    }
    names
}
