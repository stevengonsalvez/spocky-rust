//! Starting and stopping the daemon process: home, config, lock, identity,
//! listener.
//!
//! Sources at Paseo `5de45e2`: `scripts/supervisor-entrypoint.ts` (home, config
//! and lock order, heartbeat, lock updates), `daemon-worker.ts`, `bootstrap.ts`
//! (`createPaseoDaemon`: server id, key pair, credential, listen, origins) and
//! `config.ts`. The baseline runs a supervisor and a worker process; this is one
//! process that does both jobs in the same order.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::net::{TcpListener, ToSocketAddrs};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::admission::PasswordVerifier;
use crate::config_file::{first_present, load_persisted_config};
use crate::daemon_keypair::load_or_create_daemon_key_pair;
use crate::hostnames::{merge_hostnames, parse_hostnames_env};
use crate::js;
use crate::listen::{
    ListenTarget, format_listen_target, parse_listen_string, resolve_listen_address,
};
use crate::local_credential::{
    delete_local_credential, read_local_credential, write_local_credential,
};
use crate::log::Logger;
use crate::pid_lock::{
    AcquireOptions, HeartbeatHandle, PID_LOCK_HEARTBEAT_INTERVAL, PidLockError, PidLockPatch,
    acquire_pid_lock, release_pid_lock, start_pid_lock_heartbeat, update_pid_lock,
};
use crate::server::{ListenHandle, Server, ServerConfig, ServerDeps, Timeouts};
use crate::server_id::get_or_create_server_id;
use crate::session_api::{ProtocolFailure, SessionBackend, SessionHandle, SessionOpen, SocketId};
use spocky_contracts::text::JsText;
use spocky_contracts::ws::{
    DaemonPermission, ServerCapabilities, ServerCapabilityState, ServerId, ServerVoiceCapabilities,
};

/// `@getpaseo/server` version at the pinned commit; reported in `server_info`.
pub const DAEMON_VERSION: &str = "0.10.0";

/// The process inputs the daemon reads: environment, working directory, home.
#[derive(Clone)]
pub struct DaemonEnv {
    vars: HashMap<String, String>,
    cwd: PathBuf,
    home_dir: Option<PathBuf>,
}

/// The environment can hold `PASEO_PASSWORD`: Debug shows names, never values.
impl std::fmt::Debug for DaemonEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut names: Vec<&String> = self.vars.keys().collect();
        names.sort();
        f.debug_struct("DaemonEnv")
            .field("var_names", &names)
            .field("cwd", &self.cwd)
            .field("home_dir", &self.home_dir)
            .finish()
    }
}

impl DaemonEnv {
    #[must_use]
    pub fn new(vars: HashMap<String, String>, cwd: PathBuf, home_dir: Option<PathBuf>) -> Self {
        Self {
            vars,
            cwd,
            home_dir,
        }
    }

    /// The real process environment.
    #[must_use]
    pub fn from_process() -> Self {
        let vars: HashMap<String, String> = std::env::vars().collect();
        let home_dir = vars.get("HOME").map(PathBuf::from);
        Self::new(vars, std::env::current_dir().unwrap_or_default(), home_dir)
    }

    /// The value of environment variable `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }
}

/// A startup failure; the message goes to stderr and the exit code is 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupError(pub String);

impl std::fmt::Display for StartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StartupError {}

/// `path.resolve(expandHomeDir(PASEO_HOME ?? "~/.paseo"))`.
#[must_use]
pub fn resolve_paseo_home(env: &DaemonEnv) -> PathBuf {
    let raw = env.get("PASEO_HOME").unwrap_or("~/.paseo");
    let expanded = if raw == "~" {
        env.home_dir.clone().unwrap_or_default()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        env.home_dir.clone().unwrap_or_default().join(rest)
    } else {
        PathBuf::from(raw)
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        env.cwd.join(expanded)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => resolved.push(other.as_os_str()),
        }
    }
    resolved
}

/// Placeholder password check for a daemon that cannot verify bcrypt hashes:
/// nothing verifies. Startup refuses a configured password, so this is only the
/// fail-closed default.
pub struct DenyAllVerifier;

impl PasswordVerifier for DenyAllVerifier {
    fn verify(&self, _password: &str, _password_hash: &str) -> bool {
        false
    }
}

/// The session layer used until `spocky-session` is wired in: it accepts a
/// hello and answers nothing else.
pub struct NoSessionBackend;

