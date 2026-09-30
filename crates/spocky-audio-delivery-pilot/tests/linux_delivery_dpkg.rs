//! Real `dpkg` lifecycle. Mutates `/opt/Paseo` and `/usr/bin/Paseo`, so it only runs on demand
//! inside the disposable container started by `scripts/phase2/delivery-linux-runtime.sh`.
#![cfg(target_os = "linux")]

use std::fs;
use std::path::PathBuf;

use spocky_audio_delivery_pilot::{
    DISPOSABLE_ROOT_ENV, LinuxDebLifecycleConfig, LinuxDeliveryStep, run_linux_deb_lifecycle,
};

fn lifecycle() -> (Vec<LinuxDeliveryStep>, bool) {
    assert_eq!(
        std::env::var(DISPOSABLE_ROOT_ENV).as_deref(),
        Ok("1"),
        "run only inside a disposable root"
    );
    let root = PathBuf::from("/tmp/spocky-delivery-dpkg-test");
    fs::create_dir_all(&root).unwrap();
    let config = LinuxDebLifecycleConfig {
        root,
        user: "spocky-delivery".into(),
        uid: 1000,
        gid: 1000,
        home: PathBuf::from("/home/spocky-delivery"),
        disposable_root_acknowledged: true,
    };
    run_linux_deb_lifecycle(&config).expect("deb lifecycle completes")
}

fn step<'a>(steps: &'a [LinuxDeliveryStep], operation: &str) -> &'a LinuxDeliveryStep {
    steps
        .iter()
        .find(|step| step.operation == operation)
        .unwrap_or_else(|| panic!("missing step {operation}"))
}

fn assert_install_layout(steps: &[LinuxDeliveryStep]) {
    let installed = step(steps, "install");
    assert_eq!(installed.exit_code, Some(0));
    assert_eq!(installed.observations["dpkgStatus"], "ii |1.0.0");
    assert_eq!(
        installed.observations["usrBinLink"],
        "/etc/alternatives/Paseo"
    );
    assert_eq!(
        installed.observations["alternativesLink"],
        "/opt/Paseo/Paseo"
    );
    assert_eq!(installed.observations["chromeSandbox"], "0:0:4755");

    let launched = step(steps, "install_launch");
    let output = launched.output.as_deref().unwrap();
    assert!(output.starts_with("paseo-version=1.0.0\n"), "{output}");
    assert!(output.contains("agent-state-a"), "{output}");
    assert!(
        launched.observations["launcherSandbox"].starts_with("[linux-sandbox] enabled:"),
        "{}",
        launched.observations["launcherSandbox"]
    );
}

fn assert_truncated_deb_rejected(steps: &[LinuxDeliveryStep], preserved: bool) {
    let before = step(steps, "install_launch");
    let rejected = step(steps, "corrupt_update_rejected");
    assert_ne!(rejected.exit_code, Some(0));
    assert!(preserved);
    assert_eq!(rejected.active_version.as_deref(), Some("1.0.0"));
    assert_eq!(rejected.executable_digest, before.executable_digest);
    assert_eq!(rejected.state_digest, before.state_digest);
    assert_eq!(rejected.observations["dpkgStatus"], "ii |1.0.0");
    assert!(
        step(steps, "corrupt_update_launch")
            .output
            .as_deref()
            .unwrap()
            .contains("1.0.0")
    );
}

fn assert_upgrade_downgrade_remove_purge(steps: &[LinuxDeliveryStep]) {
    let first = step(steps, "install_launch");
    let upgraded = step(steps, "valid_update_launch");
    assert_eq!(upgraded.active_version.as_deref(), Some("1.1.0"));
    assert!(
        upgraded
            .output
            .as_deref()
            .unwrap()
            .starts_with("paseo-version=1.1.0\n")
    );
    assert_ne!(upgraded.executable_digest, first.executable_digest);

    let rolled_back = step(steps, "rollback_launch");
    assert_eq!(rolled_back.active_version.as_deref(), Some("1.0.0"));
    assert_eq!(rolled_back.executable_digest, first.executable_digest);

    for operation in ["remove_retain_state", "purge_retain_state"] {
        let removed = step(steps, operation);
        assert_eq!(removed.exit_code, Some(0), "{operation}");
        assert_eq!(
            removed.observations["installPrefixPresent"], "false",
            "{operation}"
        );
        assert_eq!(removed.active_version, None, "{operation}");
        assert_eq!(removed.state_digest, first.state_digest, "{operation}");
        // The baseline postrm names /usr/bin/Paseo where dpkg registered /opt/Paseo/Paseo, so
        // the --remove is a no-op. dpkg's own cleanup of the vanished alternative leaves no link.
        assert_eq!(removed.observations["usrBinLink"], "absent", "{operation}");
        assert_eq!(
            removed.observations["alternativesLink"], "absent",
            "{operation}"
        );
    }
    let removal_output = step(steps, "remove_retain_state")
        .output
        .as_deref()
        .unwrap();
    assert!(
        removal_output.contains("update-alternatives: warning"),
        "{removal_output}"
    );
    assert!(step(steps, "remove_retain_state").observations["dpkgStatus"].starts_with("rc"));
    assert_eq!(
        step(steps, "purge_retain_state").observations["dpkgStatus"],
        "not-installed"
    );
}

// One lifecycle: three tests would race on the shared /opt and /usr/bin paths.
#[test]
#[ignore = "mutates /opt and /usr/bin; run only in the disposable Linux container"]
fn dpkg_lifecycle_matches_baseline_layout_rejection_and_state_retention() {
    let (steps, preserved) = lifecycle();
    assert_install_layout(&steps);
    assert_truncated_deb_rejected(&steps, preserved);
    assert_upgrade_downgrade_remove_purge(&steps);
}
