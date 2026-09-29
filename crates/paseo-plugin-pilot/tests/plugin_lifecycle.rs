use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use paseo_plugin_pilot::{
    Candidate, Contribution, PluginError, PluginHost, PluginId, PluginSourceIdentity,
    ReviewedUpdate,
};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("paseo-plugin-pilot-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove test directory");
    }
}

fn git_candidate(revision: &str, contribution: &str) -> Candidate {
    Candidate::new(
        PluginSourceIdentity::Git {
            remote: "https://example.invalid/acme/review.git".into(),
            plugin_path: "plugins/review".into(),
        },
        Some(revision.into()),
        [Contribution::Surface(contribution.into())],
    )
}

#[test]
fn source_identity_contributions_settings_removal_and_restart() {
    let root = TestDir::new();
    let state = root.path().join("plugins.json");
    let mut host = PluginHost::open(&state).expect("open host");

    let directory_id = PluginId::new("directory-review").expect("valid id");
    host.install(
        directory_id.clone(),
        Candidate::new(
            PluginSourceIdentity::Directory {
                path: "/trusted/review".into(),
            },
            None,
            [Contribution::Rpc("review.start".into())],
        ),
    )
    .expect("install directory plugin");
    host.install(
        PluginId::new("git-review").expect("valid id"),
        git_candidate(&"a".repeat(40), "review-v1"),
    )
    .expect("install git plugin");
    host.install(
        PluginId::new("npm-review").expect("valid id"),
        Candidate::new(
            PluginSourceIdentity::Npm {
                package_name: "@acme/review".into(),
                plugin_path: ".".into(),
            },
            Some("1.2.3".into()),
            [Contribution::SettingsScreen("preferences".into())],
        ),
    )
    .expect("install npm plugin");
    host.write_settings(
        &directory_id,
        BTreeMap::from([("tone".into(), "terse".into())]),
    )
    .expect("write installation-scoped settings");

    let transported = host.contributions();
    assert_eq!(transported.len(), 3);
    assert!(
        transported
            .iter()
            .all(|item| !item.plugin_id.as_str().is_empty())
    );

    drop(host);
    let mut restarted = PluginHost::open(&state).expect("restart host");
    assert_eq!(
        restarted.settings(&directory_id),
        Some(&BTreeMap::from([("tone".into(), "terse".into())]))
    );
    assert_eq!(restarted.installations().len(), 3);
    restarted.remove(&directory_id).expect("remove plugin");
    assert!(restarted.settings(&directory_id).is_none());
    assert_eq!(restarted.installations().len(), 2);
}

#[test]
fn reviewed_staged_update_activates_exact_revision_and_rolls_back_failure() {
    let root = TestDir::new();
    let state = root.path().join("plugins.json");
    let mut host = PluginHost::open(&state).expect("open host");
    let id = PluginId::new("git-review").expect("valid id");
    let revision_one = "1".repeat(40);
    let revision_two = "2".repeat(40);
    let revision_three = "3".repeat(40);
    host.install(id.clone(), git_candidate(&revision_one, "review-v1"))
        .expect("install v1");

    let review = host
        .review_update(&id, revision_two.clone())
        .expect("preview exact target");
    assert_eq!(review.expected_revision, revision_one);
    assert_eq!(review.target_revision, revision_two);
    host.apply_reviewed(review, git_candidate(&"2".repeat(40), "review-v2"))
        .expect("staged candidate activates");
    assert_eq!(
        host.installation(&id).expect("installed").revision(),
        Some("2".repeat(40).as_str())
    );

    let failed_review = host
        .review_update(&id, revision_three.clone())
        .expect("review v3");
    let failed = git_candidate(&revision_three, "broken-v3").with_activation_failure();
    assert_eq!(
        host.apply_reviewed(failed_review, failed),
        Err(PluginError::ActivationFailed)
    );
    let active = host.installation(&id).expect("v2 retained");
    assert_eq!(active.revision(), Some("2".repeat(40).as_str()));
    assert_eq!(
        host.contributions_for(&id),
        vec![Contribution::Surface("review-v2".into())]
    );
}

#[test]
fn stale_review_cannot_replace_changed_installation() {
    let root = TestDir::new();
    let mut host = PluginHost::open(root.path().join("plugins.json")).expect("open host");
    let id = PluginId::new("git-review").expect("valid id");
    host.install(id.clone(), git_candidate(&"1".repeat(40), "v1"))
        .expect("install v1");
    let stale: ReviewedUpdate = host.review_update(&id, "2".repeat(40)).expect("review v2");
    host.install(id.clone(), git_candidate(&"9".repeat(40), "v9"))
        .expect("replace during review");
    assert_eq!(
        host.apply_reviewed(stale, git_candidate(&"2".repeat(40), "v2")),
        Err(PluginError::ReviewedStateChanged)
    );
}