struct NoSession;

impl SessionHandle for NoSession {
    fn session_id(&self) -> String {
        String::new()
    }
    fn permissions(&self) -> Vec<DaemonPermission> {
        DaemonPermission::ALL.to_vec()
    }
    fn update_client_capabilities(&self, _: Option<&Value>, _: SocketId, _: Option<&str>) {}
    fn update_app_version(&self, _: &str) {}
    fn handle_message(&self, _: Value, _: SocketId) {}
    fn protocol_failure(&self, _: SocketId, _: ProtocolFailure) {}
    fn socket_detached(&self, _: SocketId) {}
    fn cleanup(&self) {}
}

impl SessionBackend for NoSessionBackend {
    fn open(&self, _open: SessionOpen) -> Arc<dyn SessionHandle> {
        Arc::new(NoSession)
    }
    fn validate_inbound(&self, _message: &Value) -> Result<(), String> {
        Err("session requests are not available".to_owned())
    }
}

/// A started daemon.
pub struct RunningDaemon {
    server: Server,
    listener: Option<ListenHandle>,
    paseo_home: PathBuf,
    listen: String,
    server_id: ServerId,
    unix_socket: Option<PathBuf>,
    heartbeat: Option<HeartbeatHandle>,
    shutdown_requested: Arc<AtomicBool>,
    logger: Arc<dyn Logger>,
    backend: Arc<dyn SessionBackend>,
}

fn fail(message: impl Into<String>) -> StartupError {
    StartupError(message.into())
}

/// Binds the configured target. A TCP port is checked here, as Node checks it
/// in `listen`.
fn bind(
    target: &ListenTarget,
    server: &Server,
) -> Result<(ListenHandle, ListenTarget, Option<PathBuf>), StartupError> {
    match target {
        ListenTarget::Tcp { host, port } => {
            let port_number = u16::try_from(*port).map_err(|_| {
                fail(format!(
                    "options.port should be >= 0 and < 65536. Received type number ({port})"
                ))
            })?;
            let address = (host.as_str(), port_number)
                .to_socket_addrs()
                .map_err(|error| fail(format!("listen ENOTFOUND {host}: {error}")))?
                .next()
                .ok_or_else(|| fail(format!("listen ENOTFOUND {host}")))?;
            let listener = TcpListener::bind(address).map_err(|error| {
                fail(format!(
                    "listen {} {host}:{port}: {error}",
                    errno_name(&error)
                ))
            })?;
            let handle = server
                .serve_tcp(listener)
                .map_err(|error| fail(error.to_string()))?;
            let bound_port = handle
                .local_addr()
                .map_or(*port, |address| i64::from(address.port()));
            Ok((
                handle,
                ListenTarget::Tcp {
                    host: host.clone(),
                    port: bound_port,
                },
                None,
            ))
        }
        #[cfg(unix)]
        ListenTarget::Socket { path } => {
            if Path::new(path).exists() {
                fs::remove_file(path).map_err(|error| fail(error.to_string()))?;
            }
            let listener = std::os::unix::net::UnixListener::bind(path)
                .map_err(|error| fail(format!("listen {} {path}: {error}", errno_name(&error))))?;
            let handle = server
                .serve_unix(listener)
                .map_err(|error| fail(error.to_string()))?;
            Ok((handle, target.clone(), Some(PathBuf::from(path))))
        }
        #[cfg(not(unix))]
        ListenTarget::Socket { .. } => Err(fail("Unix sockets are not supported on this platform")),
        ListenTarget::Pipe { .. } => Err(fail("Named pipes are not supported on this platform")),
    }
}

fn errno_name(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::AddrInUse => "EADDRINUSE",
        io::ErrorKind::AddrNotAvailable => "EADDRNOTAVAIL",
        io::ErrorKind::PermissionDenied => "EACCES",
        _ => "EUNKNOWN",
    }
}

