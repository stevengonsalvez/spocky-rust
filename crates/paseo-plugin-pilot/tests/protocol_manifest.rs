use std::fs;

use paseo_plugin_pilot::{
    PluginCatalogEntry, PluginCatalogGetRequest, PluginCatalogGetResponse, PluginId,
    PluginRpcInvokeRequest, PluginRpcInvokeResponse, PluginStatus, PluginStatusPayload,
    load_manifest,
};
use serde_json::json;

#[test]
fn manifest_matches_pinned_paseo_shape_and_infers_entries() {
    let root = tempfile_directory("manifest");
    fs::write(
        root.join("paseo-plugin.json"),
        r#"{
          "id":"review",
          "description":"Reviews changes",
          "requirements":{"paseo":">=0.8.0"},
          "build":[["npm","ci","--omit=dev"]]
        }"#,
    )
    .expect("write manifest");
    fs::write(
        root.join("index.client.ts"),
        "export default () => () => {};",
    )
    .expect("write client entry");
    fs::write(root.join("index.server.ts"), "process.exit(0);").expect("write server entry");

    let loaded = load_manifest(&root).expect("load manifest");
    assert_eq!(loaded.manifest.id.as_str(), "review");
    assert_eq!(
        loaded.manifest.description.as_deref(),
        Some("Reviews changes")
    );
    assert_eq!(
        loaded.manifest.requirements.paseo.as_deref(),
        Some(">=0.8.0")
    );
    assert_eq!(
        loaded.manifest.build,
        vec![vec![
            "npm".to_owned(),
            "ci".to_owned(),
            "--omit=dev".to_owned()
        ]]
    );
    assert_eq!(
        loaded.client_entry.as_deref(),
        Some(root.join("index.client.ts").as_path())
    );
    assert_eq!(
        loaded.server_entry.as_deref(),
        Some(root.join("index.server.ts").as_path())
    );

    fs::write(
        root.join("paseo-plugin.json"),
        r#"{"id":"review","server":"runtime.js"}"#,
    )
    .expect("write legacy manifest");
    assert!(load_manifest(&root).is_err());
    fs::write(root.join("paseo-plugin.json"), r#"{"id":"Bad_Id"}"#)
        .expect("write invalid id manifest");
    assert!(load_manifest(&root).is_err());
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn client_protocol_serializes_exact_pinned_message_shapes() {
    let pinned: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/pinned_protocol.json"))
            .expect("pinned protocol capture");
    let request = PluginCatalogGetRequest {
        request_id: "catalog-1".into(),
    };
    assert_eq!(
        serde_json::to_value(request).expect("catalog request"),
        pinned["catalogRequest"]
    );

    let response = PluginCatalogGetResponse {
        request_id: "catalog-1".into(),
        plugins: vec![PluginCatalogEntry {
            id: PluginId::new("review").expect("plugin id"),
            client_bundle: "bundle".into(),
            paseo_requirement: Some(">=0.8.0".into()),
        }],
    };
    assert_eq!(
        serde_json::to_value(response).expect("catalog response"),
        pinned["catalogResponse"]
    );

    let invoke = PluginRpcInvokeRequest {
        request_id: "rpc-1".into(),
        plugin_id: PluginId::new("review").expect("plugin id"),
        method: "review.start".into(),
        input: json!({"change":7}),
    };
    assert_eq!(
        serde_json::to_value(invoke).expect("invoke request"),
        pinned["invokeRequest"]
    );
    assert_eq!(
        serde_json::to_value(PluginRpcInvokeResponse {
            request_id: "rpc-1".into(),
            output: json!({"accepted":true}),
        })
        .expect("invoke response"),
        pinned["invokeResponse"]
    );
    assert_eq!(
        serde_json::to_value(PluginStatus {
            payload: PluginStatusPayload::SettingsChanged {
                plugin_id: PluginId::new("review").expect("plugin id"),
                settings_id: "display".into(),
            },
        })
        .expect("status"),
        pinned["settingsChanged"]
    );
}

fn tempfile_directory(label: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "paseo-plugin-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&path).expect("create fixture");
    path
}
