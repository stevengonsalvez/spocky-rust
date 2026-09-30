use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use paseo_differential::{
    CapturePlan, DifferentialManifest, ExecutionCounts, ProcessSpec, RunPlan, Scenario,
    run_differential,
};

const BASELINE: &str = "5de45e208690b0efc51c59a585ae9729325a9204";

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repository_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .ok_or("cannot resolve repository root")?;
    let reference_root = repository_root.join(".baselines/paseo-runtime");
    verify_reference(&reference_root)?;

    let original = ProcessSpec {
        program: reference_root.join("node_modules/.bin/tsx"),
        arguments: vec![
            repository_root
                .join("scripts/phase2/original-lifecycle-driver.ts")
                .display()
                .to_string(),
        ],
        environment: BTreeMap::from([(
            "PASEO_REFERENCE_RUNTIME".into(),
            reference_root.display().to_string(),
        )]),
        timeout_ms: 10_000,
    };
    let rust = ProcessSpec {
        program: repository_root.join("target/debug/paseo-lifecycle-driver"),
        arguments: Vec::new(),
        environment: BTreeMap::new(),
        timeout_ms: 10_000,
    };
    let plan = RunPlan {
        scenario: Scenario {
            id: "full-lifecycle-runtime".into(),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            initial_files: Vec::new(),
            expected_counts: ExecutionCounts {
                fixtures: 8,
                assertions: 26,
            },
        },
        original,
        rust,
        state_environment_variable: "PASEO_DIFFERENTIAL_STATE".into(),
        captures: CapturePlan {
            structured_output: Some("output/structured.json".into()),
            counts: Some("output/counts.json".into()),
            ..CapturePlan::default()
        },
        normalization_rules: Vec::new(),
    };
    let report = run_differential(&plan)?;
    let output_path = std::env::args_os().nth(1).map_or_else(
        || repository_root.join("evidence/raw/phase2/lifecycle-differential.json"),
        PathBuf::from,
    );
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output_path, DifferentialManifest::to_bytes(&report)?)?;
    println!("{}", output_path.display());
    if !report.equivalent {
        return Err(format!("lifecycle differences: {:?}", report.differences).into());
    }
    Ok(())
}

fn verify_reference(reference_root: &Path) -> Result<(), Box<dyn Error>> {
    let root = reference_root.display().to_string();
    let head = Command::new("git")
        .args(["-C", &root, "rev-parse", "HEAD"])
        .output()?;
    let actual = String::from_utf8(head.stdout)?.trim().to_owned();
    if !head.status.success() || actual != BASELINE {
        return Err(format!("reference HEAD mismatch: expected {BASELINE}, got {actual}").into());
    }
    let status = Command::new("git")
        .args(["-C", &root, "status", "--porcelain", "--untracked-files=no"])
        .output()?;
    if !status.status.success() || !status.stdout.is_empty() {
        return Err("reference tracked tree is dirty".into());
    }
    Ok(())
}
