use std::collections::BTreeMap;
use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use paseo_differential::{
    Artifact, CapturePlan, Comparison, DifferentialManifest, ExecutionCounts,
    NormalizationCategory, NormalizationRule, NormalizationTarget, Observation, ObservationSlot,
    ProcessSpec, RunPlan, Scenario, compare_observations, run_differential,
};
use serde_json::json;

fn scenario() -> Scenario {
    Scenario {
        id: "create-agent".into(),
        arguments: vec!["create".into()],
        environment: BTreeMap::from([("MODE".into(), "test".into())]),
        initial_files: vec![Artifact::new("seed.json", br#"{"agents":[]}"#.to_vec())],
        expected_counts: ExecutionCounts {
            fixtures: 1,
            assertions: 7,
        },
    }
}

fn observation(id: &str, timestamp: &str, temp: &str) -> Observation {
    Observation {
        structured_output: ObservationSlot::Value(json!({
            "id": id,
            "createdAt": timestamp,
            "workspace": temp,
            "nullable": null,
            "ordered": ["first", "second"]
        })),
        stdout: ObservationSlot::Value(format!("created {id} at {temp}\n").into_bytes()),
        stderr: ObservationSlot::Value(Vec::new()),
        exit_code: ObservationSlot::Value(0),
        artifacts: ObservationSlot::Value(vec![Artifact::new(
            "agent.json",
            format!(r#"{{"id":"{id}"}}"#).into_bytes(),
        )]),
        state: ObservationSlot::Value(vec![Artifact::new("state.db", vec![1, 2, 3])]),
        screenshots: ObservationSlot::Value(vec![Artifact::new("main.png", vec![4, 5, 6])]),
        accessibility: ObservationSlot::Value(json!({"role": "button", "name": "Create"})),
        performance: ObservationSlot::Value(json!({"samples_ms": [8, 9, 10]})),
        recovery: ObservationSlot::Value(json!({"restart": "resumed"})),
        counts: ObservationSlot::Value(ExecutionCounts {
            fixtures: 1,
            assertions: 7,
        }),
    }
}

fn rules() -> Vec<NormalizationRule> {
    vec![
        NormalizationRule::exact_json(
            "agent-id",
            NormalizationCategory::GeneratedId,
            "agent IDs are generated independently",
            "paseo-differential",
            "/id",
            vec!["original-123", "rust-456"],
        ),
        NormalizationRule::exact_json(
            "created-at",
            NormalizationCategory::WallClock,
            "creation time comes from each process clock",
            "paseo-differential",
            "/createdAt",
            vec!["2026-09-29T10:00:00Z", "2026-09-29T10:00:01Z"],
        ),
        NormalizationRule::exact_json(
            "workspace-path",
            NormalizationCategory::TemporaryPath,
            "each side receives a separate disposable state root",
            "paseo-differential",
            "/workspace",
            vec!["/tmp/original", "/tmp/rust"],
        ),
        NormalizationRule::exact_text(
            "stdout-agent-id",
            NormalizationCategory::GeneratedId,
            "stdout repeats the generated agent ID",
            "paseo-differential",
            NormalizationTarget::Stdout,
            vec!["original-123", "rust-456"],
        ),
        NormalizationRule::exact_text(
            "stdout-workspace",
            NormalizationCategory::TemporaryPath,
            "stdout repeats the disposable state root",
            "paseo-differential",
            NormalizationTarget::Stdout,
            vec!["/tmp/original", "/tmp/rust"],
        ),
        NormalizationRule::exact_text(
            "artifact-agent-id",
            NormalizationCategory::GeneratedId,
            "agent artifact repeats the generated agent ID",
            "paseo-differential",
            NormalizationTarget::Artifact("agent.json".into()),
            vec!["original-123", "rust-456"],
        ),
    ]
}

#[test]
fn allowed_values_normalize_without_changing_semantics() {
    let original = observation("original-123", "2026-09-29T10:00:00Z", "/tmp/original");
    let rust = observation("rust-456", "2026-09-29T10:00:01Z", "/tmp/rust");

    let comparison = compare_observations(&scenario(), original, rust, &rules())
        .expect("documented nondeterminism normalizes");

    assert!(comparison.equivalent);
    assert!(comparison.differences.is_empty());
    assert_eq!(comparison.executed_fixtures, 1);
    assert_eq!(comparison.executed_assertions, 7);
}

#[test]
fn manifest_retains_raw_artifacts_and_digests() {
    let original = observation("original-123", "2026-09-29T10:00:00Z", "/tmp/original");
    let rust = observation("rust-456", "2026-09-29T10:00:01Z", "/tmp/rust");

    let comparison = compare_observations(&scenario(), original.clone(), rust.clone(), &rules())
        .expect("comparison succeeds");

    assert_eq!(comparison.original.raw, original);
    assert_eq!(comparison.rust.raw, rust);
    assert_ne!(comparison.original.raw, comparison.original.normalized);
    assert_eq!(comparison.original.raw_digests["stdout"].len(), 64);
    assert_eq!(
        comparison.original.raw_digests["artifact:agent.json"].len(),
        64
    );
    assert_eq!(
        comparison.original.raw_digests["screenshot:main.png"].len(),
        64
    );
}

#[test]
fn manifest_serialization_is_deterministic() {
    let build = || {
        compare_observations(
            &scenario(),
            observation("original-123", "2026-09-29T10:00:00Z", "/tmp/original"),
            observation("rust-456", "2026-09-29T10:00:01Z", "/tmp/rust"),
            &rules(),
        )
        .expect("comparison succeeds")
    };

    let first = DifferentialManifest::to_bytes(&build()).expect("manifest serializes");
    let second = DifferentialManifest::to_bytes(&build()).expect("manifest serializes again");

    assert_eq!(first, second);
}

#[test]
fn forbidden_semantic_changes_remain_different() {
    let original = observation("same-id", "same-time", "/tmp/same");
    let mut rust = original.clone();
    rust.structured_output = ObservationSlot::Value(json!({
        "id": "same-id",
        "createdAt": "same-time",
        "workspace": "/tmp/same",
        "nullable": null,
        "ordered": ["second", "first"]
    }));
    rust.stderr = ObservationSlot::Error("provider failed".into());
    rust.recovery = ObservationSlot::Missing;

    let comparison = compare_observations(&scenario(), original, rust, &[])
        .expect("semantic differences produce a report");

    assert!(!comparison.equivalent);
    assert_eq!(
        comparison.differences,
        vec![
            Comparison::different("structured_output"),
            Comparison::different("stderr"),
            Comparison::different("recovery"),
        ]
    );
}

#[test]
fn executable_runner_uses_identical_inputs_and_isolated_state() {
    let plan = runner_plan();

    let report = run_differential(&plan).expect("real processes run and compare");

    assert!(report.equivalent, "{:?}", report.differences);
    assert_ne!(report.original.raw.stdout, report.rust.raw.stdout);
    assert_eq!(
        report.original.normalized.stdout,
        report.rust.normalized.stdout
    );
    assert_eq!(
        report.original.raw.state,
        ObservationSlot::Value(vec![Artifact::new(
            "seed.json",
            br#"{"agents":[]}"#.to_vec()
        )])
    );
    assert_eq!(report.executed_fixtures, 1);
    assert_eq!(report.executed_assertions, 7);
}

#[test]
fn executable_runner_rejects_identical_capture_failures() {
    let mut plan = runner_plan();
    plan.captures.screenshots = vec!["screens/missing.png".into()];

    let report = run_differential(&plan).expect("capture failures produce a report");

    assert!(!report.equivalent);
    assert_eq!(
        report.differences,
        vec![Comparison::different("screenshots")]
    );
    assert!(matches!(
        report.original.raw.screenshots,
        ObservationSlot::Error(_)
    ));
    assert!(matches!(
        report.rust.raw.screenshots,
        ObservationSlot::Error(_)
    ));
}

#[test]
fn executable_runner_terminates_timed_out_processes() {
    let process = ProcessSpec {
        program: "/bin/sh".into(),
        arguments: vec![
            "-c".into(),
            "printf '{\"fixtures\":0,\"assertions\":0}' > counts.json; sleep 2".into(),
        ],
        environment: BTreeMap::new(),
        timeout_ms: 50,
    };
    let plan = RunPlan {
        scenario: Scenario {
            id: "timeout".into(),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            initial_files: Vec::new(),
            expected_counts: ExecutionCounts {
                fixtures: 0,
                assertions: 0,
            },
        },
        original: process.clone(),
        rust: process,
        state_environment_variable: "PASEO_DIFFERENTIAL_STATE".into(),
        captures: CapturePlan {
            counts: Some("counts.json".into()),
            ..CapturePlan::default()
        },
        normalization_rules: Vec::new(),
    };
    let started = std::time::Instant::now();

    let report = run_differential(&plan).expect("timeouts produce a report");

    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert!(!report.equivalent);
    assert_eq!(report.differences, vec![Comparison::different("exit_code")]);
    assert!(matches!(
        report.original.raw.exit_code,
        ObservationSlot::Error(ref message) if message == "process timed out after 50 ms"
    ));
    assert!(matches!(
        report.rust.raw.exit_code,
        ObservationSlot::Error(ref message) if message == "process timed out after 50 ms"
    ));
}

#[test]
fn executable_runner_rejects_identical_malformed_json() {
    let process = ProcessSpec {
        program: "/bin/sh".into(),
        arguments: vec![
            "-c".into(),
            "printf 'not-json' > structured.json; printf '{\"fixtures\":0,\"assertions\":0}' > counts.json"
                .into(),
        ],
        environment: BTreeMap::new(),
        timeout_ms: 1_000,
    };
    let plan = minimal_plan(
        "malformed-json",
        process,
        CapturePlan {
            structured_output: Some("structured.json".into()),
            counts: Some("counts.json".into()),
            ..CapturePlan::default()
        },
    );

    let report = run_differential(&plan).expect("malformed captures produce a report");

    assert!(!report.equivalent);
    assert_eq!(
        report.differences,
        vec![Comparison::different("structured_output")]
    );
}

#[test]
fn executable_runner_rejects_identical_spawn_failures() {
    let process = ProcessSpec {
        program: "/definitely/missing/paseo-program".into(),
        arguments: Vec::new(),
        environment: BTreeMap::new(),
        timeout_ms: 1_000,
    };
    let plan = minimal_plan("spawn-failure", process, CapturePlan::default());

    let report = run_differential(&plan).expect("spawn failures produce a report");

    assert!(!report.equivalent);
    assert_eq!(
        report.differences,
        vec![
            Comparison::different("stdout"),
            Comparison::different("stderr"),
            Comparison::different("exit_code"),
            Comparison::different("counts:expected"),
        ]
    );
}

fn minimal_plan(id: &str, process: ProcessSpec, captures: CapturePlan) -> RunPlan {
    RunPlan {
        scenario: Scenario {
            id: id.into(),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            initial_files: Vec::new(),
            expected_counts: ExecutionCounts {
                fixtures: 0,
                assertions: 0,
            },
        },
        original: process.clone(),
        rust: process,
        state_environment_variable: "PASEO_DIFFERENTIAL_STATE".into(),
        captures,
        normalization_rules: Vec::new(),
    }
}

#[test]
fn binary_writes_the_deterministic_manifest() {
    let directory = TestDirectory::new();
    let plan_path = directory.path.join("plan.json");
    let manifest_path = directory.path.join("manifest.json");
    fs::write(
        &plan_path,
        serde_json::to_vec_pretty(&runner_plan()).expect("plan serializes"),
    )
    .expect("plan writes");

    let output = Command::new(env!("CARGO_BIN_EXE_paseo-differential"))
        .args([&plan_path, &manifest_path])
        .output()
        .expect("binary executes");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest: DifferentialManifest =
        serde_json::from_slice(&fs::read(&manifest_path).expect("binary wrote manifest"))
            .expect("manifest parses");
    assert!(manifest.equivalent);
    assert_eq!(manifest.executed_fixtures, 1);
    assert_eq!(manifest.executed_assertions, 7);
}

fn runner_plan() -> RunPlan {
    let script = r#"
set -eu
root="$PASEO_DIFFERENTIAL_STATE"
mkdir -p "$root/output" "$root/screens"
printf '{"id":"%s","workspace":"%s","ordered":[1,2]}' "$SIDE-id" "$root" > "$root/output/structured.json"
printf '{"id":"%s"}' "$SIDE-id" > "$root/output/artifact.json"
printf '{"role":"status","name":"Ready"}' > "$root/output/accessibility.json"
printf '{"samples_ms":[4,5,6]}' > "$root/output/performance.json"
printf '{"restart":"resumed"}' > "$root/output/recovery.json"
printf '{"fixtures":1,"assertions":7}' > "$root/output/counts.json"
printf '\001\002\003' > "$root/screens/main.png"
printf 'created %s-id at %s\n' "$SIDE" "$root"
"#;
    let process = |side: &str| ProcessSpec {
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), script.into()],
        environment: BTreeMap::from([("SIDE".into(), side.into())]),
        timeout_ms: 30_000,
    };
    RunPlan {
        scenario: scenario(),
        original: process("original"),
        rust: process("rust"),
        state_environment_variable: "PASEO_DIFFERENTIAL_STATE".into(),
        captures: CapturePlan {
            structured_output: Some("output/structured.json".into()),
            artifacts: vec!["output/artifact.json".into()],
            state: vec!["seed.json".into()],
            screenshots: vec!["screens/main.png".into()],
            accessibility: Some("output/accessibility.json".into()),
            performance: Some("output/performance.json".into()),
            recovery: Some("output/recovery.json".into()),
            counts: Some("output/counts.json".into()),
        },
        normalization_rules: vec![
            NormalizationRule::exact_json(
                "runner-id",
                NormalizationCategory::GeneratedId,
                "each implementation generates its own ID",
                "paseo-differential",
                "/id",
                vec!["original-id", "rust-id"],
            ),
            NormalizationRule::exact_json(
                "runner-state-root",
                NormalizationCategory::TemporaryPath,
                "runner allocates one disposable root per implementation",
                "paseo-differential",
                "/workspace",
                vec!["$PASEO_DIFFERENTIAL_STATE"],
            ),
            NormalizationRule::exact_text(
                "artifact-id",
                NormalizationCategory::GeneratedId,
                "artifact repeats generated ID",
                "paseo-differential",
                NormalizationTarget::Artifact("output/artifact.json".into()),
                vec!["original-id", "rust-id"],
            ),
            NormalizationRule::exact_text(
                "runner-stdout-id",
                NormalizationCategory::GeneratedId,
                "stdout repeats generated ID",
                "paseo-differential",
                NormalizationTarget::Stdout,
                vec!["original-id", "rust-id"],
            ),
            NormalizationRule::exact_text(
                "runner-stdout-root",
                NormalizationCategory::TemporaryPath,
                "stdout repeats disposable root",
                "paseo-differential",
                NormalizationTarget::Stdout,
                vec!["$PASEO_DIFFERENTIAL_STATE"],
            ),
        ],
    }
}

struct TestDirectory {
    path: std::path::PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock follows Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "paseo-differential-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("test directory creates");
        Self { path }
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).expect("test directory removes");
    }
}
