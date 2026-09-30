use serde::{Deserialize, Serialize};
use spocky_platform_bridge::{HostCapability, HostDescriptor};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Route {
    Workspaces,
    Workspace {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
    },
    Agent {
        #[serde(rename = "workspaceId")]
        workspace_id: String,
        #[serde(rename = "agentId")]
        agent_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePresentation {
    pub id: String,
    pub name: String,
}

impl WorkspacePresentation {
    #[must_use]
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPresentation {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub status: String,
}

impl AgentPresentation {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        workspace_id: impl Into<String>,
        name: impl Into<String>,
        status: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            workspace_id: workspace_id.into(),
            name: name.into(),
            status: status.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSource {
    Keyboard,
    Touch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiIntent {
    OpenWorkspaces,
    OpenWorkspace(String),
    OpenAgent(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputAction {
    pub source: InputSource,
    pub intent: UiIntent,
}

impl InputAction {
    #[must_use]
    pub fn new(source: InputSource, intent: UiIntent) -> Self {
        Self { source, intent }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisibleError {
    pub code: String,
    pub message: String,
}

pub type UiError = VisibleError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentationState {
    pub route: Route,
    pub workspaces: Vec<WorkspacePresentation>,
    pub agents: Vec<AgentPresentation>,
    pub visible_error: Option<VisibleError>,
}

impl PresentationState {
    #[must_use]
    pub fn new(workspaces: Vec<WorkspacePresentation>, agents: Vec<AgentPresentation>) -> Self {
        Self {
            route: Route::Workspaces,
            workspaces,
            agents,
            visible_error: None,
        }
    }

    /// Applies one keyboard or touch action with explicit host gating.
    ///
    /// # Errors
    ///
    /// Returns an error, renders it into presentation state, and leaves the
    /// route unchanged when input or destination support is absent.
    pub fn dispatch(&mut self, host: &HostDescriptor, action: InputAction) -> Result<(), UiError> {
        let required = match action.source {
            InputSource::Keyboard => HostCapability::KeyboardInput,
            InputSource::Touch => HostCapability::TouchInput,
        };
        if host.require([required]).is_err() {
            let input = match action.source {
                InputSource::Keyboard => "Keyboard",
                InputSource::Touch => "Touch",
            };
            return self.fail(
                "HOST_CAPABILITY_UNSUPPORTED",
                format!("{input} input is unavailable on this host."),
            );
        }

        let next_route = match action.intent {
            UiIntent::OpenWorkspaces => Route::Workspaces,
            UiIntent::OpenWorkspace(workspace_id) => {
                if !self
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.id == workspace_id)
                {
                    return self.fail(
                        "WORKSPACE_NOT_FOUND",
                        format!("Workspace {workspace_id} is unavailable."),
                    );
                }
                Route::Workspace { workspace_id }
            }
            UiIntent::OpenAgent(agent_id) => {
                let Some(agent) = self.agents.iter().find(|agent| agent.id == agent_id) else {
                    return self.fail(
                        "AGENT_NOT_FOUND",
                        format!("Agent {agent_id} is unavailable."),
                    );
                };
                Route::Agent {
                    workspace_id: agent.workspace_id.clone(),
                    agent_id,
                }
            }
        };
        self.route = next_route;
        self.visible_error = None;
        Ok(())
    }

    fn fail(&mut self, code: &str, message: String) -> Result<(), UiError> {
        let error = VisibleError {
            code: code.into(),
            message,
        };
        self.visible_error = Some(error.clone());
        Err(error)
    }

    #[must_use]
    pub fn accessibility_tree(&self) -> AccessibilityTree {
        match &self.route {
            Route::Workspaces => {
                let mut nodes = vec![AccessibilityNode {
                    role: AccessibilityRole::Navigation,
                    name: "Workspaces".into(),
                    focus_order: None,
                }];
                nodes.extend(
                    self.workspaces
                        .iter()
                        .enumerate()
                        .map(|(index, workspace)| AccessibilityNode {
                            role: AccessibilityRole::Button,
                            name: workspace.name.clone(),
                            focus_order: Some(index + 1),
                        }),
                );
                AccessibilityTree { nodes }
            }
            Route::Workspace { workspace_id } => {
                let workspace_name = self
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == *workspace_id)
                    .map_or(workspace_id.as_str(), |workspace| workspace.name.as_str());
                let mut nodes = vec![AccessibilityNode {
                    role: AccessibilityRole::Navigation,
                    name: workspace_name.into(),
                    focus_order: None,
                }];
                nodes.extend(
                    self.agents
                        .iter()
                        .filter(|agent| agent.workspace_id == *workspace_id)
                        .enumerate()
                        .map(|(index, agent)| AccessibilityNode {
                            role: AccessibilityRole::Button,
                            name: format!("{}, {}", agent.name, agent.status),
                            focus_order: Some(index + 1),
                        }),
                );
                AccessibilityTree { nodes }
            }
            Route::Agent { agent_id, .. } => {
                let name = self
                    .agents
                    .iter()
                    .find(|agent| agent.id == *agent_id)
                    .map_or(agent_id.as_str(), |agent| agent.name.as_str());
                AccessibilityTree {
                    nodes: vec![AccessibilityNode {
                        role: AccessibilityRole::Main,
                        name: name.into(),
                        focus_order: None,
                    }],
                }
            }
        }
    }

    /// Creates a deterministic headless visual snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot cannot be serialized as JSON.
    pub fn snapshot(&self, environment: VisualEnvironment) -> Result<Vec<u8>, serde_json::Error> {
        let content = match &self.route {
            Route::Workspaces => self
                .workspaces
                .iter()
                .map(|workspace| SnapshotContent {
                    kind: "workspace",
                    id: workspace.id.clone(),
                    name: workspace.name.clone(),
                })
                .collect(),
            Route::Workspace { workspace_id } => self
                .agents
                .iter()
                .filter(|agent| agent.workspace_id == *workspace_id)
                .map(|agent| SnapshotContent {
                    kind: "agent",
                    id: agent.id.clone(),
                    name: agent.name.clone(),
                })
                .collect(),
            Route::Agent { agent_id, .. } => self
                .agents
                .iter()
                .filter(|agent| agent.id == *agent_id)
                .map(|agent| SnapshotContent {
                    kind: "agent",
                    id: agent.id.clone(),
                    name: agent.name.clone(),
                })
                .collect(),
        };
        serde_json::to_vec(&VisualSnapshot {
            viewport: environment.viewport,
            theme: environment.theme,
            locale: environment.locale,
            reduced_motion: environment.reduced_motion,
            route: &self.route,
            content,
            visible_error: &self.visible_error,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessibilityRole {
    Navigation,
    Button,
    Main,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibilityNode {
    pub role: AccessibilityRole,
    pub name: String,
    pub focus_order: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibilityTree {
    pub nodes: Vec<AccessibilityNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
    pub scale_milli: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    Light,
    Dark,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Locale(String);

impl Locale {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualEnvironment {
    pub viewport: Viewport,
    pub theme: Theme,
    pub locale: Locale,
    pub reduced_motion: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VisualSnapshot<'a> {
    viewport: Viewport,
    theme: Theme,
    locale: Locale,
    reduced_motion: bool,
    route: &'a Route,
    content: Vec<SnapshotContent>,
    visible_error: &'a Option<VisibleError>,
}

#[derive(Serialize)]
struct SnapshotContent {
    kind: &'static str,
    id: String,
    name: String,
}
