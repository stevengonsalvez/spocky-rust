//! The `server_info` frame sent after an accepted hello.
//!
//! Source at Paseo `5de45e2`: `buildServerInfoStatusPayload` and
//! `createServerInfoMessage` in `websocket-server.ts`. The payload types live in
//! `spocky-contracts`; this module only fills them from the daemon's state.

use serde_json::{Map, Value};
use spocky_contracts::number::Int;
use spocky_contracts::text::JsText;
use spocky_contracts::ws::{
    DaemonPermission, ServerCapabilities, ServerFeatureGates, ServerFeatures, ServerId, ServerInfo,
    WS_PROTOCOL_VERSION,
};

/// What the daemon knows when it builds the payload.
#[derive(Debug, Clone)]
pub struct ServerInfoInputs {
    pub server_id: ServerId,
    pub hostname: String,
    pub version: String,
    pub permissions: Vec<DaemonPermission>,
    pub desktop_managed: bool,
    /// `buildServerCapabilities` from the speech readiness; `None` omits the key.
    pub capabilities: Option<ServerCapabilities>,
    pub gates: ServerFeatureGates,
}

/// `buildServerInfoStatusPayload`: `status` first, then the fields in
/// construction order.
///
/// # Panics
///
/// Never in practice: the payload types always serialize to an object.
#[must_use]
pub fn server_info_payload(inputs: &ServerInfoInputs) -> Value {
    let info = ServerInfo {
        protocol_version: Int::new(WS_PROTOCOL_VERSION)
            .expect("protocol version is a safe integer"),
        server_id: inputs.server_id.clone(),
        hostname: JsText::new(&inputs.hostname),
        version: JsText::new(&inputs.version),
        permissions: inputs.permissions.clone(),
        desktop_managed: inputs.desktop_managed,
        capabilities: inputs.capabilities.clone(),
        features: ServerFeatures::advertised(inputs.gates),
    };
    let Value::Object(fields) = serde_json::to_value(info).expect("server info serializes") else {
        unreachable!("ServerInfo serializes to an object");
    };
    let mut payload = Map::new();
    payload.insert("status".to_owned(), Value::from("server_info"));
    payload.extend(fields);
    Value::Object(payload)
}

/// `createServerInfoMessage`: the payload as a `status` session message. The
/// transport wraps it in the session envelope when it sends it.
#[must_use]
pub fn server_info_message(inputs: &ServerInfoInputs) -> Value {
    let mut message = Map::new();
    message.insert("type".to_owned(), Value::from("status"));
    message.insert("payload".to_owned(), server_info_payload(inputs));
    Value::Object(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> ServerInfoInputs {
        ServerInfoInputs {
            server_id: ServerId::new("srv_test").unwrap(),
            hostname: "box".to_owned(),
            version: "0.10.0".to_owned(),
            permissions: DaemonPermission::ALL.to_vec(),
            desktop_managed: false,
            capabilities: None,
            gates: ServerFeatureGates {
                workspace_labels: false,
                daemon_status_rpc: true,
                relay_config: true,
                desktop_managed: false,
            },
        }
    }

    #[test]
    fn the_payload_keeps_the_construction_order() {
        let payload = server_info_payload(&inputs());
        let keys: Vec<_> = payload
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "status",
                "protocolVersion",
                "serverId",
                "hostname",
                "version",
                "permissions",
                "desktopManaged",
                "features"
            ]
        );
        assert_eq!(payload["status"], "server_info");
        assert_eq!(payload["protocolVersion"], 1);
        assert_eq!(payload["serverId"], "srv_test");
        assert_eq!(payload["desktopManaged"], false);
    }

    #[test]
    fn owner_permissions_are_listed_in_declaration_order() {
        let payload = server_info_payload(&inputs());
        assert_eq!(
            payload["permissions"],
            serde_json::json!([
                "daemon.read",
                "daemon.manage",
                "tunnel.manage",
                "access.manage",
                "workspace.read",
                "workspace.write",
                "workspace.manage",
                "automation.manage",
                "hub.execute"
            ])
        );
    }

    #[test]
    fn features_follow_the_gates() {
        let mut input = inputs();
        input.gates.workspace_labels = true;
        input.gates.desktop_managed = true;
        input.gates.daemon_status_rpc = false;
        let features = &server_info_payload(&input)["features"];
        assert_eq!(features["workspaceLabels"], true);
        assert_eq!(features["daemonSelfUpdate"], false);
        assert!(features.get("daemonStatusRpc").is_none());
        assert_eq!(features["relayConfig"], true);
    }

    #[test]
    fn capabilities_are_placed_before_features_when_present() {
        use spocky_contracts::ws::{ServerCapabilityState, ServerVoiceCapabilities};
        let mut input = inputs();
        let state = |enabled| ServerCapabilityState {
            enabled,
            reason: JsText::new(""),
        };
        input.capabilities = Some(ServerCapabilities {
            voice: ServerVoiceCapabilities {
                dictation: state(false),
                voice: state(true),
            },
        });
        let payload = server_info_payload(&input);
        let keys: Vec<_> = payload
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(&keys[6..], ["desktopManaged", "capabilities", "features"]);
    }

    #[test]
    fn the_message_is_a_status_with_the_payload() {
        let message = server_info_message(&inputs());
        assert_eq!(message["type"], "status");
        assert_eq!(message["payload"]["status"], "server_info");
    }
}
