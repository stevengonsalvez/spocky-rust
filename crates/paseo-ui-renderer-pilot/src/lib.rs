use dioxus::prelude::*;

pub const APP_TITLE: &str = "Paseo";

const SHELL_CSS: &str = r#"
:root { color-scheme: light; font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }
* { box-sizing: border-box; }
body { margin: 0; background: #fff; color: #1a1a1e; }
button, a { font: inherit; }
.shell { min-height: 100vh; display: grid; grid-template-columns: 320px 1fr; background: #fff; }
.sidebar { min-height: 100vh; display: flex; flex-direction: column; border-right: 1px solid #e4e4e7; background: #f4f4f5; color: #71717a; font-size: 14px; }
.nav-list { display: grid; gap: 2px; padding: 6px 10px 7px; border-bottom: 1px solid #e4e4e7; }
.nav-button { min-height: 28px; display: flex; align-items: center; gap: 10px; padding: 4px 7px; border: 0; border-radius: 7px; color: inherit; background: transparent; text-align: left; cursor: pointer; }
.nav-button:hover, .nav-button:focus-visible { background: #ececf0; color: #28292f; outline: 2px solid #8ba9d8; outline-offset: -2px; }
.nav-icon { width: 12px; text-align: center; color: #72747c; }
.icon-add::before { content: "+"; }
.icon-history::before { content: "◴"; }
.icon-search::before { content: "⌕"; }
.icon-schedule::before { content: "◫"; }
.icon-project::before { content: "⊞"; }
.icon-hosts::before { content: "▤"; }
.icon-import::before { content: "⇩"; }
.icon-activity::before { content: "◜"; }
.icon-help::before { content: "?"; }
.icon-settings::before { content: "⚙"; }
.sidebar-empty { margin: 12px 8px 0; padding: 16px; border: 1px solid #e4e4e7; border-radius: 8px; color: #1a1a1e; }
.sidebar-empty-title { margin: 0 0 4px; font-size: 12px; line-height: 16px; }
.sidebar-empty-detail { margin: 0; color: #71717a; font-size: 12px; line-height: 16px; }
.sidebar-empty-actions { display: flex; gap: 8px; margin-top: 16px; }
.sidebar-empty-actions button { min-height: 28px; padding: 4px 12px; border: 1px solid transparent; border-radius: 14px; background: #e4e4e7; color: #1a1a1e; font-size: 12px; cursor: pointer; }
.sidebar-empty-actions button:last-child { border-color: #e4e4e7; background: transparent; }
.sidebar-spacer { flex: 1; }
.sidebar-footer { min-height: 57px; display: flex; align-items: center; gap: 14px; padding: 10px 15px; border-top: 1px solid #e4e4e7; }
.sidebar-footer .nav-button:first-child { flex: 1; }
.icon-button { min-width: 18px; padding: 4px 2px; }
.mobile-menu { display: none; position: absolute; z-index: 2; top: 24px; left: 16px; width: 28px; height: 28px; border: 0; background: transparent; color: #666873; font-size: 20px; cursor: pointer; }
.mobile-menu::before { content: "☰"; }
.workspace { position: relative; min-width: 0; min-height: 100vh; padding: 198px 24px 72px; }
.content { width: min(452px, 100%); margin: 0 auto; }
.mark { width: 52px; height: 52px; margin: 0 auto 76px; }
.mark svg { display: block; width: 52px; height: 52px; fill: #1a1a1e; }
.actions { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
.action { min-height: 141px; padding: 14px 16px; border: 1px solid #dedee4; border-radius: 12px; background: #fbfbfc; color: #24252a; text-align: left; cursor: pointer; }
.action:hover, .action:focus-visible { border-color: #aeb0b9; box-shadow: 0 1px 4px rgb(28 29 34 / 10%); outline: 2px solid #8ba9d8; outline-offset: 2px; }
.action-icon { display: block; width: 20px; height: 20px; margin-bottom: 12px; color: #71717a; }
.action:first-child .action-icon { color: #20744a; }
.action-icon svg { display: block; width: 20px; height: 20px; fill: none; stroke: currentColor; stroke-width: 2; stroke-linecap: round; stroke-linejoin: round; }
.action-title { display: block; margin-bottom: 4px; font-size: 14px; line-height: 20px; }
.action-detail { display: block; color: #777983; font-size: 14px; line-height: 18px; }
.community { position: absolute; right: 0; bottom: 50px; left: 0; display: flex; justify-content: center; gap: 24px; color: #72747d; font-size: 14px; }
.community a { color: inherit; text-decoration: none; }
.community-icon { margin-right: 6px; }
.icon-star::before { content: "◉"; }
.icon-sponsor::before { content: "♡"; }
.icon-community::before { content: "♣"; }
.community a:hover, .community a:focus-visible { color: #292a30; text-decoration: underline; outline: none; }
.sr-only { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip: rect(0, 0, 0, 0); white-space: nowrap; border: 0; }
@media (max-width: 42rem) {
  .shell { display: block; }
  .sidebar { display: none; }
  .sidebar-empty { display: none; }
  .mobile-menu { display: block; }
  .workspace { min-height: 100vh; padding: 114px 24px 92px; }
  .content { width: 100%; }
  .mark { margin-bottom: 58px; }
  .actions { grid-template-columns: 1fr; gap: 12px; }
  .action { min-height: 105px; padding: 14px 16px; }
  .action-icon { margin-bottom: 10px; }
  .community { bottom: 78px; gap: 23px; }
}
"#;

#[derive(Clone, Copy)]
struct ProjectAction {
    icon: ProjectIcon,
    title: &'static str,
    detail: &'static str,
}

#[derive(Clone, Copy)]
enum ProjectIcon {
    FolderOpen,
    Inbox,
    Plug,
}

const PROJECT_ACTIONS: [ProjectAction; 3] = [
    ProjectAction {
        icon: ProjectIcon::FolderOpen,
        title: "Add a project",
        detail: "Open a folder on your machine",
    },
    ProjectAction {
        icon: ProjectIcon::Inbox,
        title: "Import session",
        detail: "Open a Claude Code, Codex or other session you started in a terminal",
    },
    ProjectAction {
        icon: ProjectIcon::Plug,
        title: "Setup providers",
        detail: "Configure Claude Code, Codex, and more",
    },
];

const PASEO_LOGO_PATH: &str = "M291.495 91.399C333.897 104.892 379.155 135.075 416.229 173.191C453.389 211.394 484.429 259.725 495.708 311.251C497.555 319.693 498.865 328.216 499.586 336.776C509.755 326.554 519.867 317.815 529.89 311.547C540.647 304.821 553.808 299.297 568.641 299.785C584.29 300.299 597.395 307.326 607.747 317.632C632.173 341.947 629.612 372.898 619.872 397.936C610.185 422.833 591.557 447.826 572.732 469.124C553.591 490.78 532.713 510.308 516.779 524.318C508.775 531.355 501.936 537.073 497.07 541.052C494.635 543.043 492.689 544.603 491.334 545.679C490.657 546.217 490.126 546.635 489.756 546.926C489.571 547.071 489.425 547.184 489.321 547.265C489.269 547.305 489.227 547.338 489.196 547.362C489.181 547.374 489.168 547.385 489.157 547.393C489.153 547.397 489.147 547.401 489.144 547.403C489.134 547.4 488.837 547.06 473.001 528.499L489.135 547.411C478.157 555.911 462.033 554.334 453.122 543.89C444.213 533.448 445.887 518.094 456.861 509.592C456.863 509.591 456.865 509.588 456.869 509.586C456.88 509.577 456.902 509.561 456.933 509.536C456.997 509.487 457.101 509.404 457.245 509.292C457.533 509.066 457.979 508.715 458.569 508.247C459.749 507.31 461.506 505.901 463.742 504.073C468.216 500.414 474.589 495.088 482.073 488.508C497.114 475.284 516.315 457.282 533.578 437.75C551.157 417.862 565.26 398.01 571.859 381.048C578.403 364.227 575.681 356.302 570.724 351.367C568.928 349.579 567.744 348.902 567.267 348.676C566.888 348.496 566.811 348.52 566.804 348.52C566.605 348.513 563.971 348.537 557.953 352.3C545.161 360.299 528.815 377.492 506.807 403.867C494.927 418.106 481.871 434.435 467.547 451.957C463.709 457.28 459.503 462.538 454.91 467.717L454.702 467.549C420.808 508.347 380.37 553.856 332.335 593.848C301.853 619.226 262.656 622.597 228.642 614.743C194.834 606.936 162.658 587.448 142.217 561.686C108.054 518.631 100.57 469.801 108.223 427.836C115.56 387.606 137.391 351.005 166.502 331.557C161.248 315.813 156.813 299.49 153.519 283.013C142.593 228.368 143.239 167.031 174.28 119.619C186.922 100.31 205.846 89.1535 227.387 85.2773C248.1 81.5504 270.278 84.648 291.495 91.399ZM378.642 206.356C345.773 172.563 307.463 147.917 275.208 137.654C259.096 132.527 246.171 131.514 236.828 133.195C228.314 134.727 222.227 138.497 217.721 145.38C196.712 177.468 193.858 224.004 203.82 273.827C206.532 287.394 210.127 300.834 214.345 313.817C236.45 310.276 260.156 311.463 281.22 317.11C319.621 327.403 357.501 355.419 357.501 405.654C357.501 435.255 339.111 465.136 307.278 473.815C273.211 483.103 238.854 464.822 213.105 427.541C203.716 413.947 194.443 397.766 185.947 379.89C174.028 392.223 163.08 411.953 158.673 436.118C153.128 466.518 158.514 501.286 183.085 532.253C195.993 548.522 217.742 562.031 240.771 567.349C263.594 572.619 284.147 569.24 298.664 557.154C349.383 514.927 390.709 466.547 426.366 422.952C448.879 390.86 453.195 356.06 445.578 321.265C436.703 280.718 411.425 240.06 378.642 206.356ZM306.296 405.722C306.296 384.769 292.223 370.736 267.284 364.051C256.012 361.03 244.156 360.087 233.095 360.771C240.361 375.935 248.168 389.513 255.897 400.704C275.647 429.298 289.989 427.822 293.247 426.934C298.737 425.437 306.296 418.161 306.296 405.722Z";

fn project_icon(icon: ProjectIcon) -> Element {
    match icon {
        ProjectIcon::FolderOpen => rsx! {
            svg { view_box: "0 0 24 24",
                path { d: "m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6A2 2 0 0 1 18.45 20H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H19a2 2 0 0 1 2 2v2" }
            }
        },
        ProjectIcon::Inbox => rsx! {
            svg { view_box: "0 0 24 24",
                polyline { points: "22 12 16 12 14 15 10 15 8 12 2 12" }
                path { d: "M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z" }
            }
        },
        ProjectIcon::Plug => rsx! {
            svg { view_box: "0 0 24 24",
                path { d: "M12 22v-5" }
                path { d: "M9 8V2" }
                path { d: "M15 8V2" }
                path { d: "M18 8v5a6 6 0 0 1-12 0V8Z" }
            }
        },
    }
}

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
                    button { class: "nav-button", aria_label: "New workspace", span { class: "nav-icon icon-add" } "New workspace" }
                    button { class: "nav-button", aria_label: "History", span { class: "nav-icon icon-history" } "History" }
                    button { class: "nav-button", aria_label: "Search", span { class: "nav-icon icon-search" } "Search" }
                    button { class: "nav-button", aria_label: "Schedules", span { class: "nav-icon icon-schedule" } "Schedules" }
                }
                div { class: "sidebar-empty",
                    p { class: "sidebar-empty-title", "No projects yet" }
                    p { class: "sidebar-empty-detail", "Add a project to get started" }
                    div { class: "sidebar-empty-actions",
                        button { "+  Add project" }
                        button { "⇩  Import session" }
                    }
                }
                div { class: "sidebar-spacer" }
                div { class: "sidebar-footer",
                    button { class: "nav-button", span { class: "nav-icon icon-project" } "Add project" }
                    button { class: "nav-button icon-button", aria_label: "Hosts", span { class: "nav-icon icon-hosts" } }
                    button { class: "nav-button icon-button", aria_label: "Import", span { class: "nav-icon icon-import" } }
                    button { class: "nav-button icon-button", aria_label: "Activity", span { class: "nav-icon icon-activity" } }
                    button { class: "nav-button icon-button", aria_label: "Help", span { class: "nav-icon icon-help" } }
                    button { class: "nav-button icon-button", aria_label: "Settings", span { class: "nav-icon icon-settings" } }
                }
            }
            button { class: "mobile-menu", aria_label: "Open menu" }
            section { class: "workspace", aria_label: "Open project",
                div { class: "content",
                    div { class: "mark", aria_hidden: "true",
                        svg { view_box: "0 0 700 700", path { d: PASEO_LOGO_PATH } }
                    }
                    div { class: "actions",
                        for (index, action) in PROJECT_ACTIONS.iter().enumerate() {
                            div {
                                class: "action",
                                role: "button",
                                tabindex: "0",
                                onclick: move |_| selected_action.set(Some(index)),
                                onkeydown: move |event: KeyboardEvent| {
                                    if event.key() == Key::Enter {
                                        selected_action.set(Some(index));
                                    }
                                },
                                span { class: "action-icon", {project_icon(action.icon)} }
                                span { class: "action-title", "{action.title}" }
                                span { class: "action-detail", "{action.detail}" }
                            }
                        }
                    }
                }
                footer { class: "community",
                    a { href: "https://github.com/getpaseo/paseo", span { class: "community-icon icon-star", aria_hidden: "true" } "Star" }
                    a { href: "https://github.com/sponsors/getpaseo", span { class: "community-icon icon-sponsor", aria_hidden: "true" } "Sponsor" }
                    a { href: "https://discord.gg/paseo", span { class: "community-icon icon-community", aria_hidden: "true" } "Community" }
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
