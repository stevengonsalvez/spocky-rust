use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use spocky_store::registry::{
    PersistedProjectRecord, PersistedWorkspaceRecord, ProjectAllocation, ProjectKind,
    ProjectRegistry, ProjectRootInput, WorkspaceKind, WorkspaceRegistry,
};

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock must be after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "spocky-store-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create disposable registry directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove disposable registry directory");
    }
}

fn directory_workspace(id: &str, project_id: &str, cwd: &str) -> PersistedWorkspaceRecord {
    PersistedWorkspaceRecord {
        workspace_id: id.to_owned(),
        project_id: project_id.to_owned(),
        cwd: cwd.to_owned(),
        kind: WorkspaceKind::Directory,
        display_name: "project".to_owned(),
        title: None,
        branch: None,
        worktree_root: None,
        base_branch: None,
        is_paseo_owned_worktree: false,
        main_repo_root: None,
        created_at: "2026-10-01T10:00:00.000Z".to_owned(),
        updated_at: "2026-10-01T10:00:00.000Z".to_owned(),
        archived_at: None,
        auto_archived_change_request_url: None,
        pinned_at: None,
        labels: None,
        untrusted_source: None,
    }
}

/// `JSON.stringify([createPersistedWorkspaceRecord(...)], null, 2)` from the
/// pinned schema: every nullish key present as `null`, `labels` and
/// `untrustedSource` absent, two-space indent, no trailing newline.
const DIRECTORY_WORKSPACE_FILE: &str = r#"[
  {
    "workspaceId": "wks_0123456789abcdef",
    "projectId": "prj_0123456789abcdef",
    "cwd": "/tmp/project",
    "kind": "directory",
    "displayName": "project",
    "title": null,
    "branch": null,
    "worktreeRoot": null,
    "baseBranch": null,
    "isPaseoOwnedWorktree": false,
    "mainRepoRoot": null,
    "createdAt": "2026-10-01T10:00:00.000Z",
    "updatedAt": "2026-10-01T10:00:00.000Z",
    "archivedAt": null,
    "autoArchivedChangeRequestUrl": null,
    "pinnedAt": null
  }
]"#;

#[test]
fn workspace_file_matches_json_stringify_layout() {
    let home = TestDir::new("registry-layout");
    let path = home.path().join("projects").join("workspaces.json");
    let mut registry = WorkspaceRegistry::new(&path);
    assert!(!registry.exists_on_disk());
    assert!(registry.initialize().is_none());
    assert!(!registry.exists_on_disk(), "loading never creates the file");

    registry
        .upsert(directory_workspace(
            "wks_0123456789abcdef",
            "prj_0123456789abcdef",
            "/tmp/project",
        ))
        .expect("upsert writes the registry");

    assert_eq!(
        fs::read_to_string(&path).expect("read registry file"),
        DIRECTORY_WORKSPACE_FILE
    );
    let names = fs::read_dir(path.parent().expect("registry parent"))
        .expect("list registry directory")
        .map(|entry| entry.expect("entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["workspaces.json"], "temp file renamed away");
}

#[test]
fn load_applies_zod_output_order_defaults_and_strips_unknown_keys() {
    let home = TestDir::new("registry-zod");
    let path = home.path().join("workspaces.json");
    fs::write(
        &path,
        r#"[{"pinnedAt":"2026-10-01T11:00:00.000Z","extra":1,"archivedAt":null,
            "updatedAt":"2026-10-01T10:00:00.000Z","createdAt":"2026-10-01T10:00:00.000Z",
            "displayName":"project","kind":"directory","cwd":"/tmp/project",
            "projectId":"prj_0123456789abcdef","workspaceId":"wks_0123456789abcdef",
            "labels":["a"],"untrustedSource":{"headRepository":"o/r","number":7.0,
            "forge":"github","kind":"change_request","junk":true}}]"#,
    )
    .expect("seed registry file");
    let mut registry = WorkspaceRegistry::new(&path);
    assert!(registry.initialize().is_none());
    registry
        .update("wks_0123456789abcdef", Clone::clone)
        .expect("update rewrites the file")
        .expect("record exists");

    let expected = r#"[
  {
    "workspaceId": "wks_0123456789abcdef",
    "projectId": "prj_0123456789abcdef",
    "cwd": "/tmp/project",
    "kind": "directory",
    "displayName": "project",
    "title": null,
    "branch": null,
    "worktreeRoot": null,
    "baseBranch": null,
    "isPaseoOwnedWorktree": false,
    "mainRepoRoot": null,
    "createdAt": "2026-10-01T10:00:00.000Z",
    "updatedAt": "2026-10-01T10:00:00.000Z",
    "archivedAt": null,
    "autoArchivedChangeRequestUrl": null,
    "pinnedAt": "2026-10-01T11:00:00.000Z",
    "labels": [
      "a"
    ],
    "untrustedSource": {
      "kind": "change_request",
      "forge": "github",
      "number": 7,
      "headRepository": "o/r"
    }
  }
]"#;
    assert_eq!(fs::read_to_string(&path).expect("read registry"), expected);
}

