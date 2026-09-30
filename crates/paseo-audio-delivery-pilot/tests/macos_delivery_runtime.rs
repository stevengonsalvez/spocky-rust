#![cfg(target_os = "macos")]

use std::fs;
use std::path::{Path, PathBuf};

use paseo_audio_delivery_pilot::{
    MacOsAppDeliveryRuntime, create_unsigned_macos_app_bundle, run_macos_delivery_feasibility,
};

fn temp_directory(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "paseo-macos-delivery-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock follows Unix epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&path).expect("temporary directory is created");
    path
}

fn cleanup(path: &Path) {
    if path.exists() {
        fs::remove_dir_all(path).expect("temporary directory is removed");
    }
}

fn fixture(root: &Path, version: &str) -> PathBuf {
    let bundle = root.join(format!("Paseo-{version}.app"));
    create_unsigned_macos_app_bundle(&bundle, version).expect("unsigned app fixture is created");
    bundle
}

#[test]
fn unsigned_app_bundle_installs_and_launches_its_macos_executable() {
    let root = temp_directory("install-launch");
    let source = fixture(&root, "1.0.0");
    let mut runtime = MacOsAppDeliveryRuntime::new(
        root.join("Applications"),
        root.join("Library/Application Support/Paseo"),
    );
    runtime
        .write_user_state(b"workspace=delivery-pilot\n")
        .expect("user state is written outside the app bundle");

    let installed = runtime.install(&source).expect("bundle installs");
    let launched = runtime.launch().expect("installed executable launches");

    assert_eq!(installed.active_version.as_deref(), Some("1.0.0"));
    assert_eq!(installed.bundle_path, root.join("Applications/Paseo.app"));
    assert_eq!(launched.exit_code, Some(0));
    assert_eq!(
        String::from_utf8(launched.stdout).unwrap(),
        "paseo-version=1.0.0\nworkspace=delivery-pilot\n"
    );
    assert!(launched.stderr.is_empty());

    cleanup(&root);
}

#[test]
fn corrupt_app_update_is_rejected_without_changing_executable_or_state() {
    let root = temp_directory("rejected-update");
    let source = fixture(&root, "1.0.0");
    let corrupt = fixture(&root, "1.1.0");
    fs::write(
        corrupt.join("Contents/MacOS/Paseo"),
        b"#!/bin/sh\nprintf 'tampered\\n'\n",
    )
    .expect("update executable is corrupted after packaging");
    let mut runtime = MacOsAppDeliveryRuntime::new(
        root.join("Applications"),
        root.join("Library/Application Support/Paseo"),
    );
    runtime.write_user_state(b"state-v1\n").unwrap();
    let installed = runtime.install(&source).unwrap();

    let error = runtime
        .update(&corrupt)
        .expect_err("corrupt executable is rejected");
    let after = runtime.snapshot().expect("active bundle remains readable");
    let launched = runtime.launch().expect("old executable still launches");

    assert_eq!(error.to_string(), "app executable checksum mismatch");
    assert_eq!(after, installed);
    assert_eq!(
        String::from_utf8(launched.stdout).unwrap(),
        "paseo-version=1.0.0\nstate-v1\n"
    );
    assert_eq!(fs::read(runtime.user_state_path()).unwrap(), b"state-v1\n");

    cleanup(&root);
}

#[test]
fn app_upgrade_rollback_and_uninstall_preserve_external_user_state() {
    let root = temp_directory("lifecycle");
    let source_100 = fixture(&root, "1.0.0");
    let source_110 = fixture(&root, "1.1.0");
    let mut runtime = MacOsAppDeliveryRuntime::new(
        root.join("Applications"),
        root.join("Library/Application Support/Paseo"),
    );
    runtime.write_user_state(b"agent-state-a\n").unwrap();
    runtime.install(&source_100).unwrap();

    let upgraded = runtime.update(&source_110).expect("valid update activates");
    let upgraded_launch = runtime.launch().expect("upgraded executable launches");
    assert_eq!(upgraded.active_version.as_deref(), Some("1.1.0"));
    assert_eq!(
        String::from_utf8(upgraded_launch.stdout).unwrap(),
        "paseo-version=1.1.0\nagent-state-a\n"
    );

    let rolled_back = runtime.rollback().expect("previous app is restored");
    let rollback_launch = runtime.launch().expect("restored executable launches");
    assert_eq!(rolled_back.active_version.as_deref(), Some("1.0.0"));
    assert_eq!(
        String::from_utf8(rollback_launch.stdout).unwrap(),
        "paseo-version=1.0.0\nagent-state-a\n"
    );

    let uninstalled = runtime.uninstall(true).expect("bundle is removed");
    assert_eq!(uninstalled.active_version, None);
    assert!(!root.join("Applications/Paseo.app").exists());
    assert_eq!(
        fs::read(runtime.user_state_path()).unwrap(),
        b"agent-state-a\n"
    );

    cleanup(&root);
}

#[test]
fn feasibility_report_records_real_bundle_lifecycle_evidence() {
    let root = temp_directory("report");

    let report = run_macos_delivery_feasibility(&root).expect("delivery evidence run succeeds");

    assert_eq!(report.schema_version, 1);
    assert_eq!(report.contract_id, "P2-DELIVERY-01");
    assert_eq!(
        report.baseline,
        "paseo@5de45e208690b0efc51c59a585ae9729325a9204"
    );
    assert_eq!(report.package_format, "unsigned_macos_app_bundle");
    assert_eq!(
        report
            .steps
            .iter()
            .map(|step| step.operation.as_str())
            .collect::<Vec<_>>(),
        [
            "install_launch",
            "corrupt_update_rejected",
            "valid_update_launch",
            "rollback_launch",
            "uninstall_retain_state",
        ]
    );
    assert!(report.corrupt_update_preserved_executable);
    assert!(report.corrupt_update_preserved_state);
    assert_eq!(report.steps[0].active_version.as_deref(), Some("1.0.0"));
    assert_eq!(report.steps[2].active_version.as_deref(), Some("1.1.0"));
    assert_eq!(report.steps[3].active_version.as_deref(), Some("1.0.0"));
    assert_eq!(report.steps[4].active_version, None);
    assert_eq!(
        fs::read(&report.retained_state_path).unwrap(),
        b"agent-state-a\n"
    );
    assert!(!report.installed_bundle_path.exists());

    cleanup(&root);
}
