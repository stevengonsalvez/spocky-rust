#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use spocky_audio_delivery_pilot::{
    BASELINE_AFTER_INSTALL, BASELINE_AFTER_REMOVE, BASELINE_LAUNCHER, DISPOSABLE_ROOT_ENV,
    LinuxAppImageRuntime, LinuxDebLifecycleConfig, UpdateManifest, baseline_fixture_digests,
    create_linux_appimage_fixture, create_linux_deb_tree, run_linux_appimage_lifecycle,
    run_linux_deb_lifecycle,
};

fn temp_directory(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "spocky-linux-delivery-{label}-{}-{}",
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
    fs::remove_dir_all(path).expect("temporary directory is removed");
}

struct Fixture {
    root: PathBuf,
    runtime: LinuxAppImageRuntime,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = temp_directory(label);
        let runtime = LinuxAppImageRuntime::new(root.join("install"), root.join("home"));
        Self { root, runtime }
    }

    fn package(&self, version: &str) -> (PathBuf, UpdateManifest) {
        let path = self.root.join(format!("{version}.AppImage"));
        let bytes = create_linux_appimage_fixture(&path, version).unwrap();
        (path, UpdateManifest::describe(version, &bytes))
    }
}

fn stdout(runtime: &LinuxAppImageRuntime) -> String {
    let launched = runtime.launch().expect("installed AppImage launches");
    assert_eq!(launched.exit_code, Some(0));
    assert!(launched.stderr.is_empty());
    String::from_utf8(launched.stdout).unwrap()
}

#[test]
fn appimage_installs_links_cli_to_stable_path_and_launches_through_it() {
    let mut fixture = Fixture::new("install");
    let (source, manifest) = fixture.package("1.0.0");
    fixture
        .runtime
        .write_user_state(b"workspace=linux\n")
        .unwrap();

    let installed = fixture.runtime.install(&source, &manifest).unwrap();

    let stable = fixture.root.join("install/Paseo-x64.AppImage");
    assert_eq!(installed.active_version.as_deref(), Some("1.0.0"));
    assert_eq!(installed.cli_link_target, Some(stable.clone()));
    assert!(installed.cli_link_resolves);
    assert_eq!(
        stdout(&fixture.runtime),
        format!(
            "paseo-version=1.0.0\nargv=launch\nappimage={}\nworkspace=linux\n",
            stable.display()
        )
    );
    cleanup(&fixture.root);
}

#[test]
fn corrupt_appimage_update_is_rejected_without_changing_install_or_state() {
    let mut fixture = Fixture::new("corrupt");
    let (source, manifest) = fixture.package("1.0.0");
    let (update, update_manifest) = fixture.package("1.1.0");
    let mut damaged = fs::read(&update).unwrap();
    damaged.extend_from_slice(b"# tampered\n");
    fs::write(&update, damaged).unwrap();
    fixture.runtime.write_user_state(b"state-v1\n").unwrap();
    let installed = fixture.runtime.install(&source, &manifest).unwrap();

    let error = fixture
        .runtime
        .update(&update, &update_manifest)
        .unwrap_err();

    assert_eq!(error.to_string(), "update size mismatch");
    assert_eq!(fixture.runtime.snapshot().unwrap(), installed);
    assert!(
        !fixture
            .root
            .join("install/.Paseo-x64.AppImage.staging")
            .exists()
    );
    assert!(stdout(&fixture.runtime).starts_with("paseo-version=1.0.0\n"));
    cleanup(&fixture.root);
}

#[test]
fn appimage_update_and_rollback_keep_path_link_and_inode_reachability() {
    let mut fixture = Fixture::new("update");
    let (source_100, manifest_100) = fixture.package("1.0.0");
    let (source_110, manifest_110) = fixture.package("1.1.0");
    fixture
        .runtime
        .write_user_state(b"agent-state-a\n")
        .unwrap();
    let installed = fixture.runtime.install(&source_100, &manifest_100).unwrap();

    let upgraded = fixture.runtime.update(&source_110, &manifest_110).unwrap();
    assert_eq!(upgraded.active_version.as_deref(), Some("1.1.0"));
    assert_eq!(upgraded.retained_version.as_deref(), Some("1.0.0"));
    assert_eq!(upgraded.cli_link_target, installed.cli_link_target);
    assert!(upgraded.cli_link_resolves);
    assert!(stdout(&fixture.runtime).starts_with("paseo-version=1.1.0\n"));

    let rolled_back = fixture.runtime.rollback().unwrap();
    assert_eq!(rolled_back.active_version.as_deref(), Some("1.0.0"));
    assert_eq!(rolled_back.retained_version.as_deref(), Some("1.1.0"));
    assert_eq!(rolled_back.executable_digest, installed.executable_digest);
    assert!(stdout(&fixture.runtime).ends_with("agent-state-a\n"));
    cleanup(&fixture.root);
}

