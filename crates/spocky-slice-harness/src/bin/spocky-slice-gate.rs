//! Runs one Phase 3 slice gate with the pinned Paseo CLI against two daemons
//! and enforces the comparison.
//!
//! Usage:
//! `spocky-slice-gate <gate> --left <original|spocky> --right <original|spocky>
//!   --paseo-root <dir> --node-bin <dir> --codex <path> --stub <path>
//!   [--spocky-daemon <path>] --evidence <dir>`
//!
//! Exit 0 only when both sides pass every positive check, no process
//! survives, and the normalized comparison is equivalent. Exit 1 on any
//! mismatch or failed check, 2 on usage or harness setup errors.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use spocky_slice_harness::compare::{compare_sides, differing_artifacts};
use spocky_slice_harness::gates;
use spocky_slice_harness::side::{DaemonKind, Tools, run_side};

const USAGE: &str = "usage: spocky-slice-gate <gate> --left <original|spocky> --right <original|spocky> --paseo-root <dir> --node-bin <dir> --codex <path> --stub <path> [--spocky-daemon <path>] --evidence <dir>";

struct Options {
    gate: String,
    left: DaemonKind,
    right: DaemonKind,
    tools: Tools,
    evidence: PathBuf,
}

fn parse() -> Result<Options, String> {
    let mut arguments = std::env::args().skip(1);
    let gate = arguments.next().ok_or(USAGE)?;
    let mut values = std::collections::BTreeMap::new();
    while let Some(flag) = arguments.next() {
        let known = [
            "--left",
            "--right",
            "--paseo-root",
            "--node-bin",
            "--codex",
            "--stub",
            "--spocky-daemon",
            "--evidence",
        ];
        if !known.contains(&flag.as_str()) {
            return Err(format!("unknown flag {flag}\n{USAGE}"));
        }
        let value = arguments.next().ok_or(format!("{flag} needs a value"))?;
        if values.insert(flag.clone(), value).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    let spocky_daemon = values.remove("--spocky-daemon").map(PathBuf::from);
    let mut take = |flag: &str| {
        values
            .remove(flag)
            .ok_or(format!("missing {flag}\n{USAGE}"))
    };
    let left = DaemonKind::parse(&take("--left")?)?;
    let right = DaemonKind::parse(&take("--right")?)?;
    let tools = Tools {
        paseo_root: PathBuf::from(take("--paseo-root")?),
        node_bin: PathBuf::from(take("--node-bin")?),
        codex: PathBuf::from(take("--codex")?),
        stub: PathBuf::from(take("--stub")?),
        spocky_daemon,
    };
    let evidence = PathBuf::from(take("--evidence")?);
    if [left, right].contains(&DaemonKind::Spocky) && tools.spocky_daemon.is_none() {
        return Err("a spocky side needs --spocky-daemon".into());
    }
    Ok(Options {
        gate,
        left,
        right,
        tools,
        evidence,
    })
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    fs::write(path, bytes).map_err(|error| format!("write {}: {error}", path.display()))
}

fn run() -> Result<bool, String> {
    let options = parse()?;
    let gate = gates::by_id(&options.gate).ok_or(format!("unknown gate {}", options.gate))?;
    fs::create_dir_all(&options.evidence).map_err(|error| error.to_string())?;
    let left_dir = options
        .evidence
        .join(format!("left-{}", options.left.label()));
    let right_dir = options
        .evidence
        .join(format!("right-{}", options.right.label()));
    for directory in [&left_dir, &right_dir] {
        fs::create_dir(directory)
            .map_err(|error| format!("create {}: {error}", directory.display()))?;
    }
    eprintln!(
        "spocky-slice-gate: {} left side ({})",
        gate.id,
        options.left.label()
    );
    let left = run_side(&gate, options.left, &options.tools, &left_dir)?;
    eprintln!(
        "spocky-slice-gate: {} right side ({})",
        gate.id,
        options.right.label()
    );
    let right = run_side(&gate, options.right, &options.tools, &right_dir)?;
    let outcome = compare_sides(&gate, &left, &right);
    write_json(&options.evidence.join("rules.json"), &outcome.rules)?;
    if let Some(manifest) = &outcome.manifest {
        write_json(&options.evidence.join("manifest.json"), manifest)?;
        let differing = differing_artifacts(manifest);
        fs::write(
            options.evidence.join("differing.txt"),
            differing.join("\n") + "\n",
        )
        .map_err(|error| error.to_string())?;
    }
    write_json(&options.evidence.join("verdict.json"), &outcome.verdict)?;
    let verdict = &outcome.verdict;
    eprintln!(
        "spocky-slice-gate: {} {} vs {}: {} (differences {}, rules {}, check failures {}, survivors {}, harness errors {})",
        verdict.gate,
        verdict.left,
        verdict.right,
        if verdict.pass { "PASS" } else { "FAIL" },
        verdict.differences.len(),
        verdict.rule_count,
        verdict.check_failures.len(),
        verdict.survivors.len(),
        verdict.harness_errors.len(),
    );
    for line in verdict
        .discovery_error
        .iter()
        .chain(&verdict.comparison_error)
        .chain(&verdict.check_failures)
        .chain(&verdict.survivors)
        .chain(&verdict.harness_errors)
    {
        eprintln!("  {line}");
    }
    Ok(verdict.pass)
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(error) => {
            eprintln!("spocky-slice-gate: {error}");
            ExitCode::from(2)
        }
    }
}
