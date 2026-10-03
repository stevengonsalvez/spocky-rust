//! A decoded binary frame reaches `DaemonSession::binary_frame` and, as in
//! pinned `Session.handleBinaryFrame` (`session.ts:3095`), produces no reply
//! whether or not the session holds `workspace.write`: without it the frame is
//! dropped, with it the frame goes to a file upload or terminal that no
//! request created, which ignores it.

use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use spocky_contracts::ws::DaemonPermission;
use spocky_daemon::binary_frames::{BinaryFrame, decode_binary_frame};
use spocky_daemon::session_api::{SessionBackend, SessionOpen, SessionSink, SocketId};
use spocky_daemon_app::session::{DaemonBackend, Services};
use spocky_daemon_app::workspace_handlers::{ServicesSlot, validate_completed};
use spocky_daemon_app::workspace_label_handlers::WorkspaceLabels;
use spocky_message_receipts::MessageReceipts;
use spocky_session::agent_manager::{AgentManager, AgentManagerOptions};
use spocky_session::agent_storage::AgentStorage;
use spocky_session::checkout::CheckoutContext;
use spocky_session::creation::CreationService;
use spocky_session::provider_snapshot_manager::{
    ProviderSnapshotManager, ProviderSnapshotManagerOptions,
};
use spocky_session::provisioning::WorkspaceProvisioning;
use spocky_store::registry::{ProjectRegistry, WorkspaceRegistry};

/// Every frame the session sends.
#[derive(Default)]
struct Recorder(Mutex<Vec<Value>>);

impl SessionSink for Recorder {
    fn send_to_connection(&self, message: &Value) {
        self.0.lock().unwrap().push(message.clone());
    }
    fn send_to_source(&self, _source: SocketId, message: &Value) {
        self.0.lock().unwrap().push(message.clone());
    }
    fn buffered_amount(&self, _source: Option<SocketId>) -> Option<usize> {
        None
    }
}

fn services(home: &Path, runtime: tokio::runtime::Handle) -> Arc<Services> {
    let storage = Arc::new(AgentStorage::new(home.join("agents")));
    let mut projects = ProjectRegistry::new(home.join("projects").join("projects.json"));
    let mut workspaces = WorkspaceRegistry::new(home.join("projects").join("workspaces.json"));
    projects.initialize();
    workspaces.initialize();
    let slot: ServicesSlot = Arc::new(std::sync::OnceLock::new());
    let creation = CreationService::new(home, Some(validate_completed(Arc::clone(&slot))));
    let provisioning = Arc::new(WorkspaceProvisioning {
        projects: tokio::sync::Mutex::new(projects),
        workspaces: tokio::sync::Mutex::new(workspaces),
        server_id: None,
        checkout: CheckoutContext {
            paseo_home: home.to_string_lossy().into_owned(),
            worktrees_root: Some(home.join("worktrees").to_string_lossy().into_owned()),
            home: home.to_string_lossy().into_owned(),
        },
        on_workspace_created: None,
    });
    let labels = WorkspaceLabels::new(Arc::clone(&provisioning), home);
    Arc::new(Services {
        runtime,
        manager: Arc::new(AgentManager::new(AgentManagerOptions::default())),
        storage,
        provisioning,
        creation,
        snapshots: ProviderSnapshotManager::new(ProviderSnapshotManagerOptions {
            definitions: Vec::new(),
            refresh_timeout_ms: None,
            home: None,
        }),
        receipts: MessageReceipts::new(home.join("agent-requests").to_string_lossy()),
        labels,
        paseo_home: home.to_path_buf(),
        home: home.to_string_lossy().into_owned(),
    })
}

fn frames() -> Vec<BinaryFrame> {
    [
        // The g4-wire fixture's decodable terminal input frame for slot 0.
        &[0x02, 0x00, 0x61][..],
        // A file chunk and a file end for a request no upload registered.
        &[0x11, 0x01, b'r', 0x00, 0x01],
        &[0x12, 0x01, b'r'],
    ]
    .iter()
    .map(|bytes| decode_binary_frame(bytes).expect("a decodable frame"))
    .collect()
}

#[tokio::test]
async fn a_binary_frame_is_never_answered_with_or_without_workspace_write() {
    let home = std::env::temp_dir().join(format!("spocky-binary-frame-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let backend = DaemonBackend::new(services(&home, tokio::runtime::Handle::current()));
    for permissions in [
        vec![DaemonPermission::WorkspaceRead],
        vec![DaemonPermission::WorkspaceWrite],
        DaemonPermission::ALL.to_vec(),
    ] {
        let sink = Arc::new(Recorder::default());
        let session = backend.open(SessionOpen {
            client_id: "c".to_owned(),
            app_version: None,
            client_capabilities: None,
            permissions: permissions.clone(),
            sink: Arc::clone(&sink) as Arc<dyn SessionSink>,
        });
        for frame in frames() {
            assert_eq!(session.binary_frame(frame, 1), Ok(()), "{permissions:?}");
        }
        assert!(
            sink.0.lock().unwrap().is_empty(),
            "no frame was sent for {permissions:?}"
        );
        session.cleanup();
    }
    std::fs::remove_dir_all(&home).unwrap();
}
