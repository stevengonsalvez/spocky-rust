use paseo_ui_renderer_pilot::{APP_TITLE, render_shell_html};

#[test]
fn shell_renders_deterministic_semantic_html() {
    let first = render_shell_html();
    let second = render_shell_html();

    assert_eq!(first, second);
    assert!(first.contains("<main"));
    assert!(first.contains("<nav"));
    assert!(first.contains("aria-label=\"Primary navigation\""));
    assert!(first.contains("<button"));
    assert!(first.contains("role=\"status\""));
    assert!(first.contains("aria-live=\"polite\""));
    assert!(first.contains(APP_TITLE));
    assert!(first.contains("New workspace"));
    assert!(first.contains("Add a project"));
    assert!(first.contains("Import session"));
    assert!(first.contains("Setup providers"));
    assert!(first.contains("Community"));
    assert!(first.contains("No projects yet"));
    assert!(first.contains("Add a project to get started"));
    assert!(first.contains("<svg"));
    assert!(first.contains("viewBox=\"0 0 700 700\""));
    assert!(first.contains(".mark svg { transform: translateY(-2px); }"));
    assert!(first.contains(".actions { transform: translateY(-4px); }"));
    assert!(first.contains("1.95 1.5H4"));
    assert!(first.contains("0 0 0 1.67.9H18"));
    assert!(first.contains("M18 8v5a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V8Z"));
    assert!(!first.contains("1.95 1.5H5"));
    assert!(!first.contains("0 0 0 1.67.9H19"));
    assert!(!first.contains("M18 8v5a6 6"));
    assert!(!first.contains("icon-folder::before"));
    assert!(first.contains("https://github.com/sponsors/boudra"));
    assert!(first.contains("https://discord.gg/jz8T2uahpH"));
    assert!(first.contains("viewBox=\"0 -0.5 25 25\""));
    assert!(first.contains("M2 9.5a5.5 5.5"));
    assert!(first.contains("M20.317 4.3698"));
    assert!(!first.contains("icon-star::before"));
    assert!(!first.contains("icon-sponsor::before"));
    assert!(!first.contains("icon-community::before"));
    assert!(first.contains("M3 12a9 9 0 1 0"));
    assert!(first.contains("M16 14v2.2l1.6 1"));
    assert!(first.contains("M20 20a2 2 0 0 0 2-2V8"));
    assert!(first.contains("M9.09 9a3 3 0 0 1 5.83 1"));
    assert!(!first.contains("icon-history::before"));
    assert!(!first.contains("icon-project::before"));
    assert!(!first.contains("icon-settings::before"));
    assert!(first.contains(".action-detail { display: block; color: #71717a;"));
    assert!(first.contains("gap: 0; color: #71717a; font-size: 14px;"));
    assert!(!first.contains("#777983"));
    assert!(!first.contains("#72747d"));
    assert!(first.contains("mobile-menu-line short"));
    assert!(first.contains("width: 16px; height: 12px;"));
    assert!(!first.contains("content: \"☰\""));
    assert!(first.contains(".action-title { display: block; margin-bottom: 4px; font-size: 14px; line-height: normal; }"));
    assert!(first.contains(
        "font-family: system-ui, -apple-system, \"system-ui\", \"Segoe UI\", Roboto, Helvetica, Arial, sans-serif;"
    ));
    assert!(!first.contains("font-family: Inter"));
    assert!(first.contains(".mark { width: 52px; height: 52px; margin: 0 auto 73.5px; }"));
    assert!(first.contains(".community { bottom: 72px; }"));
}

#[test]
fn feasibility_report_has_explicit_evidence_states() {
    let report: serde_json::Value = serde_json::from_str(include_str!("../feasibility.json"))
        .expect("feasibility report must be valid JSON");

    assert_eq!(report["schemaVersion"], 1);
    assert_eq!(report["framework"]["name"], "Dioxus");
    assert_eq!(report["framework"]["version"], "0.7.0");

    let evidence = report["evidence"]
        .as_object()
        .expect("evidence must be an object");
    for required in [
        "hostTests",
        "webCompile",
        "desktopCompile",
        "iosCompile",
        "androidCompile",
        "launch",
        "visual",
        "accessibility",
        "native",
        "packaging",
    ] {
        let entry = evidence
            .get(required)
            .unwrap_or_else(|| panic!("missing evidence category: {required}"));
        let state = entry["state"]
            .as_str()
            .unwrap_or_else(|| panic!("missing state for {required}"));
        assert!(
            matches!(
                state,
                "passed" | "partial" | "failed" | "blocked" | "unsupported" | "unproven"
            ),
            "invalid evidence state for {required}: {state}"
        );
        assert!(
            entry["detail"]
                .as_str()
                .is_some_and(|detail| !detail.is_empty()),
            "missing detail for {required}"
        );
    }
}