/// `fixedAllowedOrigins` plus the configured ones.
fn allowed_origins(
    configured: &[String],
    env_cors: Option<&str>,
    target: &ListenTarget,
) -> HashSet<String> {
    let from_env = env_cors
        .map(|raw| {
            raw.split(',')
                .map(|part| js::trim(part).to_owned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut origins: HashSet<String> = configured
        .iter()
        .cloned()
        .chain(from_env)
        .filter(|origin| !origin.is_empty())
        .collect();
    origins.insert("paseo://app".to_owned());
    if let ListenTarget::Tcp { host, port } = target {
        origins.insert(format!("http://{host}:{port}"));
        origins.insert(format!("http://localhost:{port}"));
        origins.insert(format!("http://127.0.0.1:{port}"));
    }
    origins
}

/// Starts the daemon: config, PID lock, identity, credential, listener, then the
/// lock update that publishes `listen` and `serverId`.
///
/// # Errors
///
/// A [`StartupError`] with the text the baseline writes to stderr before it
/// exits 1.
pub fn start(
    env: &DaemonEnv,
    backend: Arc<dyn SessionBackend>,
    logger: &Arc<dyn Logger>,
) -> Result<RunningDaemon, StartupError> {
    start_with(env, backend, logger, &|home, patch| {
        update_pid_lock(home, patch, None)
    })
}

/// How the daemon publishes `listen` and `serverId` in `paseo.pid`.
pub type LockPublisher<'a> = &'a dyn Fn(&Path, &PidLockPatch) -> Result<(), PidLockError>;

/// [`start`] with the lock publication supplied, so a failure of that last step
/// can be exercised.
///
/// # Errors
///
/// As [`start`].
pub fn start_with(
    env: &DaemonEnv,
    backend: Arc<dyn SessionBackend>,
    logger: &Arc<dyn Logger>,
    publish: LockPublisher<'_>,
) -> Result<RunningDaemon, StartupError> {
    let paseo_home = resolve_paseo_home(env);
    let persisted = load_persisted_config(&paseo_home, logger.as_ref())
        .map_err(|error| fail(error.to_string()))?;

    // A password refuses the start, whether it comes from the config (already
    // checked to be a bcrypt hash) or from the environment. `PASEO_PASSWORD` is
    // trimmed and a blank value counts as unset, as in `resolveAuthConfig`. An
    // empty `daemon.auth.password` never reaches here: the config load rejects it.
    let env_password = env
        .get("PASEO_PASSWORD")
        .map(js::trim)
        .filter(|p| !p.is_empty());
    if persisted.auth_password.is_some() || env_password.is_some() {
        return Err(fail(
            "A daemon password is configured, but spocky-daemon cannot verify bcrypt password hashes yet; refusing to start",
        ));
    }

    let desktop_managed = env.get("PASEO_DESKTOP_MANAGED") == Some("1");
    acquire_pid_lock(
        &paseo_home,
        None,
        AcquireOptions {
            owner_pid: None,
            desktop_managed,
        },
    )
    .map_err(|error| fail(error.to_string()))?;

    match start_after_lock(
        env,
        &paseo_home,
        &persisted,
        desktop_managed,
        backend,
        logger,
        publish,
    ) {
        Ok(daemon) => Ok(daemon),
        Err(error) => {
            release_pid_lock(&paseo_home, None, None);
            Err(error)
        }
    }
}

/// `resolveListenAddress` then `parseListenString`, with the production-port guard.
fn resolve_target(
    env: &DaemonEnv,
    persisted: &crate::config_file::PersistedDaemonConfig,
) -> Result<ListenTarget, StartupError> {
    let listen_text = resolve_listen_address(
        None,
        env.get("PASEO_LISTEN"),
        persisted.listen.as_deref(),
        env.get("PORT"),
    );
    let target = parse_listen_string(&listen_text).map_err(|error| fail(error.to_string()))?;
    Ok(target)
}

/// `resolveOptionalBooleanFlag(firstSpeechDefinedValue([env, persisted]))`: a
/// defined environment string wins over the persisted boolean, a string is
/// trimmed and lower-cased, and anything unrecognized or absent is `true`.
fn speech_flag(environment: Option<&str>, persisted: Option<bool>) -> bool {
    if let Some(text) = environment {
        // Only the false words switch it off; any other text reads as unset.
        return !matches!(
            js::trim(text).to_lowercase().as_str(),
            "0" | "false" | "no" | "n" | "off"
        );
    }
    persisted.unwrap_or(true)
}

/// `buildServerCapabilities` for a daemon whose speech runtime is switched off in
/// config: both capabilities report why they are disabled. With either feature
/// on, the baseline reports the speech runtime's readiness (models, download
/// state), which this daemon does not have; capabilities are then omitted.
fn speech_capabilities(
    env: &DaemonEnv,
    persisted: &crate::config_file::PersistedDaemonConfig,
) -> Option<ServerCapabilities> {
    let dictation = speech_flag(
        env.get("PASEO_DICTATION_ENABLED"),
        persisted.dictation_enabled,
    );
    let voice = speech_flag(
        env.get("PASEO_VOICE_MODE_ENABLED"),
        persisted.voice_mode_enabled,
    );
    if dictation || voice {
        return None;
    }
    Some(ServerCapabilities {
        voice: ServerVoiceCapabilities {
            dictation: ServerCapabilityState {
                enabled: false,
                reason: JsText::new("Dictation is disabled in daemon config."),
            },
            voice: ServerCapabilityState {
                enabled: false,
                reason: JsText::new("Realtime voice is disabled in daemon config."),
            },
        },
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "one port of the bootstrap start sequence, kept in pinned order"
)]
fn start_after_lock(
    env: &DaemonEnv,
    paseo_home: &Path,
    persisted: &crate::config_file::PersistedDaemonConfig,
    desktop_managed: bool,
    backend: Arc<dyn SessionBackend>,
    logger: &Arc<dyn Logger>,
    publish: LockPublisher<'_>,
) -> Result<RunningDaemon, StartupError> {
    let shutdown_requested = Arc::new(AtomicBool::new(false));
    let heartbeat_flag = Arc::clone(&shutdown_requested);
    let heartbeat = start_pid_lock_heartbeat(
        paseo_home.to_path_buf(),
        None,
        PID_LOCK_HEARTBEAT_INTERVAL,
        Some(Box::new(move |error| {
            eprintln!("PID lock heartbeat failed: {error}");
            if matches!(error, PidLockError::Lock { .. }) {
                heartbeat_flag.store(true, Ordering::SeqCst);
            }
        })),
    )
    .map_err(|error| fail(format!("Failed to start the PID lock heartbeat: {error}")))?;

    let target = resolve_target(env, persisted)?;

    // `serverId` is `z.string().trim().min(1)` on the wire; the id is already
    // trimmed and non-empty, and a blank one is refused here rather than sent.
    let server_id = ServerId::new(&get_or_create_server_id(
        paseo_home,
        env.get("PASEO_SERVER_ID"),
        logger.as_ref(),
    ))
    .ok_or_else(|| fail("The server id is blank"))?;
    load_or_create_daemon_key_pair(paseo_home, logger.as_ref())
        .map_err(|error| fail(error.to_string()))?;

    let hostnames = merge_hostnames(&[
        persisted.hostnames.clone(),
        parse_hostnames_env(first_present(&[
            env.get("PASEO_HOSTNAMES"),
            env.get("PASEO_ALLOWED_HOSTS"),
        ])),
    ]);
    let local_credential: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let credential_reader = Arc::clone(&local_credential);
    let server = Server::new(
        ServerConfig {
            server_id: server_id.clone(),
            daemon_version: DAEMON_VERSION.to_owned(),
            hostname: gethostname::gethostname().to_string_lossy().into_owned(),
            hostnames: Some(hostnames),
            allowed_origins: allowed_origins(
                &persisted.cors_allowed_origins,
                env.get("PASEO_CORS_ORIGINS"),
                &target,
            ),
            password_hash: None,
            desktop_managed,
            workspace_labels: false,
            advertise_daemon_status_rpc: true,
            advertise_relay_config: true,
            start_paused: false,
            capabilities: speech_capabilities(env, persisted),
            timeouts: Timeouts::default(),
        },
        ServerDeps {
            backend: Arc::clone(&backend),
            verifier: Arc::new(DenyAllVerifier),
            local_credential: Arc::new(move || {
                credential_reader
                    .lock()
                    .ok()
                    .and_then(|token| token.clone())
            }),
            logger: Arc::clone(logger),
        },
    );

    let token = write_local_credential(paseo_home).map_err(|error| fail(error.to_string()))?;
    *local_credential
        .lock()
        .map_err(|_| fail("credential lock poisoned"))? = Some(token);

    let bound = bind(&target, &server);
    let (listener, bound_target, unix_socket) = match bound {
        Ok(bound) => bound,
        Err(error) => {
            let _ = delete_local_credential(paseo_home);
            server.close();
            return Err(error);
        }
    };
    let listen = format_listen_target(&bound_target);
    server.set_listen(&listen, matches!(bound_target, ListenTarget::Tcp { .. }));
    backend.listening(&bound_target);
    logger.info(&[("listen", &listen)], "Server listening");

    let patch = PidLockPatch::Listening {
        listen: listen.clone(),
        server_id: server_id.as_str().to_owned(),
    };
    let daemon = RunningDaemon {
        server,
        listener: Some(listener),
        paseo_home: paseo_home.to_path_buf(),
        listen,
        server_id,
        unix_socket,
        heartbeat: Some(heartbeat),
        shutdown_requested,
        logger: Arc::clone(logger),
        backend,
    };
    if let Err(error) = publish(paseo_home, &patch) {
        // Listening but unpublished: undo everything that was started.
        daemon.stop();
        return Err(fail(error.to_string()));
    }
    Ok(daemon)
}

impl RunningDaemon {
    /// `formatListenTarget(boundListenTarget)`.
    #[must_use]
    pub fn listen(&self) -> &str {
        &self.listen
    }

    #[must_use]
    pub fn server_id(&self) -> &str {
        self.server_id.as_str()
    }

    /// The session transport, for tests that need to reach it.
    #[must_use]
    pub fn server(&self) -> &Server {
        &self.server
    }

    /// Set when the daemon must stop on its own (the PID lock was lost).
    #[must_use]
    pub fn shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::SeqCst)
    }

    /// `daemon.stop()` then the supervisor's exit steps: remove the credential,
    /// freeze ingress, stop the backend's agents, close the server, clear
    /// `listen` in the lock, release the lock.
    pub fn stop(mut self) {
        if let Err(error) = delete_local_credential(&self.paseo_home) {
            self.logger.warn(
                &[("err", &error.to_string())],
                "Failed to delete local credential",
            );
        }
        self.server.prepare_for_shutdown();
        self.backend.stop_agents();
        self.server.close();
        if let Some(listener) = self.listener.take() {
            listener.stop();
        }
        if let Some(path) = &self.unix_socket {
            let _ = fs::remove_file(path);
        }
        self.logger.info(&[], "Server closed");
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.stop();
        }
        let _ = update_pid_lock(&self.paseo_home, &PidLockPatch::Cleared, None);
        release_pid_lock(&self.paseo_home, None, None);
    }

    /// Whether the local credential file currently holds a token.
    #[must_use]
    pub fn has_local_credential(&self) -> bool {
        read_local_credential(&self.paseo_home).is_some()
    }
}

/// How long the baseline waits for a graceful stop before it exits 1.
pub const FORCE_EXIT_AFTER: Duration = Duration::from_secs(10);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_file::PersistedDaemonConfig;

    #[test]
    fn speech_flags_follow_the_baseline_boolean_parsing() {
        // Environment text wins over the persisted value, once trimmed and lower-cased.
        assert!(!speech_flag(Some(" OFF "), Some(true)));
        assert!(!speech_flag(Some("0"), None));
        assert!(speech_flag(Some("Yes"), Some(false)));
        // Text it does not recognize, even empty, is unset and so true.
        assert!(speech_flag(Some(""), Some(false)));
        assert!(speech_flag(Some("maybe"), Some(false)));
        // No environment value: the persisted one, defaulting to true.
        assert!(!speech_flag(None, Some(false)));
        assert!(speech_flag(None, None));
    }

    #[test]
    fn capabilities_are_reported_only_when_both_speech_features_are_off() {
        let env = |vars: &[(&str, &str)]| {
            DaemonEnv::new(
                vars.iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect(),
                PathBuf::from("/"),
                None,
            )
        };
        let off = PersistedDaemonConfig {
            dictation_enabled: Some(false),
            voice_mode_enabled: Some(false),
            ..PersistedDaemonConfig::default()
        };
        assert!(speech_capabilities(&env(&[]), &off).is_some());
        assert!(speech_capabilities(&env(&[]), &PersistedDaemonConfig::default()).is_none());
        let dictation_only = PersistedDaemonConfig {
            dictation_enabled: Some(false),
            ..PersistedDaemonConfig::default()
        };
        assert!(speech_capabilities(&env(&[]), &dictation_only).is_none());
        // The environment turns a feature back on.
        assert!(speech_capabilities(&env(&[("PASEO_VOICE_MODE_ENABLED", "true")]), &off).is_none());
        assert!(
            speech_capabilities(
                &env(&[
                    ("PASEO_DICTATION_ENABLED", "no"),
                    ("PASEO_VOICE_MODE_ENABLED", "off")
                ]),
                &PersistedDaemonConfig::default()
            )
            .is_some()
        );
    }
}
