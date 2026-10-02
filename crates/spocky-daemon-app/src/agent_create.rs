//! `agent.create.request`, as `session.ts` handles it
//! (`handleAgentCreation`, `createRequestedAgent`, `createSessionAgent`,
//! `resolveSessionCreateAgentIntent`) over `create.ts`
//! (`createAgentCommand`, `resolveSessionCreateAgent`, `sendInitialPrompt`)
//! and `agent-prompt.ts` (`startCreatedAgentInitialPrompt`).
//!
//! Worktree targets, legacy git options, auto-archive, caller-less creates
//! without a workspace, and the stale-provider retry are not ported: those
//! requests fail with a not-ported error before any state changes.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use spocky_contracts::js::{spread, truthy};
use spocky_contracts::js_value::{self, JsObject, JsValue};
use spocky_contracts::json::js_wire_text;
use spocky_contracts::request::AgentCreateRequest;
use spocky_contracts::text::js_trim;
use spocky_session::agent_identity::resolve_create_agent_titles;
use spocky_session::agent_labels::PARENT_AGENT_ID_LABEL;
use spocky_session::agent_manager::{AgentManager, CreateAgentOptions, ManagedAgentSnapshot};
use spocky_session::agent_sdk::{AgentPromptInput, AgentRunOptions};
use spocky_session::creation::{CreationError, CreationInput, CreationTarget, OnReady};
use spocky_session::paths::{expand_tilde, resolve_from_cwd};
use spocky_session::provider_snapshot_manager::ResolveProviderCreateConfigOptions;

use crate::agent_updates::AgentUpdates;
use crate::request::Emit;
use crate::session::{Services, agent_payload, frame, js_text};
use crate::workspace_handlers::{creation_observer, resource_exists};

/// `AGENT_RUN_START_TIMEOUT_MS`.
const AGENT_RUN_START_TIMEOUT: Duration = Duration::from_secs(60);

fn not_ported(what: &str) -> String {
    format!("agent.create with {what} is not ported in spocky-daemon-app yet")
}

/// The request as zod outputs it, so spreads keep its key order.
fn request_object(request: &AgentCreateRequest) -> JsObject {
    let text = serde_json::to_string(request).expect("requests serialize");
    spread(Some(
        &js_value::parse(&js_wire_text(&text)).expect("serde_json writes JSON"),
    ))
}

fn text<'a>(object: &'a JsValue, key: &str) -> Option<&'a str> {
    object.get(key).and_then(JsValue::as_str)
}

fn array(value: Option<&JsValue>) -> &[JsValue] {
    match value {
        Some(JsValue::Array(items)) => items,
        _ => &[],
    }
}

/// `buildAgentPrompt(text, images, attachments)`.
pub(crate) fn build_agent_prompt(
    prompt: &str,
    images: Option<&JsValue>,
    attachments: Option<&JsValue>,
) -> AgentPromptInput {
    let normalized = js_trim(prompt);
    let images = array(images);
    let attachments = array(attachments);
    if images.is_empty() && attachments.is_empty() {
        return AgentPromptInput::Text(normalized.to_owned());
    }
    let (chat_history, others): (Vec<&JsValue>, Vec<&JsValue>) =
        attachments.iter().partition(|attachment| {
            text(attachment, "type") == Some("text")
                && text(attachment, "contextKind") == Some("chat_history")
        });
    let mut blocks: Vec<JsValue> = chat_history.into_iter().cloned().collect();
    if !normalized.is_empty() {
        let mut block = JsObject::new();
        block.insert("type", JsValue::String("text".to_owned()));
        block.insert("text", JsValue::String(normalized.to_owned()));
        blocks.push(JsValue::Object(block));
    }
    for image in images {
        let mut block = JsObject::new();
        block.insert("type", JsValue::String("image".to_owned()));
        block.insert(
            "data",
            image.get("data").cloned().unwrap_or(JsValue::Undefined),
        );
        block.insert(
            "mimeType",
            image.get("mimeType").cloned().unwrap_or(JsValue::Undefined),
        );
        blocks.push(JsValue::Object(block));
    }
    blocks.extend(others.into_iter().cloned());
    AgentPromptInput::Blocks(blocks)
}

const fn has_prompt_content(prompt: &AgentPromptInput) -> bool {
    match prompt {
        AgentPromptInput::Text(text) => !text.is_empty(),
        AgentPromptInput::Blocks(blocks) => !blocks.is_empty(),
    }
}

/// `initialPrompt?.trim()`.
fn trimmed_prompt(request: &JsObject) -> Option<String> {
    request
        .get("initialPrompt")
        .and_then(JsValue::as_str)
        .map(|prompt| js_trim(prompt).to_owned())
}

