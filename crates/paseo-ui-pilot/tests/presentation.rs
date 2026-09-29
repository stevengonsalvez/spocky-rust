use paseo_platform_bridge::{HostCapability, HostDescriptor, HostKind};
use paseo_ui_pilot::{
    AccessibilityRole, AgentPresentation, InputAction, InputSource, Locale, PresentationState,
    Route, Theme, UiIntent, Viewport, VisualEnvironment, WorkspacePresentation,
};

fn state() -> PresentationState {
    PresentationState::new(
        vec![
            WorkspacePresentation::new("w1", "Paseo"),
            WorkspacePresentation::new("w2", "Hub"),
        ],
        vec![
            AgentPresentation::new("a1", "w1", "Implementer", "running"),
            AgentPresentation::new("a2", "w1", "Reviewer", "waiting"),
        ],
    )
}

#[test]
fn routes_workspace_and_agent_state_through_keyboard_and_touch() {
    let host = HostDescriptor::new(
        HostKind::Browser,
        [HostCapability::KeyboardInput, HostCapability::TouchInput],
    );
    let mut presentation = state();

    presentation
        .dispatch(
            &host,
            InputAction::new(InputSource::Keyboard, UiIntent::OpenWorkspace("w1".into())),
        )
        .expect("keyboard opens workspace");
    assert_eq!(
        presentation.route,
        Route::Workspace {
            workspace_id: "w1".into()
        }
    );

    presentation
        .dispatch(
            &host,
            InputAction::new(InputSource::Touch, UiIntent::OpenAgent("a2".into())),
        )
        .expect("touch opens agent");
    assert_eq!(
        presentation.route,
        Route::Agent {
            workspace_id: "w1".into(),
            agent_id: "a2".into(),
        }
    );
}

#[test]
fn unsupported_input_fails_visibly_without_fallback() {
    let host = HostDescriptor::new(HostKind::Browser, [HostCapability::KeyboardInput]);
    let mut presentation = state();

    let error = presentation
        .dispatch(
            &host,
            InputAction::new(InputSource::Touch, UiIntent::OpenWorkspace("w1".into())),
        )
        .expect_err("missing touch capability fails");

    assert_eq!(presentation.route, Route::Workspaces);
    assert_eq!(error.code, "HOST_CAPABILITY_UNSUPPORTED");
    assert_eq!(
        presentation
            .visible_error
            .expect("error is visible")
            .message,
        "Touch input is unavailable on this host."
    );
}

#[test]
fn accessibility_tree_has_exact_roles_names_and_focus_order() {
    let presentation = state();
    let tree = presentation.accessibility_tree();

    assert_eq!(tree.nodes.len(), 3);
    assert_eq!(tree.nodes[0].role, AccessibilityRole::Navigation);
    assert_eq!(tree.nodes[0].name, "Workspaces");
    assert_eq!(tree.nodes[0].focus_order, None);
    assert_eq!(tree.nodes[1].role, AccessibilityRole::Button);
    assert_eq!(tree.nodes[1].name, "Paseo");
    assert_eq!(tree.nodes[1].focus_order, Some(1));
    assert_eq!(tree.nodes[2].name, "Hub");
    assert_eq!(tree.nodes[2].focus_order, Some(2));
}

#[test]
fn visual_snapshot_includes_fixed_environment_and_ordered_content() {
    let presentation = state();
    let environment = VisualEnvironment {
        viewport: Viewport {
            width: 390,
            height: 844,
            scale_milli: 3000,
        },
        theme: Theme::Dark,
        locale: Locale::new("en-GB"),
        reduced_motion: true,
    };

    let first = presentation
        .snapshot(environment.clone())
        .expect("snapshot serializes");
    let second = presentation
        .snapshot(environment)
        .expect("snapshot repeats");

    assert_eq!(first, second);
    assert_eq!(
        first,
        br#"{"viewport":{"width":390,"height":844,"scaleMilli":3000},"theme":"dark","locale":"en-GB","reducedMotion":true,"route":{"kind":"workspaces"},"content":[{"kind":"workspace","id":"w1","name":"Paseo"},{"kind":"workspace","id":"w2","name":"Hub"}],"visibleError":null}"#
    );
}
