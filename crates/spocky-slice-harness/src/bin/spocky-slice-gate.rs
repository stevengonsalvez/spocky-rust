//! Runs one Phase 3 slice gate with the pinned Paseo CLI against two daemons
//! and enforces the comparison.
//!
//! Usage:
//! `spocky-slice-gate <gate> --left <original|spocky> --right <original|spocky>
//!   --paseo-root <dir> --node-bin <dir> --codex <path> --stub <path>
//!   [--spocky-daemon <path>] [--order-reference <side.json>]...
//!   [--order-history <dir>] --evidence <dir>`
//!
//! A spocky right side is also checked against the codex invocation order of
//! the original observations: each `--order-reference` side (the self-check
//! sides), the original sides of the latest [`ORDER_HISTORY`] earlier runs
//! with the same identity whose self-check passed (in the `--order-history`
//! directory, by default the one holding this run), and this run's original
//! left side. A pair every original agrees on and the spocky side inverts
//! fails the verdict. Every pair run writes `order.json` with its identity,
//! the references it used, and the pairs excluded as unstable.
//!
//! Exit 0 only when both sides pass every positive check, no process
//! survives, and the normalized comparison is equivalent. Exit 1 on any
//! mismatch or failed check, 2 on usage or harness setup errors.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::Value;
use spocky_slice_harness::compare::{Verdict, compare_sides, differing_artifacts, stable_pairs};
use spocky_slice_harness::gates;
use spocky_slice_harness::normalize::{lower_hex, sha256_hex};
use spocky_slice_harness::side::{DaemonKind, Tools, run_side};
use spocky_slice_harness::side::{GateSpec, SideRun};

const USAGE: &str = "usage: spocky-slice-gate <gate> --left <original|spocky> --right <original|spocky> --paseo-root <dir> --node-bin <dir> --codex <path> --stub <path> [--spocky-daemon <path>] [--order-reference <side.json>]... [--order-history <dir>] --evidence <dir>";

struct Options {
    gate: String,
    left: DaemonKind,
    right: DaemonKind,
    tools: Tools,
    evidence: PathBuf,
    order_references: Vec<PathBuf>,
    order_history: Option<PathBuf>,
}

fn parse() -> Result<Options, String> {
    let mut arguments = std::env::args().skip(1);
    let gate = arguments.next().ok_or(USAGE)?;
    let mut values = std::collections::BTreeMap::new();
    let mut order_references = Vec::new();
    while let Some(flag) = arguments.next() {
        if flag == "--order-reference" {
            order_references.push(PathBuf::from(
                arguments.next().ok_or("--order-reference needs a value")?,
            ));
            continue;
        }
        let known = [
            "--left",
            "--right",
            "--paseo-root",
            "--node-bin",
            "--codex",
            "--stub",
            "--spocky-daemon",
            "--order-history",
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
    let order_history = values.remove("--order-history").map(PathBuf::from);
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
        order_references,
        order_history,
    })
}

/// The codex arrival order recorded in a side's raw `side.json`.
fn recorded_order(path: &Path) -> Result<Vec<String>, String> {
    let text =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let side: Value = serde_json::from_str(&text)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    if side["kind"] != "original" {
        return Err(format!(
            "order reference {} is not an original side",
            path.display()
        ));
    }
    side["codex_order"]
        .as_array()
        .and_then(|labels| {
            labels
                .iter()
                .map(|label| label.as_str().map(str::to_owned))
                .collect()
        })
        .ok_or(format!(
            "order reference {} has no codex_order",
            path.display()
        ))
}

/// Earlier runs whose self-check sides join the codex order references.
const ORDER_HISTORY: usize = 2;

/// Lowercase hex SHA-256 of a file's bytes.
fn file_sha256(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    fs::read(path)
        .ok()
        .map(|bytes| lower_hex(&Sha256::digest(bytes)))
}

/// The commit the Paseo build root records in its `.spocky-build` marker.
fn baseline_commit(paseo_root: &Path) -> Option<String> {
    fs::read_to_string(paseo_root.join(".spocky-build"))
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("commit=").map(str::to_owned))
}

