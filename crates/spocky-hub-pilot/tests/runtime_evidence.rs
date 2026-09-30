use std::process::Command;

use serde_json::Value;

#[test]
fn runtime_evidence_reports_restart_authority_and_failure_boundaries() {
    let output = Command::new(env!("CARGO_BIN_EXE_hub-runtime-evidence"))
        .output()
        .expect("run Hub runtime evidence binary");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let evidence: Value = serde_json::from_slice(&output.stdout).expect("parse evidence JSON");
    assert_eq!(evidence["schemaVersion"], 1);
    assert_eq!(evidence["storeSemantics"], "singleProcessFileSnapshot");
    assert_eq!(evidence["restart"]["ownerAuthorized"], true);
    assert_eq!(evidence["authorizationFailures"]["memberRegistration"], 1);
    assert_eq!(evidence["sessionFailures"]["permissionMismatch"], 1);
    assert_eq!(evidence["sessionFailures"]["supersededGeneration"], 1);
    assert_eq!(evidence["sessions"]["connectedGeneration"], 1);
    assert_eq!(evidence["sessions"]["continuedGeneration"], 2);
    assert_eq!(evidence["limitations"][0], "not PGlite");
    assert_eq!(evidence["limitations"][1], "not PostgreSQL");
}
