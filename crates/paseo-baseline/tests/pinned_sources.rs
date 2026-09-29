use std::path::PathBuf;

use paseo_baseline::{Baseline, verify_baselines};

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate directory has workspace parent")
        .parent()
        .expect("workspace parent has repository root")
        .to_path_buf()
}

#[test]
fn verifies_all_four_immutable_source_commits() {
    let root = repository_root();
    let baselines = [
        Baseline::new(
            "paseo",
            PathBuf::from("/Users/stevengonsalvez/orca/workspaces/paseo/paseo-rewrite"),
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
