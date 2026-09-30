use serde_json::{Value, json};
use spocky_plugin_pilot::{decode_process_message, decode_process_request};

fn assert_roundtrip(values: &[Value], decode: impl Fn(&str) -> Result<Value, String>) {
    for value in values {
        let encoded = serde_json::to_string(value).expect("encode fixture");
        assert_eq!(decode(&encoded).expect("decode pinned shape"), *value);
    }
}

#[test]
fn every_pinned_host_request_shape_roundtrips_strictly() {
    let requests = vec![
        json!({"type":"initialize","pluginId":"review","bundle":"bundle","appVersion":"0.8.0","pluginDirectory":"/plugins/review","settingsDirectory":"/settings/review"}),
        json!({"type":"provider.catalog_key","requestId":"catalog-1","providerId":"codex","options":{"scope":"global","force":true}}),
        json!({"type":"provider.catalog_key","requestId":"catalog-2","providerId":"codex","options":{"scope":"workspace","cwd":"/work","force":false}}),
        json!({"type":"hook","requestId":"hook-1","kind":"event","name":"session.created","input":{"id":1}}),
        json!({"type":"hook.cancel","requestId":"hook-1"}),
        json!({"type":"usage.identify","requestId":"usage-1","sourceId":"credits","input":{"token":"a"}}),
        json!({"type":"usage.fetch","requestId":"usage-2","sourceId":"credits","input":{"account":"a"}}),
        json!({"type":"usage.discover","requestId":"usage-3","sourceId":"credits"}),
        json!({"type":"invoke","requestId":"rpc-1","method":"review.start","input":{"change":7}}),
        json!({"type":"provider.connect","providerId":"codex","connectionId":"connection-1","request":{"versions":[1,2],"capabilities":["sessions"]}}),
        json!({"type":"provider.send","connectionId":"connection-1","acceptanceId":"accept-1","input":{"type":"sessions","requestId":"sessions-1"}}),
        json!({"type":"provider.close","connectionId":"connection-1"}),
        json!({"type":"shutdown"}),
        json!({"type":"paseo_frame","data":"frame","isBinary":false}),
        json!({"type":"paseo_close"}),
    ];
    assert_roundtrip(&requests, |encoded| {
        decode_process_request(encoded)
            .and_then(|message| serde_json::to_value(message).map_err(|error| error.to_string()))
    });

    assert!(
        decode_process_request(r#"{"type":"invoke","requestId":"","method":"x","input":null}"#)
            .is_err()
    );
    assert!(decode_process_request(r#"{"type":"provider.connect","providerId":"p","connectionId":"c","request":{"versions":[0],"capabilities":[]}}"#).is_err());
    assert!(decode_process_request(r#"{"type":"shutdown","extra":true}"#).is_err());
}

#[test]
fn every_pinned_plugin_message_shape_roundtrips_strictly() {
    let messages = vec![
        json!({"type":"settings.changed","settingsId":"display"}),
        json!({"type":"hooks.changed","hooks":{"events":["session.created"],"before":["session.prompt"]}}),
        json!({"type":"ready","methods":["review.start"],"providers":[{"id":"codex","label":"Codex","description":"Agent","iconPath":"codex.svg","hasCatalogCacheKey":true}],"usageSources":[{"id":"credits","label":"Credits","icon":"credit-card","discover":true}],"hooks":{"events":["session.created"],"before":[]}}),
        json!({"type":"result","requestId":"rpc-1","output":{"accepted":true}}),
        json!({"type":"error","requestId":"rpc-1","error":"failed"}),
        json!({"type":"fatal","error":"broken"}),
        json!({"type":"provider.connected","connectionId":"connection-1","version":2,"capabilities":["sessions"]}),
        json!({"type":"provider.connect_failed","connectionId":"connection-1","error":"denied"}),
        json!({"type":"provider.accepted","connectionId":"connection-1","acceptanceId":"accept-1"}),
        json!({"type":"provider.rejected","connectionId":"connection-1","acceptanceId":"accept-1","error":"invalid"}),
        json!({"type":"provider.event","connectionId":"connection-1","event":{"type":"sessions","requestId":"sessions-1","sessions":[]}}),
        json!({"type":"provider.closed","connectionId":"connection-1","error":"closed"}),
        json!({"type":"paseo_frame","data":"frame","isBinary":false}),
        json!({"type":"paseo_close"}),
    ];
    assert_roundtrip(&messages, |encoded| {
        decode_process_message(encoded)
            .and_then(|message| serde_json::to_value(message).map_err(|error| error.to_string()))
    });

    assert!(
        decode_process_message(
            r#"{"type":"ready","methods":[],"providers":[{"id":"","label":"Codex"}]}"#
        )
        .is_err()
    );
    assert!(
        decode_process_message(
            r#"{"type":"provider.connected","connectionId":"c","version":0,"capabilities":[]}"#
        )
        .is_err()
    );
    assert!(decode_process_message(r#"{"type":"paseo_close","extra":true}"#).is_err());
}
