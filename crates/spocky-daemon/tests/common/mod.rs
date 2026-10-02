//! Shared by the process and lifecycle tests: nothing started by a test may
//! listen on the production daemon ports.

/// Ports of the production daemon.
const PRODUCTION_PORTS: [u16; 2] = [6767, 6768];

/// Panics when `listen` (`host:port`, a bare port, or a path) names a
/// production port. Call it before any launch.
#[allow(dead_code)]
pub fn assert_disposable_listen(listen: &str) {
    let port = listen.rsplit(':').next().unwrap_or(listen);
    if let Ok(port) = port.trim().parse::<u16>() {
        assert!(
            !PRODUCTION_PORTS.contains(&port),
            "refusing to start a test daemon on production port {port} ({listen})"
        );
    }
}

/// The value of `NAME=value` in `scripts/phase3/pins.sh`.
#[allow(dead_code)]
fn pin(name: &str) -> String {
    include_str!("../../../../scripts/phase3/pins.sh")
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
        .unwrap_or_else(|| panic!("{name} is not in pins.sh"))
        .trim_matches('"')
        .to_owned()
}

/// A fixture captured from the pinned original must say so: its provenance names
/// the pinned Node version and binary digest, and a build marker with the pinned
/// Paseo commit, lock digest and Node digest. A fixture left over from another
/// build, or from another Node, fails here.
#[allow(dead_code)]
pub fn assert_fixture_provenance(fixture: &serde_json::Value) {
    let provenance = &fixture["provenance"];
    assert_eq!(
        provenance["node"].as_str(),
        Some(format!("v{}", pin("P3_NODE_VERSION")).as_str()),
        "Node version"
    );
    assert_eq!(
        provenance["nodeSha256"].as_str(),
        Some(pin("P3_NODE_BINARY_SHA256").as_str()),
        "Node binary digest"
    );
    let marker: Vec<&str> = provenance["buildMarker"]
        .as_str()
        .unwrap()
        .lines()
        .collect();
    for (key, name) in [
        ("commit", "P3_PASEO_COMMIT"),
        ("lock", "P3_PASEO_LOCK_SHA256"),
        ("node", "P3_NODE_BINARY_SHA256"),
    ] {
        assert!(
            marker.contains(&format!("{key}={}", pin(name)).as_str()),
            "build marker {key}"
        );
    }
    assert!(
        provenance["dist"]
            .as_object()
            .is_some_and(|dist| !dist.is_empty()),
        "dist digests"
    );
}