#[test]
fn appimage_uninstall_retains_state_and_leaves_the_user_owned_cli_link_dangling() {
    let mut fixture = Fixture::new("uninstall");
    let (source, manifest) = fixture.package("1.0.0");
    fixture
        .runtime
        .write_user_state(b"agent-state-a\n")
        .unwrap();
    let installed = fixture.runtime.install(&source, &manifest).unwrap();

    let removed = fixture.runtime.uninstall(true).unwrap();

    assert_eq!(removed.active_version, None);
    assert_eq!(removed.state_digest, installed.state_digest);
    assert_eq!(removed.cli_link_target, installed.cli_link_target);
    assert!(!removed.cli_link_resolves);
    assert_eq!(
        fs::read(fixture.runtime.user_state_path()).unwrap(),
        b"agent-state-a\n"
    );

    let (source, manifest) = fixture.package("1.0.0");
    fixture.runtime.install(&source, &manifest).unwrap();
    fixture.runtime.uninstall(false).unwrap();
    assert!(!fixture.runtime.paseo_home().exists());
    cleanup(&fixture.root);
}

#[test]
fn appimage_lifecycle_report_records_real_execution_for_every_operation() {
    let root = temp_directory("report");

    let (steps, preserved) = run_linux_appimage_lifecycle(&root).unwrap();

    assert!(preserved);
    assert_eq!(
        steps
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
    assert_eq!(steps[1].outcome, "update size mismatch");
    assert_eq!(
        steps
            .iter()
            .map(|step| step.active_version.as_deref())
            .collect::<Vec<_>>(),
        [
            Some("1.0.0"),
            Some("1.0.0"),
            Some("1.1.0"),
            Some("1.0.0"),
            None
        ]
    );
    assert!(
        steps[2]
            .output
            .as_deref()
            .unwrap()
            .contains("agent-state-a")
    );
    assert_eq!(steps[2].observations["retainedVersion"], "1.0.0");
    assert_eq!(steps[3].observations["retainedVersion"], "1.1.0");
    assert_eq!(steps[4].observations["cliLinkResolves"], "false");
    cleanup(&root);
}

#[test]
fn deb_tree_carries_baseline_launcher_and_substituted_maintainer_scripts() {
    let root = temp_directory("tree");
    let tree = root.join("tree");

    create_linux_deb_tree(&tree, "1.0.0", "amd64").unwrap();

    assert_eq!(
        fs::read_to_string(tree.join("opt/Paseo/Paseo")).unwrap(),
        BASELINE_LAUNCHER
    );
    let postinst = fs::read_to_string(tree.join("DEBIAN/postinst")).unwrap();
    assert!(
        postinst.contains(
            "update-alternatives --install '/usr/bin/Paseo' 'Paseo' '/opt/Paseo/Paseo' 100"
        )
    );
    assert!(postinst.contains("chmod 4755 '/opt/Paseo/chrome-sandbox' || exit 1"));
    let postrm = fs::read_to_string(tree.join("DEBIAN/postrm")).unwrap();
    assert!(postrm.contains("update-alternatives --remove 'Paseo' '/usr/bin/Paseo'"));
    for script in [postinst, postrm] {
        assert!(!script.contains("${executable}") && !script.contains("${sanitizedProductName}"));
    }
    for executable in [
        "opt/Paseo/Paseo",
        "opt/Paseo/Paseo.bin",
        "DEBIAN/postinst",
        "DEBIAN/postrm",
    ] {
        let mode = fs::metadata(tree.join(executable))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755, "{executable}");
    }
    let control = fs::read_to_string(tree.join("DEBIAN/control")).unwrap();
    assert!(control.contains("Package: paseo\nVersion: 1.0.0\n"));
    assert!(control.contains("Architecture: amd64\n"));
    cleanup(&root);
}

#[test]
fn baseline_fixtures_are_pinned_by_digest() {
    let digests = baseline_fixture_digests();
    assert_eq!(digests.len(), 3);
    assert!(BASELINE_LAUNCHER.starts_with("#!/bin/sh\nset -eu\n"));
    assert!(BASELINE_AFTER_INSTALL.contains("chown root:root"));
    assert!(BASELINE_AFTER_REMOVE.contains("update-alternatives --remove"));
    for (name, expected) in [
        (
            "launcher.sh",
            "38cb6a3126e63405827a1836e40ea64324aa707d304c6bce2d4283fdc782c36a",
        ),
        (
            "after-install.tpl",
            "3afd8f459c65c1919b648d1cfbe618509b8fe00f01b6d15deab1c503e30d189e",
        ),
        (
            "after-remove.tpl",
            "b8d4c31b037d4888fa2a9a99226bf09d0f3ed7724075675c490a38155667e403",
        ),
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/linux")
            .join(name);
        let output = std::process::Command::new(if cfg!(target_os = "macos") {
            "shasum"
        } else {
            "sha256sum"
        })
        .args(if cfg!(target_os = "macos") {
            vec!["-a", "256"]
        } else {
            vec![]
        })
        .arg(&path)
        .output()
        .unwrap();
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .starts_with(expected),
            "{name}"
        );
    }
}

#[test]
fn deb_lane_refuses_to_run_without_disposable_root_acknowledgement() {
    let root = temp_directory("refuse");
    let config = LinuxDebLifecycleConfig {
        root: root.clone(),
        user: "nobody".into(),
        uid: 65534,
        gid: 65534,
        home: root.join("home"),
        disposable_root_acknowledged: false,
    };

    let error = run_linux_deb_lifecycle(&config).unwrap_err();

    assert!(error.to_string().contains(DISPOSABLE_ROOT_ENV));
    cleanup(&root);
}
