use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use spocky_store::StoreError;
use spocky_store::js_value::{JsValue, parse};
use spocky_store::registry::{
    PersistedProjectRecord, PersistedWorkspaceRecord, ProjectAllocation, ProjectKind,
    ProjectRegistry, ProjectRootInput, UntrustedWorkspaceSource, WorkspaceKind, WorkspaceRegistry,
    parse_registry_file, render_registry_file,
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

/// Each case in `tests/fixtures/registry-zod.json` was produced by
/// `tests/oracle/zod-oracle.sh tests/oracle/registry-cases.json`: node runs
/// `JSON.stringify(z.array(schema).parse(JSON.parse(input)), null, 2)` with
/// the pinned zod and the schema text copied from `workspace-registry.ts`.
/// `output` is null where the baseline load fails.
#[test]
fn registry_parse_and_render_match_zod_oracle() {
    let fixture_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/registry-zod.json");
    let fixture = parse(&fs::read_to_string(fixture_path).expect("read oracle fixture"))
        .expect("fixture is JSON");
    let cases = fixture.as_array().expect("fixture is an array of cases");
    assert_eq!(cases.len(), 13, "every oracle case is checked");
    for case in cases {
        let case = case.as_object().expect("case object");
        let field = |key: &str| {
            case.get(key)
                .and_then(JsValue::as_str)
                .expect("string field")
        };
        let (name, kind, input) = (field("name"), field("kind"), field("input"));
        // The fixture string is JavaScript text; `JSON.stringify` output never
        // holds a lone surrogate, so only a literal U+10FFFF needs decoding.
        let expected = case
            .get("output")
            .and_then(JsValue::as_str)
            .map(|text| text.replace("\u{10FFFF}\u{10FFFF}", "\u{10FFFF}"));
        let actual = match kind {
            "workspace" => parse_registry_file::<PersistedWorkspaceRecord>(input)
                .ok()
                .map(|records| render_registry_file(&records)),
            "project" => parse_registry_file::<PersistedProjectRecord>(input)
                .ok()
                .map(|records| render_registry_file(&records)),
            other => panic!("unknown case kind {other}"),
        };
        assert_eq!(actual, expected, "case {name}");
    }
}

#[test]
fn javascript_only_input_survives_load_and_next_write() {
    let home = TestDir::new("registry-js-input");
    let path = home.path().join("workspaces.json");
    // `JSON.parse` accepts a lone surrogate, an out-of-range number, and
    // nesting beyond 128 levels; the baseline keeps the record and drops the
    // unknown keys on the next write.
    let deep = format!("{}{}", "[".repeat(300), "]".repeat(300));
    fs::write(
        &path,
        format!(
            r#"[{{"workspaceId":"wks_a","projectId":"prj","cwd":"/tmp/x","kind":"directory","displayName":"x","title":"split \ud83d","createdAt":"2026-10-01T10:00:00.000Z","updatedAt":"2026-10-01T10:00:00.000Z","archivedAt":null,"huge":1e400,"nested":{deep}}}]"#
        ),
    )
    .expect("seed registry written by a newer daemon");
    let mut registry = WorkspaceRegistry::new(&path);
    assert!(
        registry.initialize().is_none(),
        "JSON.parse accepts the file"
    );
    registry
        .upsert(directory_workspace("wks_b", "prj", "/tmp/y"))
        .expect("upsert keeps existing records");
    let written = fs::read_to_string(&path).expect("read registry");
    assert!(written.contains(r#""title": "split \ud83d","#), "{written}");
    assert_eq!(registry.list().len(), 2);
    assert!(!written.contains("huge"));
}

#[test]
fn load_failure_is_kept_for_logging() {
    let home = TestDir::new("registry-load-failure");
    let path = home.path().join("projects.json");
    fs::write(&path, "{not json").expect("seed corrupt registry");
    let mut registry = ProjectRegistry::new(&path);
    assert!(registry.list().is_empty());
    assert!(
        matches!(registry.load_failure(), Some(StoreError::JsonSyntax(_))),
        "an implicit load through list() keeps the failure"
    );
    assert!(registry.initialize().is_some());
}

/// Known baseline defect, kept for exact parity (coordinator decision,
/// 2026-10-01): pinned Paseo `workspace-registry.ts:285-355` logs a failed
/// load, marks the registry loaded with an empty cache, and lets the next
/// mutation write. A corrupt or schema-invalid registry file therefore loses
/// every record on the next write. Files `JSON.parse` accepts do not reach
/// this path.
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

#[test]
fn update_keeps_the_stored_key_when_the_updater_changes_the_id() {
    let home = TestDir::new("registry-rekey");
    let path = home.path().join("workspaces.json");
    let mut registry = WorkspaceRegistry::new(&path);
    for id in ["wks_a", "wks_b"] {
        registry
            .upsert(directory_workspace(id, "prj", "/tmp/x"))
            .expect("upsert");
    }
    let renamed = registry
        .update("wks_a", |existing| PersistedWorkspaceRecord {
            workspace_id: "wks_z".to_owned(),
            ..existing.clone()
        })
        .expect("update")
        .expect("present");
    assert_eq!(renamed.workspace_id, "wks_z");
    // `records.set(id, next)`: same key, same position, new record id.
    assert_eq!(
        registry.get("wks_a").map(|record| record.workspace_id),
        Some("wks_z".to_owned())
    );
    assert!(registry.get("wks_z").is_none());
    let ids = WorkspaceRegistry::new(&path)
        .list()
        .into_iter()
        .map(|record| record.workspace_id)
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["wks_z", "wks_b"]);
}

#[test]
fn workspace_archive_restamps_and_records_change_request_url() {
    let home = TestDir::new("registry-workspace-archive");
    let mut registry = WorkspaceRegistry::new(home.path().join("workspaces.json"));
    registry
        .upsert(directory_workspace("wks_a", "prj", "/tmp/x"))
        .expect("upsert");
    let first = registry
        .archive("wks_a", "2026-10-02T00:00:00.000Z", Some(""))
        .expect("archive")
        .expect("present");
    assert_eq!(
        first.auto_archived_change_request_url, None,
        "empty URL is falsy"
    );
    let second = registry
        .archive(
            "wks_a",
            "2026-10-03T00:00:00.000Z",
            Some("https://github.com/o/r/pull/7"),
        )
        .expect("archive again")
        .expect("present");
    assert_eq!(
        second.archived_at.as_deref(),
        Some("2026-10-03T00:00:00.000Z")
    );
    assert_eq!(second.updated_at, "2026-10-03T00:00:00.000Z");
    assert_eq!(
        second.auto_archived_change_request_url.as_deref(),
        Some("https://github.com/o/r/pull/7")
    );
    assert!(
        registry
            .archive("wks_missing", "2026-10-03T00:00:00.000Z", None)
            .expect("archive unknown")
            .is_none()
    );
}

#[test]
fn project_archive_only_archives_active_projects() {
    let home = TestDir::new("registry-project-archive");
    let mut registry = ProjectRegistry::new(home.path().join("projects.json"));
    registry
        .get_or_create_active_by_root(
            &ProjectRootInput {
                root_path: "/tmp/project",
                kind: ProjectKind::NonGit,
                display_name: "project",
                project_key: None,
                timestamp: "2026-10-01T10:00:00.000Z",
            },
            || "prj_a".to_owned(),
        )
        .expect("create");
    let archived = registry
        .archive("prj_a", "2026-10-02T00:00:00.000Z")
        .expect("archive")
        .expect("was active");
    assert_eq!(
        archived.archived_at.as_deref(),
        Some("2026-10-02T00:00:00.000Z")
    );
    assert!(
        registry
            .archive("prj_a", "2026-10-03T00:00:00.000Z")
            .expect("archive again")
            .is_none()
    );
}

#[test]
fn equal_created_at_ties_break_by_locale_compare() {
    let home = TestDir::new("registry-collation");
    let mut registry = ProjectRegistry::new(home.path().join("projects.json"));
    for id in ["prj_B", "prj_a"] {
        registry
            .upsert(PersistedProjectRecord {
                project_id: id.to_owned(),
                root_path: "/tmp/project".to_owned(),
                kind: ProjectKind::NonGit,
                display_name: "project".to_owned(),
                project_key: None,
                custom_name: None,
                custom_icon_revision: None,
                created_at: "2026-10-01T10:00:00.000Z".to_owned(),
                updated_at: "2026-10-01T10:00:00.000Z".to_owned(),
                archived_at: None,
            })
            .expect("seed project");
    }
    let chosen = registry
        .get_or_create_active_by_root(
            &ProjectRootInput {
                root_path: "/tmp/project",
                kind: ProjectKind::NonGit,
                display_name: "project",
                project_key: None,
                timestamp: "2026-10-01T11:00:00.000Z",
            },
            || unreachable!("an active project exists"),
        )
        .expect("allocate");
    // Byte order puts "prj_B" first; `"prj_a".localeCompare("prj_B")` is -1.
    assert_eq!(chosen.record().project_id, "prj_a");
}

#[test]
fn writes_reject_records_the_schema_rejects() {
    let home = TestDir::new("registry-schema-parse");
    let path = home.path().join("workspaces.json");
    let mut registry = WorkspaceRegistry::new(&path);
    registry
        .upsert(directory_workspace("wks_a", "prj", "/tmp/x"))
        .expect("valid upsert");
    let before = fs::read_to_string(&path).expect("read registry");
    let mut invalid = directory_workspace("wks_b", "prj", "/tmp/y");
    invalid.untrusted_source = Some(UntrustedWorkspaceSource {
        forge: "github".to_owned(),
        number: 0,
        head_repository: "o/r".to_owned(),
    });
    assert!(matches!(
        registry.upsert(invalid.clone()),
        Err(StoreError::InvalidRecord(_))
    ));
    assert!(matches!(
        registry.update("wks_a", |_| invalid.clone()),
        Err(StoreError::InvalidRecord(_))
    ));
    assert_eq!(fs::read_to_string(&path).expect("read registry"), before);
    assert_eq!(registry.list().len(), 1);
}