/// `hasInitialCreationPrompt(request)`.
fn has_initial_creation_prompt(request: &JsObject) -> bool {
    has_prompt_content(&build_agent_prompt(
        trimmed_prompt(request).as_deref().unwrap_or_default(),
        request.get("images"),
        request.get("attachments"),
    ))
}

/// `normalizeClientMessageId`.
fn normalize_client_message_id(value: Option<&JsValue>) -> Option<String> {
    value
        .and_then(JsValue::as_str)
        .map(js_trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

fn is_directory(path: &str) -> bool {
    Path::new(path).is_dir()
}

fn require_directory(path: &str) -> Result<(), String> {
    if is_directory(path) {
        Ok(())
    } else {
        Err(format!(
            "Working directory does not exist or is not a directory: {path}"
        ))
    }
}

/// `resolveCreateAgentIntent`'s placement and labels.
struct Intent {
    workspace_id: String,
    cwd: String,
    labels: JsValue,
}

/// `resolveSessionCreateAgentIntent` without a created worktree.
async fn resolve_intent(services: &Services, request: &JsObject) -> Result<Intent, String> {
    let caller = match request.get("callerAgentId").and_then(JsValue::as_str) {
        Some(caller_id) => Some(
            services
                .manager
                .get_agent(caller_id)
                .ok_or_else(|| format!("Caller agent {caller_id} not found"))?,
        ),
        None => None,
    };
    let explicit = request
        .get("workspaceId")
        .and_then(JsValue::as_str)
        .filter(|id| !id.is_empty());
    let (workspace_id, cwd) = if let Some(workspace_id) = explicit {
        let workspace = services
            .provisioning
            .workspaces
            .lock()
            .await
            .get(workspace_id)
            .filter(|workspace| workspace.archived_at.is_none())
            .ok_or_else(|| format!("Workspace {workspace_id} not found"))?;
        (workspace_id.to_owned(), workspace.cwd)
    } else if let Some(caller) = &caller {
        let workspace_id = caller
            .workspace_id
            .clone()
            .ok_or_else(|| format!("Caller agent {} has no workspace", caller.id))?;
        (workspace_id, caller.cwd.clone())
    } else {
        return Err(not_ported("no workspace and no caller agent"));
    };
    let mut labels = spread(request.get("labels"));
    if let Some(caller) = &caller {
        labels.insert(PARENT_AGENT_ID_LABEL, JsValue::String(caller.id.clone()));
    }
    Ok(Intent {
        workspace_id,
        cwd,
        labels: JsValue::Object(labels),
    })
}

/// `startAgentRun(agentManager, agentId, prompt, logger, options)`: the
/// out-of-band intercept, else a run drained in the background. `true` is
/// the `turn_started` disposition. With `replace_running`, an in-flight run
/// would be replaced (`replaceAgentRun`), which is not ported and fails.
pub(crate) fn start_agent_run(
    manager: &Arc<AgentManager>,
    agent_id: &str,
    prompt: AgentPromptInput,
    run_options: Option<AgentRunOptions>,
    replace_running: bool,
) -> Result<bool, String> {
    if manager
        .try_run_out_of_band(agent_id, &prompt, run_options.as_ref())
        .map_err(|error| error.message)?
    {
        return Ok(false);
    }
    if replace_running && manager.has_in_flight_run(agent_id) {
        return Err(
            "Replacing an in-flight agent run is not ported in spocky-daemon-app yet".to_owned(),
        );
    }
    let mut stream = manager
        .stream_agent(agent_id, prompt, run_options)
        .map_err(|error| error.message)?;
    // `drainAgentRunIterator`: events reach clients through the manager's
    // subscribers; a failed run is the baseline's logged "Agent stream
    // failed".
    tokio::spawn(async move { while let Some(Ok(_)) = stream.next().await {} });
    Ok(true)
}

/// `waitForAgentRunStartWithTimeout(agentManager, agentId)`.
pub(crate) async fn wait_for_run_start(
    manager: &Arc<AgentManager>,
    agent_id: &str,
) -> Result<(), String> {
    let provider = manager
        .get_agent(agent_id)
        .map_or_else(|| "provider".to_owned(), |agent| agent.provider);
    match tokio::time::timeout(
        AGENT_RUN_START_TIMEOUT,
        manager.wait_for_agent_run_start(agent_id, None),
    )
    .await
    {
        Ok(started) => started.map_err(|error| error.message),
        Err(_) => Err(format!(
            "{provider} run did not start within 60 seconds (phase: run start)"
        )),
    }
}

/// `startCreatedAgentInitialPrompt`.
async fn start_initial_prompt(
    manager: &Arc<AgentManager>,
    snapshot: &ManagedAgentSnapshot,
    prompt: AgentPromptInput,
    run_options: Option<AgentRunOptions>,
) -> Result<ManagedAgentSnapshot, String> {
    let agent_id = snapshot.id.as_str();
    if start_agent_run(manager, agent_id, prompt, run_options, false)? {
        wait_for_run_start(manager, agent_id).await?;
    }
    Ok(manager
        .get_agent(agent_id)
        .unwrap_or_else(|| snapshot.clone()))
}

/// `createSessionAgent` over `createAgentCommand` for the session input.
async fn create_session_agent(
    services: Arc<Services>,
    updates: Arc<AgentUpdates>,
    request: JsObject,
    agent_id: Option<String>,
    on_ready: OnReady,
) -> Result<JsValue, String> {
    let request_value = JsValue::Object(request.clone());
    let config = request.get("config").cloned().unwrap_or(JsValue::Undefined);
    if truthy(request.get("worktree")) {
        return Err(not_ported("a worktree target"));
    }
    if truthy(request.get("git")) || truthy(request.get("worktreeName")) {
        return Err(not_ported("git options"));
    }
    if request.get("autoArchive") == Some(&JsValue::Bool(true)) {
        return Err(not_ported("autoArchive"));
    }
    let requested_cwd = resolve_from_cwd(text(&config, "cwd").unwrap_or_default());
    let needs_requested_directory =
        !truthy(request.get("workspaceId")) && !truthy(request.get("callerAgentId"));
    if needs_requested_directory {
        require_directory(&requested_cwd)?;
    }
    let trimmed = trimmed_prompt(&request);
    let provisional_title =
        resolve_create_agent_titles(text(&config, "title"), trimmed.as_deref()).provisional_title;
    let intent = resolve_intent(&services, &request).await?;
    require_directory(&resolve_from_cwd(&intent.cwd))?;

    // `buildSessionConfig` without git options: `{ ...config, cwd }` with
    // the tilde expanded.
    let mut session_config = spread(Some(&config));
    session_config.insert(
        "cwd",
        JsValue::String(expand_tilde(&intent.cwd, &services.home)),
    );
    let resolved = services
        .snapshots
        .resolve_create_config(ResolveProviderCreateConfigOptions {
            cwd: text(&JsValue::Object(session_config.clone()), "cwd").map(str::to_owned),
            provider: text(&config, "provider").unwrap_or_default().to_owned(),
            requested_mode: text(&config, "modeId").map(str::to_owned),
            feature_values: config.get("featureValues").cloned(),
            parent: None,
            unattended: false,
        })
        .await
        .map_err(|error| error.message)?;
    session_config.insert(
        "modeId",
        resolved.mode_id.map_or(JsValue::Undefined, JsValue::String),
    );
    session_config.insert(
        "featureValues",
        resolved.feature_values.unwrap_or(JsValue::Undefined),
    );

    let prompt = build_agent_prompt(
        trimmed.as_deref().unwrap_or_default(),
        request.get("images"),
        request.get("attachments"),
    );
    let client_message_id = normalize_client_message_id(request.get("clientMessageId"));
    let output_schema = request
        .get("outputSchema")
        .filter(|schema| truthy(Some(schema)))
        .cloned();
    let run_options =
        (output_schema.is_some() || client_message_id.is_some()).then(|| AgentRunOptions {
            output_schema,
            client_message_id,
            ..AgentRunOptions::default()
        });
    if intent.workspace_id.is_empty() {
        return Err("createAgentCommand requires a resolved workspaceId".to_owned());
    }
    let options = CreateAgentOptions {
        labels: Some(intent.labels),
        initial_prompt: trimmed,
        env: request_value
            .get("env")
            .and_then(JsValue::as_object)
            .cloned(),
        initial_title: provisional_title,
        workspace_id: Some(intent.workspace_id),
        ..CreateAgentOptions::default()
    };
    let snapshot = services
        .manager
        .create_agent(JsValue::Object(session_config), agent_id, options)
        .await
        .map_err(|error| error.message)?;
    on_ready(agent_payload(&services, &snapshot).await?)
        .await
        .map_err(|error| error.message)?;
    let live = if has_prompt_content(&prompt) {
        start_initial_prompt(&services.manager, &snapshot, prompt, run_options).await?
    } else {
        snapshot.clone()
    };
    // The run start's state change reaches subscribers before this stale
    // forward, as the baseline's synchronous dispatch orders them.
    services.manager.dispatched().await;
    updates.forward_live_agent_and_wait(&snapshot).await;
    agent_payload(&services, &live).await
}

/// `createRequestedAgent`.
async fn create_requested_agent(
    services: &Arc<Services>,
    updates: &Arc<AgentUpdates>,
    request: &AgentCreateRequest,
    emit: &Emit,
) -> Result<JsValue, CreationError> {
    let session_request = request_object(request);
    let key = request.idempotency_key.as_ref().map_or_else(
        || request.request_id.as_str().to_owned(),
        |key| key.as_str().to_owned(),
    );
    let has_prompt = has_initial_creation_prompt(&session_request);
    let mut intent = JsObject::new();
    for (key, value) in session_request.iter() {
        if !matches!(key, "requestId" | "type" | "subscribe" | "idempotencyKey") {
            intent.insert(key, value.clone());
        }
    }
    let read_services = Arc::clone(services);
    let create_services = Arc::clone(services);
    let updates = Arc::clone(updates);
    let input = CreationInput {
        target: CreationTarget::Agent {
            read_agent: Box::new(move |id| {
                Box::pin(async move {
                    match read_services.storage.get(&id).await {
                        None => Ok(None),
                        Some(_) => Err(CreationError::new(
                            crate::session::STORED_PAYLOAD_NOT_PORTED,
                        )),
                    }
                })
            }),
        },
        key,
        request: JsValue::Object(intent),
        workspace_id: request
            .workspace_id
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        agent_id: request.agent_id.as_ref().map(|id| id.as_str().to_owned()),
        has_agent: true,
        has_prompt,
        exists: resource_exists(services),
        provision: None,
        create_agent: Some(Box::new(move |id, _workspace, on_ready| {
            Box::pin(async move {
                create_session_agent(create_services, updates, session_request, id, on_ready)
                    .await
                    .map_err(|message| CreationError {
                        message,
                        code: Some("unknown".to_owned()),
                    })
            })
        })),
    };
    let observer = (request.subscribe == Some(true)).then(|| creation_observer(emit));
    services.creation.create(input, observer).await
}

/// `handleAgentCreation`.
pub async fn agent_create(
    services: &Arc<Services>,
    updates: &Arc<AgentUpdates>,
    request: AgentCreateRequest,
    emit: &Emit,
) {
    let mut payload = JsObject::new();
    payload.insert("requestId", js_text(&request.request_id));
    match create_requested_agent(services, updates, &request, emit).await {
        Ok(creation) => {
            let agent = match creation.get("agent") {
                None | Some(JsValue::Undefined | JsValue::Null) => JsValue::Null,
                Some(agent) => agent.clone(),
            };
            payload.insert("agent", agent);
            payload.insert(
                "error",
                creation.get("error").cloned().unwrap_or(JsValue::Undefined),
            );
            payload.insert("creation", creation);
        }
        Err(error) => {
            payload.insert("agent", JsValue::Null);
            payload.insert("error", JsValue::String(error.message));
        }
    }
    emit(frame("agent.create.response", payload));
}

#[cfg(test)]
mod tests {
    use spocky_contracts::js_value::parse;
    use spocky_session::agent_sdk::AgentPromptInput;

    use super::{build_agent_prompt, normalize_client_message_id};

    #[test]
    fn prompt_without_attachments_is_trimmed_text() {
        assert_eq!(
            build_agent_prompt("  hi \n", None, None),
            AgentPromptInput::Text("hi".to_owned())
        );
    }

    #[test]
    fn prompt_blocks_put_chat_history_first_and_other_attachments_last() {
        let images = parse(r#"[{"data":"AA==","mimeType":"image/png"}]"#).unwrap();
        let attachments = parse(
            r#"[{"type":"text","text":"t","contextKind":"other"},
                {"type":"text","text":"h","contextKind":"chat_history"}]"#,
        )
        .unwrap();
        let AgentPromptInput::Blocks(blocks) =
            build_agent_prompt(" go ", Some(&images), Some(&attachments))
        else {
            panic!("blocks expected");
        };
        let kinds: Vec<_> = blocks
            .iter()
            .map(|block| {
                (
                    block
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap()
                        .to_owned(),
                    block
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                )
            })
            .collect();
        assert_eq!(
            kinds,
            [
                ("text".to_owned(), Some("h".to_owned())),
                ("text".to_owned(), Some("go".to_owned())),
                ("image".to_owned(), None),
                ("text".to_owned(), Some("t".to_owned())),
            ]
        );
    }

    #[test]
    fn client_message_ids_are_trimmed_and_blank_ones_dropped() {
        assert_eq!(
            normalize_client_message_id(Some(&parse(r#"" m1 ""#).unwrap())),
            Some("m1".to_owned())
        );
        assert_eq!(
            normalize_client_message_id(Some(&parse(r#""  ""#).unwrap())),
            None
        );
        assert_eq!(normalize_client_message_id(None), None);
    }
}
