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
