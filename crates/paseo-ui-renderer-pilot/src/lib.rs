use dioxus::prelude::*;

pub const APP_TITLE: &str = "Paseo Agent Console";

const SHELL_CSS: &str = r#"
:root { color-scheme: dark; font-family: ui-sans-serif, system-ui, sans-serif; }
body { margin: 0; background: #0d1117; color: #f0f6fc; }
.shell { min-height: 100vh; display: grid; grid-template-columns: minmax(13rem, 18rem) 1fr; }
.sidebar { padding: 1.25rem; border-right: 1px solid #30363d; background: #161b22; }
.workspace { padding: 2rem; }
.agent-list { display: grid; gap: .75rem; padding: 0; list-style: none; }
.agent { width: 100%; padding: .9rem 1rem; border: 1px solid #30363d; border-radius: .6rem; color: inherit; background: #21262d; text-align: left; cursor: pointer; }
.agent[aria-pressed="true"] { border-color: #58a6ff; background: #1f3b57; }
.status { color: #8b949e; }
@media (max-width: 42rem) { .shell { grid-template-columns: 1fr; } .sidebar { border-right: 0; border-bottom: 1px solid #30363d; } }
"#;

#[derive(Clone, Copy)]
struct Agent {
    name: &'static str,
    status: &'static str,
}

const AGENTS: [Agent; 2] = [
    Agent {
        name: "Implementer",
        status: "Running",
    },
    Agent {
        name: "Reviewer",
        status: "Waiting",
    },
];

/// Interactive shell shared by web, desktop, and mobile renderers.
///
/// # Errors
///
/// Dioxus returns a rendering error when component construction fails.
#[allow(non_snake_case)]
pub fn PaseoShell() -> Element {
    let mut selected_agent = use_signal(|| 0_usize);
    let selected = AGENTS[selected_agent()];

    rsx! {
        style { {SHELL_CSS} }
        main { class: "shell",
            nav { class: "sidebar", aria_label: "Workspaces",
                h1 { {APP_TITLE} }
                p { class: "status", "Connected locally" }
                h2 { "Workspaces" }
                button { class: "agent", aria_pressed: "true", "paseo-rewrite" }
            }
            section { class: "workspace", aria_labelledby: "agents-heading",
                h2 { id: "agents-heading", "Agents" }
                ul { class: "agent-list",
                    for (index, agent) in AGENTS.iter().enumerate() {
                        li {
                            button {
                                class: "agent",
                                aria_pressed: selected_agent() == index,
                                onclick: move |_| selected_agent.set(index),
                                span { "{agent.name}" }
                                span { class: "status", " {agent.status}" }
                            }
                        }
                    }
                }
                output { role: "status", aria_live: "polite",
                    "Selected agent: {selected.name}. Status: {selected.status}."
                }
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