/// What a run's original sides were observed with: the gate, the Paseo
/// baseline commit its build root records, the SHA-256 of the codex and node
/// binaries, and the SHA-256 of the gate fixture (stub script, steps, and
/// checks). Observations of different identities are never mixed.
fn identity(gate: &GateSpec, tools: &Tools) -> Value {
    let script = serde_json::to_string(&gate.script).unwrap_or_default();
    serde_json::json!({
        "gate": gate.id,
        "baselineCommit": baseline_commit(&tools.paseo_root),
        "codexSha256": file_sha256(&tools.codex),
        "nodeSha256": file_sha256(&tools.node_bin.join("node")),
        "fixtureSha256": sha256_hex(&format!("{script}\n{:?}\n{:?}", gate.steps, gate.checks)),
    })
}

/// Whether every identity field is known. An identity with an unreadable
/// binary or baseline never matches history, so it can only use this run.
fn identity_complete(identity: &Value) -> bool {
    identity
        .as_object()
        .is_some_and(|fields| fields.values().all(|value| !value.is_null()))
}

/// Original `side.json` paths of the latest [`ORDER_HISTORY`] runs in
/// `history` other than `run` (newest first by name) whose self-check passed
/// with the same identity. Runs without a matching `self-check/order.json`
/// are skipped.
fn order_history(history: &Path, run: &Path, identity: &Value) -> Vec<PathBuf> {
    if !identity_complete(identity) {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(history) else {
        return Vec::new();
    };
    let mut runs: Vec<PathBuf> = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path != run)
        .collect();
    runs.sort();
    runs.reverse();
    let read = |path: PathBuf| -> Option<Value> {
        serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
    };
    runs.into_iter()
        .filter(|dir| {
            let check = dir.join("self-check");
            read(check.join("order.json")).is_some_and(|order| order["identity"] == *identity)
                && read(check.join("verdict.json")).is_some_and(|verdict| verdict["pass"] == true)
        })
        .take(ORDER_HISTORY)
        .flat_map(|dir| {
            ["left-original", "right-original"]
                .map(|side| dir.join("self-check").join(side).join("side.json"))
        })
        .collect()
}

/// Label pairs present in every reference whose order the references do
/// not agree on, as `a ~ b`.
fn unstable_pairs(references: &[&[String]]) -> Vec<String> {
    let stable = stable_pairs(references);
    let Some(first) = references.first() else {
        return Vec::new();
    };
    let common: Vec<&String> = first
        .iter()
        .filter(|label| references.iter().all(|order| order.contains(label)))
        .collect();
    let mut unstable = Vec::new();
    for (index, before) in common.iter().enumerate() {
        for after in &common[index + 1..] {
            if !stable.contains(&((*before).clone(), (*after).clone())) {
                unstable.push(format!("{before} ~ {after}"));
            }
        }
    }
    unstable
}

/// Applies the codex order check to a spocky right side and returns the
/// references used and the pairs excluded as unstable.
fn check_order(
    options: &Options,
    reference_orders: Vec<Vec<String>>,
    (left, left_dir): (&SideRun, &Path),
    right: &SideRun,
    identity: &Value,
    history: Option<&Path>,
    verdict: &mut Verdict,
) -> (Vec<String>, Vec<String>) {
    let mut used: Vec<String> = Vec::new();
    let run = options.evidence.parent().unwrap_or(&options.evidence);
    let mut orders: Vec<Vec<String>> = Vec::new();
    for path in options.order_references.iter().zip(reference_orders) {
        used.push(path.0.display().to_string());
        orders.push(path.1);
    }
    for path in history
        .map(|dir| order_history(dir, run, identity))
        .unwrap_or_default()
    {
        match recorded_order(&path) {
            Ok(order) => {
                used.push(path.display().to_string());
                orders.push(order);
            }
            Err(error) => eprintln!("spocky-slice-gate: order history skipped: {error}"),
        }
    }
    let mut references: Vec<&[String]> = orders.iter().map(Vec::as_slice).collect();
    if left.kind == DaemonKind::Original {
        used.push(left_dir.join("side.json").display().to_string());
        references.push(&left.codex_order);
    }
    verdict.apply_order_check(&references, &right.codex_order);
    let unstable = unstable_pairs(&references);
    verdict.order_check = format!(
        "{}; references {}; unstable pairs excluded: {}",
        verdict.order_check,
        used.join(", "),
        if unstable.is_empty() {
            "none".to_owned()
        } else {
            unstable.join(", ")
        }
    );
    (used, unstable)
}

