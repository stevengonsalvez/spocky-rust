//! `spocky-daemon`: the production daemon. It takes no arguments; the home,
//! listen address, and provider settings come from the environment and
//! `config.json` as the transport and `config.ts` resolve them. The session
//! backend wires the agent manager, the Codex provider, agent records, and
//! the project and workspace registries (`bootstrap.ts`).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use serde_json::Value;
use spocky_daemon::daemon::{DaemonEnv, resolve_paseo_home};
use spocky_daemon::listen::{ListenTarget, parse_listen_string, resolve_listen_address};
use spocky_daemon::log::JsonLineLogger;
use spocky_daemon::process::run;
use spocky_daemon::server_id::get_or_create_server_id;
use spocky_daemon_app::bootstrap::{ensure_schedule_store_dir, materialize_opencode_bridge_plugin};
use spocky_daemon_app::codex_agent::CodexAgentClient;
use spocky_daemon_app::provider::codex_runtime_settings;
use spocky_daemon_app::session::{DaemonBackend, Services};
use spocky_session::agent_manager::{AgentManager, AgentManagerOptions, ProviderDefinition};
use spocky_session::agent_sdk::AgentClient;
use spocky_session::agent_storage::AgentStorage;
use spocky_session::checkout::CheckoutContext;
use spocky_session::provisioning::WorkspaceProvisioning;
use spocky_store::registry::{ProjectRegistry, WorkspaceRegistry};
use tokio::sync::Mutex;

/// `config.json` as JSON, or an empty object when it is absent or unreadable;
/// the transport's loader validates it and refuses to start on a bad file.
fn persisted_config(paseo_home: &Path) -> Value {
    std::fs::read_to_string(paseo_home.join("config.json"))
        .ok()
        .and_then(|text| serde_json::from_str(text.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
}

/// `resolveWorktreesRoot`: `worktrees.root`, tilde-expanded and resolved
/// against the home, or none.
fn worktrees_root(paseo_home: &Path, persisted: &Value, home: &str) -> Option<String> {
    let configured = persisted
        .get("worktrees")
        .and_then(|worktrees| worktrees.get("root"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|root| !root.is_empty())?;
    let expanded = match configured.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{home}{rest}"),
        _ => configured.to_owned(),
    };
    let path = PathBuf::from(&expanded);
    Some(if path.is_absolute() {
        expanded
    } else {
        paseo_home.join(path).to_string_lossy().into_owned()
    })
}

/// `daemon.mcp.<key>` as a boolean, `default` when absent (`config.ts`:
/// `mcp.enabled` defaults on, `mcp.injectIntoAgents` off).
fn mcp_flag(persisted: &Value, key: &str, default: bool) -> bool {
    persisted
        .get("daemon")
        .and_then(|daemon| daemon.get("mcp"))
        .and_then(|mcp| mcp.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

/// `createAgentMcpBaseUrl(listenTarget)` for the listen address the
/// transport resolves (`PASEO_LISTEN`, then `daemon.listen`, then `PORT`).
/// `null` for a socket or pipe listener.
// ponytail: uses the configured port; a `:0` listener would need the bound
// port from the transport.
fn agent_mcp_base_url(env: &DaemonEnv, persisted: &Value) -> Option<String> {
    let listen = resolve_listen_address(
        None,
        env.get("PASEO_LISTEN"),
        persisted
            .get("daemon")
            .and_then(|daemon| daemon.get("listen"))
            .and_then(Value::as_str),
        env.get("PORT"),
    );
    let ListenTarget::Tcp { host, port } = parse_listen_string(&listen).ok()? else {
        return None;
    };
    // `resolveAgentMcpClientHost`, then `formatHostForHttpUrl`.
    let host = match host.as_str() {
        "0.0.0.0" => "127.0.0.1".to_owned(),
        "::" | "[::]" => "::1".to_owned(),
        _ => host,
    };
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host
    };
    // `new URL(...).toString()` drops the default http port.
    Some(if port == 80 {
        format!("http://{host}/mcp/agents")
    } else {
        format!("http://{host}:{port}/mcp/agents")
    })
}

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Failed to start the async runtime: {error}");
            return ExitCode::from(1);
        }
    };
    let _entered = runtime.enter();
    let env = DaemonEnv::from_process();
    let paseo_home = resolve_paseo_home(&env);
    let persisted = persisted_config(&paseo_home);
    let home = std::env::var("HOME").unwrap_or_default();
    // The transport reads the same file at startup; resolving it first lets
    // workspace provisioning key projects by this daemon's id.
    let server_id = get_or_create_server_id(
        &paseo_home,
        env.get("PASEO_SERVER_ID"),
        &JsonLineLogger::new(std::io::stderr(), Vec::new()),
    );
    // Bootstrap's `setMcpBaseUrl` and `setPaseoToolsEnabled` once listening.
    let inject_mcp =
        mcp_flag(&persisted, "enabled", true) && mcp_flag(&persisted, "injectIntoAgents", false);

    let codex: Arc<dyn AgentClient> = Arc::new(CodexAgentClient::new(
        codex_runtime_settings(&persisted),
        std::env::vars_os().collect(),
    ));
    let storage = Arc::new(AgentStorage::new(paseo_home.join("agents")));
    runtime.block_on(storage.initialize());
    let manager = Arc::new(AgentManager::new(AgentManagerOptions {
        clients: vec![("codex".to_owned(), codex)],
        provider_definitions: vec![(
            "codex".to_owned(),
            ProviderDefinition {
                enabled: true,
                ..ProviderDefinition::default()
            },
        )],
        registry: Some((*storage).clone()),
        mcp_base_url: agent_mcp_base_url(&env, &persisted).filter(|_| inject_mcp),
        paseo_tools_enabled: Some(inject_mcp),
        ..AgentManagerOptions::default()
    }));
    let mut projects = ProjectRegistry::new(paseo_home.join("projects").join("projects.json"));
    let mut workspaces =
        WorkspaceRegistry::new(paseo_home.join("projects").join("workspaces.json"));
    projects.initialize();
    workspaces.initialize();
    let provisioning = Arc::new(WorkspaceProvisioning {
        projects: Mutex::new(projects),
        workspaces: Mutex::new(workspaces),
        server_id: Some(server_id),
        checkout: CheckoutContext {
            paseo_home: paseo_home.to_string_lossy().into_owned(),
            worktrees_root: worktrees_root(&paseo_home, &persisted, &home),
            home,
        },
        on_workspace_created: None,
    });

    // `createAgentProviderRuntime` starts the OpenCode bridge before listening.
    if let Err(error) = materialize_opencode_bridge_plugin(&paseo_home) {
        eprintln!("Failed to write the OpenCode bridge plugin: {error}");
        return ExitCode::from(1);
    }
    // `scheduleService.start()`, also before listening.
    if let Err(error) = ensure_schedule_store_dir(&paseo_home) {
        eprintln!("Failed to create the schedule store: {error}");
        return ExitCode::from(1);
    }

    let backend = Arc::new(DaemonBackend::new(Arc::new(Services {
        runtime: runtime.handle().clone(),
        manager,
        storage,
        provisioning,
        paseo_home,
    })));
    run(backend)
}
