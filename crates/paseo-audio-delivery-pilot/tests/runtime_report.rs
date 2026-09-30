use std::fs;
use std::process::Command;

#[test]
fn local_runtime_binary_emits_machine_readable_unsigned_evidence() {
    let root =
        std::env::temp_dir().join(format!("paseo-local-runtime-report-{}", std::process::id()));
    if root.exists() {
        fs::remove_dir_all(&root).expect("old runtime report root is removed");
    }

    let output = Command::new(env!("CARGO_BIN_EXE_paseo-local-runtime"))
        .args([
            "--root",
            root.to_str().unwrap(),
            "--afinfo",
            "/usr/bin/afinfo",
        ])
        .output()
        .expect("runtime evidence binary launches");

    assert_eq!(output.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON parses");
    assert_eq!(report["schemaVersion"], 1);
    assert_eq!(report["claim"], "local_unsigned_runtime");
    assert_eq!(report["audio"]["probeExitCode"], 0);
    assert_eq!(report["audio"]["sampleRate"], 16_000);
    assert_eq!(report["process"]["exitCode"], 7);
    assert_eq!(report["delivery"]["signing"], "unsigned");
    assert_eq!(
        report["delivery"]["steps"],
        serde_json::json!([
            "install:1.0.0",
            "failed_update:checksum_mismatch",
            "update:1.1.0",
            "rollback:1.0.0",
            "uninstall:retained_state"
        ])
    );
    assert_eq!(report["native"]["status"], "not_requested");
    assert_eq!(
        fs::read(root.join("runtime-report.json")).expect("raw report is retained"),
        output.stdout
    );

    fs::remove_dir_all(root).expect("runtime report root is removed");
}
