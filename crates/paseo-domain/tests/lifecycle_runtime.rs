use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock must follow Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "paseo-lifecycle-runtime-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create disposable runtime directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove disposable runtime directory");
    }
}

#[test]
fn driver_covers_complete_lifecycle_with_measured_counts() {
    let state = TestDir::new();
    let status = Command::new(env!("CARGO_BIN_EXE_paseo-lifecycle-driver"))
        .env("PASEO_DIFFERENTIAL_STATE", state.path())
        .status()
        .expect("run lifecycle driver");
    assert!(status.success());

    let structured: Value = serde_json::from_slice(
        &fs::read(state.path().join("output/structured.json")).expect("read structured output"),
    )
    .expect("structured output is JSON");
    let phases = structured
        .as_array()
        .expect("structured output is a phase array");
    let phase_names = phases
        .iter()
        .map(|phase| phase["phase"].as_str().expect("phase has a name"))
        .collect::<Vec<_>>();
    assert_eq!(
        phase_names,
        [
            "create",
            "stream",
            "permission",
            "cancel",
            "restart",
            "resume",
            "archive",
            "recovery",
        ]
    );

    let counts: Value = serde_json::from_slice(
        &fs::read(state.path().join("output/counts.json")).expect("read measured counts"),
    )
    .expect("counts output is JSON");
    assert_eq!(counts["fixtures"], phases.len());
    assert_eq!(
        counts["assertions"],
        phases
            .iter()
            .map(|phase| phase.as_object().expect("phase is an object").len() - 1)
            .sum::<usize>()
    );
    assert!(
        state
            .path()
            .join("agents/00000000-0000-4000-8000-000000000901.json")
            .is_file(),
        "driver must persist a reloadable lifecycle record"
    );
}