#[test]
fn invalid_file_loads_empty_and_next_write_replaces_it() {
    let home = TestDir::new("registry-invalid");
    let path = home.path().join("workspaces.json");
    // `archivedAt` is `.nullable()` without a default, so a missing key fails
    // the whole array exactly as `z.array(schema).parse` does.
    fs::write(
        &path,
        r#"[{"workspaceId":"wks_a","projectId":"p","cwd":"/a","kind":"directory",
            "displayName":"a","createdAt":"t","updatedAt":"t"}]"#,
    )
    .expect("seed invalid registry");
    let mut registry = WorkspaceRegistry::new(&path);
    assert!(registry.initialize().is_some(), "load failure is reported");
    assert!(registry.list().is_empty());

    registry
        .upsert(directory_workspace(
            "wks_0123456789abcdef",
            "prj_0123456789abcdef",
            "/tmp/project",
        ))
        .expect("upsert after failed load");
    assert_eq!(
        fs::read_to_string(&path).expect("read registry"),
        DIRECTORY_WORKSPACE_FILE
    );
}

#[test]
fn map_semantics_keep_position_on_replace_and_remove_in_order() {
    let home = TestDir::new("registry-order");
    let path = home.path().join("workspaces.json");
    let mut registry = WorkspaceRegistry::new(&path);
    for id in ["wks_a", "wks_b", "wks_c"] {
        registry
            .upsert(directory_workspace(id, "prj", "/tmp/x"))
            .expect("upsert");
    }
    let mut renamed = directory_workspace("wks_a", "prj", "/tmp/x");
    renamed.title = Some("renamed".to_owned());
    registry.upsert(renamed).expect("replace keeps position");
    registry
        .remove_if_present("wks_b")
        .expect("remove")
        .expect("present");
    assert!(
        registry
            .remove_if_present("wks_b")
            .expect("remove")
            .is_none()
    );

    let mut reloaded = WorkspaceRegistry::new(&path);
    let ids = reloaded
        .list()
        .into_iter()
        .map(|record| (record.workspace_id, record.title))
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![
            ("wks_a".to_owned(), Some("renamed".to_owned())),
            ("wks_c".to_owned(), None),
        ]
    );
}

#[test]
fn archive_if_active_only_touches_unarchived_records() {
    let home = TestDir::new("registry-archive");
    let mut registry = WorkspaceRegistry::new(home.path().join("workspaces.json"));
    registry
        .upsert(directory_workspace("wks_a", "prj", "/tmp/x"))
        .expect("upsert");
    let archived = registry
        .archive_if_active("wks_a", "2026-10-02T00:00:00.000Z")
        .expect("archive")
        .expect("was active");
    assert_eq!(
        archived.archived_at.as_deref(),
        Some("2026-10-02T00:00:00.000Z")
    );
    assert_eq!(archived.updated_at, "2026-10-02T00:00:00.000Z");
    assert!(
        registry
            .archive_if_active("wks_a", "2026-10-03T00:00:00.000Z")
            .expect("archive")
            .is_none()
    );
}

