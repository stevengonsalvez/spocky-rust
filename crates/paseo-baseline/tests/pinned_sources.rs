use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use paseo_baseline::{Baseline, BaselineError, verify_baselines};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory has workspace parent")
        .parent()
        .expect("workspace parent has repository root")
        .to_path_buf()
}

fn reference_root() -> PathBuf {
    std::env::var_os("PASEO_REFERENCE_ROOT").map_or_else(
        || {
            repository_root()
                .parent()
                .expect("Rust repository has workspace parent")
                .join("paseo-rewrite")
        },
        PathBuf::from,
    )
}

#[test]
fn verifies_all_four_immutable_source_commits() {
    let root = repository_root();
    let baselines = [
        Baseline::new(
            "paseo",
            reference_root(),
            "5de45e208690b0efc51c59a585ae9729325a9204",
        ),
        Baseline::new(
            "hub",
            root.join(".baselines/hub"),
            "28f6c78833065fd282f9064f92a9aa61875dd359",
        ),
        Baseline::new(
            "relay",
            root.join(".baselines/relay"),
            "3fc41c96c8c63f3a7109e832899cc57d473c4531",
        ),
        Baseline::new(
            "import",
            root.join(".baselines/import"),
            "8b3eb738fa737010da86e8ac01d3a34cc9a7a3c5",
        ),
    ];

    let verified = verify_baselines(&baselines).expect("pinned sources verify");

    assert_eq!(verified.len(), 4);
    assert!(verified.iter().all(|entry| entry.actual == entry.expected));
}

#[test]
fn rejects_a_baseline_with_modified_tracked_content() {
    let source = repository_root().join(".baselines/hub");
    let checkout = TemporaryDirectory::new("paseo-baseline-dirty");
    let clone = Command::new("git")
        .args(["clone", "--shared", "--quiet"])
        .arg(&source)
        .arg(checkout.path())
        .status()
        .expect("git clone executes");
    assert!(clone.success(), "fixture clone succeeds");

    let package_json = checkout.path().join("package.json");
    let mut contents = fs::read_to_string(&package_json).expect("fixture package is readable");
    contents.push('\n');
    fs::write(&package_json, contents).expect("fixture package can be modified");

    let result = verify_baselines(&[Baseline::new(
        "hub",
        checkout.path().to_path_buf(),
        "28f6c78833065fd282f9064f92a9aa61875dd359",
    )]);

    assert!(matches!(
        result,
        Err(BaselineError::TrackedChanges { name, .. }) if name == "hub"
    ));
}

struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    fn new(prefix: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock follows Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()));
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        if self.path.exists() {
            fs::remove_dir_all(&self.path).expect("temporary fixture can be removed");
        }
    }
}
