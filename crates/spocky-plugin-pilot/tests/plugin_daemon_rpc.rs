use serde_json::{Value, json};
use spocky_plugin_pilot::{
    PluginDaemonRequest, PluginDaemonResponse, PluginNotification, PluginRpcError,
};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/plugin_daemon_rpc_cases.json"))
        .expect("parse daemon RPC fixture")
}

#[test]
fn all_plugin_daemon_messages_roundtrip_with_exact_shapes() {
    let fixture = fixture();
    let requests: Vec<PluginDaemonRequest> =
        serde_json::from_value(fixture["requests"].clone()).expect("parse requests");
    let responses: Vec<PluginDaemonResponse> =
        serde_json::from_value(fixture["responses"].clone()).expect("parse responses");
    let errors: Vec<PluginRpcError> =
        serde_json::from_value(fixture["errors"].clone()).expect("parse errors");
    let notifications: Vec<PluginNotification> =
        serde_json::from_value(fixture["notifications"].clone()).expect("parse notifications");
    let rejections = fixture["rejectedRequests"]
        .as_array()
        .expect("rejected requests")
        .iter()
        .map(|value| serde_json::from_value::<PluginDaemonRequest>(value.clone()).is_err())
        .chain(
            fixture["rejectedResponses"]
                .as_array()
                .expect("rejected responses")
                .iter()
                .map(|value| {
                    serde_json::from_value::<PluginDaemonResponse>(value.clone()).is_err()
                }),
        )
        .collect::<Vec<_>>();

    assert_eq!(
        serde_json::to_value(&requests).expect("requests"),
        fixture["requests"]
    );
    assert_eq!(
        serde_json::to_value(&responses).expect("responses"),
        fixture["responses"]
    );
    assert_eq!(
        serde_json::to_value(&errors).expect("errors"),
        fixture["errors"]
    );
    assert_eq!(
        serde_json::to_value(&notifications).expect("notifications"),
        fixture["notifications"]
    );

    println!(
        "PLUGIN_DAEMON_RPC_RUST {}",
        serde_json::to_string(&json!({
            "requests": requests,
            "responses": responses,
            "errors": errors,
            "notifications": notifications,
            "rejections": rejections,
        }))
        .expect("serialize capture")
    );
}

#[test]
fn correlated_errors_match_handler_and_authorization_contracts() {
    assert_eq!(
        serde_json::to_value(PluginRpcError::handler(
            "invoke-failed",
            "plugin.rpc.invoke.request",
            "Unknown plugin RPC method: missing",
        ))
        .expect("handler error"),
        fixture()["errors"][0]
    );
    assert_eq!(
        serde_json::to_value(PluginRpcError::access_denied(
            "remove-denied",
            "plugin.remove.request",
        ))
        .expect("access denied"),
        fixture()["errors"][1]
    );
}

#[test]
fn optional_wire_fields_distinguish_missing_from_null() {
    assert!(
        serde_json::from_value::<PluginDaemonRequest>(json!({
            "type": "plugin.directory.install.request",
            "requestId": "install",
            "path": "/tmp/review"
        }))
        .is_ok()
    );
    assert!(
        serde_json::from_value::<PluginDaemonRequest>(json!({
            "type": "plugin.directory.install.request",
            "requestId": "install",
            "path": "/tmp/review",
            "id": null
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<PluginDaemonResponse>(json!({
            "type": "plugin.list.response",
            "payload": {
                "requestId": "list",
                "plugins": [{
                    "id": "review",
                    "description": null,
                    "path": "/tmp/review",
                    "enabled": true,
                    "status": "running"
                }]
            }
        }))
        .is_err()
    );
}
