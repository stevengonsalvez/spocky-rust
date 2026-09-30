use std::time::Duration;

use paseo_plugin_pilot::{ClientContribution, CompiledPluginClient, PluginError};
use serde_json::json;

const FULL_CLIENT_BUNDLE: &str = r##"(function(require) {
  const module = { exports: {} };
  module.exports.default = function contribute(plugin) {
    function Component() { return null; }
    const removals = [];
    removals.push(plugin.addSettingsScreen({ id: "display", title: "Display", icon: "Settings", Component }));
    removals.push(plugin.addSurface("main", Component));
    removals.push(plugin.addSidebarItem({ id: "main", title: "Main", icon: "Blocks", surface: "main" }));
    removals.push(plugin.addWorkspacePanel({ id: "workspace", title: "Workspace", icon: "PanelLeft", context: "workspace", locations: ["workspace", "explorer"], Component }));
    removals.push(plugin.addCommandCenterItem({ id: "global-command", title: "Global", icon: "Command", context: "global", onSelect() { return require("@getpaseo/plugin/client").getPaseoClient("host-a").connectionGeneration; } }));
    removals.push(plugin.addSlashCommand({ name: "review", description: "Review", argumentHint: "[path]", context: "workspace", onSubmit(context) { return context.args.toUpperCase(); } }));
    removals.push(plugin.addAttachmentSource({ id: "issues", title: "Issues", icon: "Paperclip", pickerTitle: "Pick issue", searchPlaceholder: "Search", search: { name: "issues.search" } }));
    removals.push(plugin.addTheme({ id: "night", name: "Night", appearance: "dark", colors: { background: "#000", foreground: "#fff", raised: "#111", control: "#222", border: "#333", mutedForeground: "#aaa", ring: "#44f" } }));
    removals.push(plugin.addTimelineTransformer({ id: "assistant", query: { itemType: "assistant_message" }, transform(input) { return { items: [{ type: "plugin", kind: "summary", version: 1, data: { phase: input.phase } }] }; } }));
    removals.push(plugin.addTimelineRenderer({ kind: "summary", version: 1, schema: { safeParse(value) { return { success: true, data: value }; } }, Component }));
    const header = plugin.addHeaderButton({ id: "refresh", workspaceId: "workspace-1", button: { title: "Refresh", icon: "Refresh", behavior: { kind: "action", onPress() { return "header-pressed"; } } } });
    const pill = plugin.addComposerPill({ id: "send", workspaceId: "workspace-1", agentId: "agent-1", button: { title: "Send", icon: "Send", behavior: { kind: "action", onPress() { return "pill-pressed"; } } } });
    return async () => { header.remove(); pill.remove(); for (const remove of removals.reverse()) remove(); };
  };
  return module.exports;
})"##;

#[test]
fn all_headless_client_contributions_execute_and_clean_up() {
    let compiled = CompiledPluginClient::from_bundle(FULL_CLIENT_BUNDLE);
    let mut runtime = compiled
        .start(Duration::from_secs(5))
        .expect("start runtime");

    assert_eq!(runtime.contributions().len(), 12);
    assert!(
        runtime
            .contributions()
            .contains(&ClientContribution::WorkspacePanel {
                id: "workspace".into(),
                context: "workspace".into(),
                locations: vec!["workspace".into(), "explorer".into()],
            })
    );
    assert!(
        runtime
            .contributions()
            .contains(&ClientContribution::TimelineRenderer {
                kind: "summary".into(),
                version: 1,
            })
    );
    assert_eq!(
        runtime
            .invoke_command_with_context("global-command", &json!({}), Duration::from_secs(5))
            .expect("command"),
        json!(1)
    );
    assert_eq!(
        runtime
            .invoke_slash_command("review", "src/lib.rs", &json!({}), Duration::from_secs(5))
            .expect("slash"),
        json!("SRC/LIB.RS")
    );
    assert_eq!(
        runtime
            .transform_timeline(
                "assistant",
                &json!({"type":"assistant_message","text":"done"}),
                "complete",
                Duration::from_secs(5)
            )
            .expect("transform"),
        json!({"items":[{"type":"plugin","kind":"summary","version":1,"data":{"phase":"complete"}}]})
    );
    assert_eq!(
        runtime
            .press_button("headerButton", "refresh", Duration::from_secs(5))
            .expect("header"),
        json!("header-pressed")
    );

    runtime
        .set_host_online(false, Duration::from_secs(5))
        .expect("offline");
    assert_eq!(
        runtime.invoke_command_with_context("global-command", &json!({}), Duration::from_secs(5)),
        Err(PluginError::ClientDisconnected)
    );
    runtime
        .set_host_online(true, Duration::from_secs(5))
        .expect("reconnect");
    assert_eq!(
        runtime
            .invoke_command_with_context("global-command", &json!({}), Duration::from_secs(5))
            .expect("reconnected"),
        json!(2)
    );

    runtime.shutdown(Duration::from_secs(5)).expect("cleanup");
    assert_eq!(runtime.active_registration_count(), 0);
}

#[test]
fn timed_out_client_action_cancels_and_reaps_runtime() {
    let bundle = r#"(function() { return { default(plugin) { plugin.addCommandCenterItem({ id: "wait", title: "Wait", icon: "Clock", context: "global", async onSelect() { await new Promise(() => {}); } }); return () => {}; } }; })"#;
    let compiled = CompiledPluginClient::from_bundle(bundle);
    let mut runtime = compiled
        .start(Duration::from_secs(5))
        .expect("start runtime");
    assert_eq!(
        runtime.invoke_command_with_context("wait", &json!({}), Duration::from_millis(100)),
        Err(PluginError::RuntimeTimedOut)
    );
    assert!(runtime.is_stopped());
}