#[test]
fn project_allocation_reuses_oldest_equivalent_root_and_refreshes_kind() {
    let home = TestDir::new("registry-projects");
    let path = home.path().join("projects.json");
    let mut registry = ProjectRegistry::new(&path);
    let mut ids = ["prj_b", "prj_a", "prj_c"].into_iter().map(str::to_owned);
    let first = registry
        .get_or_create_active_by_root(
            &ProjectRootInput {
                root_path: "/tmp/project",
                kind: ProjectKind::NonGit,
                display_name: "project",
                project_key: Some("host:srv:/tmp/project"),
                timestamp: "2026-10-01T10:00:00.000Z",
            },
            || ids.next().expect("id"),
        )
        .expect("create project");
    assert!(matches!(first, ProjectAllocation::Created(_)));
    assert_eq!(first.record().project_id, "prj_b");

    let same = registry
        .get_or_create_active_by_root(
            &ProjectRootInput {
                root_path: "/tmp/project/",
                kind: ProjectKind::NonGit,
                display_name: "ignored",
                project_key: Some("host:srv:/tmp/project"),
                timestamp: "2026-10-01T11:00:00.000Z",
            },
            || unreachable!("existing project is reused"),
        )
        .expect("reuse project");
    assert_eq!(same, ProjectAllocation::Existing(first.record().clone()));

    let refreshed = registry
        .get_or_create_active_by_root(
            &ProjectRootInput {
                root_path: "/tmp/./project",
                kind: ProjectKind::Git,
                display_name: "ignored",
                project_key: None,
                timestamp: "2026-10-01T12:00:00.000Z",
            },
            || unreachable!("existing project is refreshed"),
        )
        .expect("refresh project");
    let ProjectAllocation::Refreshed(record) = refreshed else {
        panic!("kind change refreshes the project");
    };
    assert_eq!(record.kind, ProjectKind::Git);
    assert_eq!(record.project_key, None);
    assert_eq!(record.updated_at, "2026-10-01T12:00:00.000Z");
    assert_eq!(record.created_at, "2026-10-01T10:00:00.000Z");

    assert_eq!(
        fs::read_to_string(&path).expect("read projects"),
        r#"[
  {
    "projectId": "prj_b",
    "rootPath": "/tmp/project",
    "kind": "git",
    "displayName": "project",
    "projectKey": null,
    "customName": null,
    "customIconRevision": null,
    "createdAt": "2026-10-01T10:00:00.000Z",
    "updatedAt": "2026-10-01T12:00:00.000Z",
    "archivedAt": null
  }
]"#
    );
}

#[test]
fn project_allocation_skips_taken_ids_and_archived_roots() {
    let home = TestDir::new("registry-project-ids");
    let mut registry = ProjectRegistry::new(home.path().join("projects.json"));
    registry
        .upsert(PersistedProjectRecord {
            project_id: "prj_taken".to_owned(),
            root_path: "/tmp/project".to_owned(),
            kind: ProjectKind::NonGit,
            display_name: "project".to_owned(),
            project_key: None,
            custom_name: None,
            custom_icon_revision: None,
            created_at: "2026-10-01T09:00:00.000Z".to_owned(),
            updated_at: "2026-10-01T09:00:00.000Z".to_owned(),
            archived_at: Some("2026-10-01T09:30:00.000Z".to_owned()),
        })
        .expect("seed archived project");
    let mut ids = ["prj_taken", "prj_fresh"].into_iter().map(str::to_owned);
    let created = registry
        .get_or_create_active_by_root(
            &ProjectRootInput {
                root_path: "/tmp/project",
                kind: ProjectKind::NonGit,
                display_name: "project",
                project_key: None,
                timestamp: "2026-10-01T10:00:00.000Z",
            },
            || ids.next().expect("id"),
        )
        .expect("create project");
    assert_eq!(created.record().project_id, "prj_fresh");
    assert_eq!(registry.list().len(), 2);
}