/// Applies the codex order check when the right side is spocky, then
/// writes `order.json`: this run's identity, history directory, references,
/// and unstable pairs.
fn record_order(
    options: &Options,
    gate: &GateSpec,
    reference_orders: Vec<Vec<String>>,
    (left, left_dir): (&SideRun, &Path),
    right: &SideRun,
    verdict: &mut Verdict,
) -> Result<(), String> {
    let identity = identity(gate, &options.tools);
    let history = options.order_history.clone().or_else(|| {
        options
            .evidence
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    });
    let (used, unstable) = if right.kind == DaemonKind::Spocky {
        check_order(
            options,
            reference_orders,
            (left, left_dir),
            right,
            &identity,
            history.as_deref(),
            verdict,
        )
    } else {
        (Vec::new(), Vec::new())
    };
    write_json(
        &options.evidence.join("order.json"),
        &serde_json::json!({
            "identity": identity,
            "historyDir": history.as_ref().map(|dir| dir.display().to_string()),
            "references": used,
            "unstablePairs": unstable,
        }),
    )?;
    Ok(())
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    fs::write(path, bytes).map_err(|error| format!("write {}: {error}", path.display()))
}

fn run() -> Result<bool, String> {
    let options = parse()?;
    let gate = gates::by_id(&options.gate).ok_or(format!("unknown gate {}", options.gate))?;
    let reference_orders = options
        .order_references
        .iter()
        .map(|path| recorded_order(path))
        .collect::<Result<Vec<_>, _>>()?;
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
    let mut outcome = compare_sides(&gate, &left, &right);
    record_order(
        &options,
        &gate,
        reference_orders,
        (&left, &left_dir),
        &right,
        &mut outcome.verdict,
    )?;
    write_json(&options.evidence.join("rules.json"), &outcome.rules)?;
    // Named transforms live beside the manifest, which stays a plain
    // spocky-differential manifest that can be replayed as is.
    write_json(
        &options.evidence.join("transforms.json"),
        &outcome.transforms,
    )?;
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
    // A skipped comparison must never read as "differences 0".
    let differences = if verdict.compared {
        format!("differences {}", verdict.differences.len())
    } else {
        format!("NOT COMPARED, {}", verdict.comparison)
    };
    eprintln!(
        "spocky-slice-gate: {} {} vs {}: {} ({differences}, rules {}, check failures {}, survivors {}, harness errors {}, order inversions {}, order check {})",
        verdict.gate,
        verdict.left,
        verdict.right,
        if verdict.pass { "PASS" } else { "FAIL" },
        verdict.rule_count,
        verdict.check_failures.len(),
        verdict.survivors.len(),
        verdict.harness_errors.len(),
        verdict.order_inversions.len(),
        verdict.order_check,
    );
    for line in verdict
        .discovery_error
        .iter()
        .chain(&verdict.comparison_error)
        .chain(&verdict.check_failures)
        .chain(&verdict.survivors)
        .chain(&verdict.harness_errors)
        .chain(&verdict.order_inversions)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn full_identity() -> Value {
        serde_json::json!({
            "gate": "g2",
            "baselineCommit": "5de45e208690b0efc51c59a585ae9729325a9204",
            "codexSha256": "c",
            "nodeSha256": "n",
            "fixtureSha256": "a",
        })
    }

    #[test]
    fn a_codex_node_or_baseline_mismatch_contributes_nothing() {
        let history = std::env::temp_dir().join(format!(
            "spocky-p3-order-history-binaries-{}",
            std::process::id()
        ));
        let run = std::env::temp_dir().join("spocky-p3-elsewhere/g2-20261002T090000Z");
        for (field, value) in [
            ("codexSha256", "rebuilt"),
            ("nodeSha256", "rebuilt"),
            ("baselineCommit", "0000000000000000000000000000000000000000"),
        ] {
            let _ = fs::remove_dir_all(&history);
            let mut theirs = full_identity();
            theirs[field] = value.into();
            earlier_run(&history, "g2-20261002T010000Z", &theirs, true);
            assert!(
                order_history(&history, &run, &full_identity()).is_empty(),
                "{field}"
            );
            earlier_run(&history, "g2-20261002T020000Z", &full_identity(), true);
            assert_eq!(
                order_history(&history, &run, &full_identity()).len(),
                2,
                "{field}"
            );
        }
        let mut unknown = full_identity();
        unknown["codexSha256"] = Value::Null;
        earlier_run(&history, "g2-20261002T030000Z", &unknown, true);
        assert!(order_history(&history, &run, &unknown).is_empty());
        fs::remove_dir_all(&history).unwrap();
    }

    fn earlier_run(phase: &Path, name: &str, identity: &Value, pass: bool) {
        let check = phase.join(name).join("self-check");
        write(
            &check.join("order.json"),
            &serde_json::json!({ "identity": identity }).to_string(),
        );
        write(
            &check.join("verdict.json"),
            &serde_json::json!({ "pass": pass }).to_string(),
        );
    }

    #[test]
    fn order_history_takes_the_latest_passing_runs_of_the_same_identity() {
        let phase =
            std::env::temp_dir().join(format!("spocky-p3-order-history-{}", std::process::id()));
        let _ = fs::remove_dir_all(&phase);
        let identity = full_identity();
        let mut other = full_identity();
        other["fixtureSha256"] = "b".into();
        earlier_run(&phase, "g1-20261002T010000Z", &identity, true);
        earlier_run(&phase, "g1-20261002T020000Z", &identity, true);
        earlier_run(&phase, "g1-20261002T030000Z", &identity, false);
        earlier_run(&phase, "g1-20261002T040000Z", &other, true);
        earlier_run(&phase, "g1-20261002T050000Z", &identity, true);
        fs::create_dir_all(phase.join("g1-20261002T060000Z/no-order")).unwrap();
        let run = phase.join("g1-20261002T070000Z");
        earlier_run(&phase, "g1-20261002T070000Z", &identity, true);
        let history: Vec<String> = order_history(&phase, &run, &identity)
            .iter()
            .map(|path| path.strip_prefix(&phase).unwrap().display().to_string())
            .collect();
        assert_eq!(
            history,
            [
                "g1-20261002T050000Z/self-check/left-original/side.json",
                "g1-20261002T050000Z/self-check/right-original/side.json",
                "g1-20261002T020000Z/self-check/left-original/side.json",
                "g1-20261002T020000Z/self-check/right-original/side.json",
            ]
        );
        fs::remove_dir_all(&phase).unwrap();
    }

    #[test]
    fn unstable_pairs_name_what_the_originals_disagree_on() {
        let one = ["a#1", "b#1", "c#1"].map(str::to_owned);
        let two = ["a#1", "c#1", "b#1"].map(str::to_owned);
        assert_eq!(unstable_pairs(&[&one, &two, &one]), ["b#1 ~ c#1"]);
    }

    #[test]
    fn a_history_dir_of_another_identity_contributes_nothing() {
        let history = std::env::temp_dir().join(format!(
            "spocky-p3-order-history-mismatch-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&history);
        let ours = serde_json::json!({ "gate": "g2", "fixtureSha256": "a" });
        let theirs = serde_json::json!({ "gate": "g2", "fixtureSha256": "b" });
        let baseline =
            serde_json::json!({ "gate": "g2", "baseline": "other", "fixtureSha256": "a" });
        earlier_run(&history, "g2-20261002T010000Z", &theirs, true);
        earlier_run(&history, "g2-20261002T020000Z", &baseline, true);
        let run = std::env::temp_dir().join("spocky-p3-elsewhere/g2-20261002T030000Z");
        assert!(order_history(&history, &run, &ours).is_empty());
        fs::remove_dir_all(&history).unwrap();
    }
}
