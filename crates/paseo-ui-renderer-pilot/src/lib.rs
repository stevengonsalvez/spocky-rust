use dioxus::prelude::*;

pub const APP_TITLE: &str = "Paseo";

const SHELL_CSS: &str = r#"
:root { color-scheme: light; font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
* { box-sizing: border-box; }
body { margin: 0; background: #fff; color: #222329; }
button, a { font: inherit; }
.shell { min-height: 100vh; display: grid; grid-template-columns: 320px 1fr; background: #fff; }
.sidebar { min-height: 100vh; display: flex; flex-direction: column; border-right: 1px solid #e3e3e8; background: #f8f8fa; color: #6e7079; font-size: 14px; }
.nav-list { display: grid; gap: 2px; padding: 10px 10px 12px; border-bottom: 1px solid #e3e3e8; }
.nav-button { min-height: 28px; display: flex; align-items: center; gap: 10px; padding: 4px 7px; border: 0; border-radius: 7px; color: inherit; background: transparent; text-align: left; cursor: pointer; }
.nav-button:hover, .nav-button:focus-visible { background: #ececf0; color: #28292f; outline: 2px solid #8ba9d8; outline-offset: -2px; }
.nav-icon { width: 12px; text-align: center; color: #72747c; }
.sidebar-spacer { flex: 1; }
.sidebar-footer { min-height: 57px; display: flex; align-items: center; gap: 14px; padding: 10px 15px; border-top: 1px solid #e3e3e8; }
.sidebar-footer .nav-button:first-child { flex: 1; }
.icon-button { min-width: 18px; padding: 4px 2px; }
.mobile-menu { display: none; position: absolute; z-index: 2; top: 24px; left: 16px; width: 28px; height: 28px; border: 0; background: transparent; color: #666873; font-size: 20px; cursor: pointer; }
.workspace { position: relative; min-width: 0; min-height: 100vh; padding: 198px 24px 72px; }
.content { width: min(452px, 100%); margin: 0 auto; }
.mark { position: relative; width: 48px; height: 48px; margin: 0 auto 78px; }
.mark span { position: absolute; left: 13px; top: 4px; width: 19px; height: 35px; border: 4px solid #25262b; border-radius: 50%; transform: rotate(-25deg); }
.mark span:nth-child(2) { left: 9px; top: 17px; height: 28px; transform: rotate(28deg); }
.mark span:nth-child(3) { left: 25px; top: 20px; width: 13px; height: 25px; border-color: #55565d; transform: rotate(43deg); }
.actions { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
.action { min-height: 141px; padding: 14px 16px; border: 1px solid #dedee4; border-radius: 12px; background: #fbfbfc; color: #24252a; text-align: left; cursor: pointer; }
.action:hover, .action:focus-visible { border-color: #aeb0b9; box-shadow: 0 1px 4px rgb(28 29 34 / 10%); outline: 2px solid #8ba9d8; outline-offset: 2px; }
.action-icon { display: block; margin-bottom: 12px; color: #767881; font-size: 19px; line-height: 1; }
.action:first-child .action-icon { color: #24895a; }
.action-title { display: block; margin-bottom: 4px; font-size: 14px; line-height: 20px; }
.action-detail { display: block; color: #777983; font-size: 14px; line-height: 18px; }
.community { position: absolute; right: 0; bottom: 50px; left: 0; display: flex; justify-content: center; gap: 24px; color: #72747d; font-size: 14px; }
.community a { color: inherit; text-decoration: none; }
.community a:hover, .community a:focus-visible { color: #292a30; text-decoration: underline; outline: none; }
.sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
@media (max-width: 42rem) {
  .shell { display: block; }
  .sidebar { display: none; }
  .mobile-menu { display: block; }
  .workspace { min-height: 100vh; padding: 96px 24px 92px; }
  .content { width: 100%; }
  .mark { margin-bottom: 76px; }
  .actions { grid-template-columns: 1fr; gap: 12px; }
  .action { min-height: 105px; padding: 14px 16px; }
  .action-icon { margin-bottom: 10px; }
  .community { bottom: 78px; gap: 23px; }
}
"#;

#[derive(Clone, Copy)]
struct ProjectAction {
    icon: &'static str,
    title: &'static str,
    detail: &'static str,
}

const PROJECT_ACTIONS: [ProjectAction; 3] = [
    ProjectAction {
        icon: "▱",
        title: "Add a project",
        detail: "Open a folder on your machine",
    },
    ProjectAction {
        icon: "▣",
        title: "Import session",
        detail: "Open a Claude Code, Codex or other session you started in a terminal",
    },
    ProjectAction {
        icon: "♧",
        title: "Setup providers",
        detail: "Configure Claude Code, Codex, and more",
    },
];

/// Interactive open-project shell shared by web, desktop, and mobile renderers.
///
/// # Errors
///
/// Dioxus returns a rendering error when component construction fails.
#[allow(non_snake_case)]
pub fn PaseoShell() -> Element {
    let mut selected_action = use_signal(|| None::<usize>);
    let status = selected_action().map_or_else(
        || "Choose a project action.".to_owned(),
        |index| format!("Selected action: {}.", PROJECT_ACTIONS[index].title),
    );

    rsx! {
        style { {SHELL_CSS} }
        main { class: "shell",
            nav { class: "sidebar", aria_label: "Primary navigation",
                div { class: "nav-list",
                    button { class: "nav-button", span { class: "nav-icon", "+" } "New workspace" }
                    button { class: "nav-button", span { class: "nav-icon", "◴" } "History" }
                    button { class: "nav-button", span { class: "nav-icon", "⌕" } "Search" }
                    button { class: "nav-button", span { class: "nav-icon", "◫" } "Schedules" }
                }
                div { class: "sidebar-spacer" }
                div { class: "sidebar-footer",
                    button { class: "nav-button", span { class: "nav-icon", "⊞" } "Add project" }
                    button { class: "nav-button icon-button", aria_label: "Hosts", "▤" }
                    button { class: "nav-button icon-button", aria_label: "Import", "⇩" }
                    button { class: "nav-button icon-button", aria_label: "Activity", "◜" }
                    button { class: "nav-button icon-button", aria_label: "Help", "?" }
                    button { class: "nav-button icon-button", aria_label: "Settings", "⚙" }
                }
            }
            button { class: "mobile-menu", aria_label: "Open menu", "☰" }
            section { class: "workspace", aria_label: "Open project",
                div { class: "content",
                    div { class: "mark", aria_hidden: "true",
                        span {}
                        span {}
                        span {}
                    }
                    div { class: "actions",
                        for (index, action) in PROJECT_ACTIONS.iter().enumerate() {
                            button {
                                class: "action",
                                onclick: move |_| selected_action.set(Some(index)),
                                span { class: "action-icon", aria_hidden: "true", "{action.icon}" }
                                span { class: "action-title", "{action.title}" }
                                span { class: "action-detail", "{action.detail}" }
                            }
                        }
                    }
                }
                footer { class: "community",
                    a { href: "https://github.com/getpaseo/paseo", "◉ Star" }
                    a { href: "https://github.com/sponsors/getpaseo", "♡ Sponsor" }
                    a { href: "https://discord.gg/paseo", "♣ Community" }
                }
                output { class: "sr-only", role: "status", aria_live: "polite", "{status}" }
                span { class: "sr-only", {APP_TITLE} }
            }
        }
    }
}

/// Produces deterministic HTML for DOM and accessibility-contract checks.
#[cfg(feature = "host")]
#[must_use]
pub fn render_shell_html() -> String {
    dioxus_ssr::render_element(rsx! { PaseoShell {} })
}
