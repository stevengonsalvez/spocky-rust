use paseo_plugin_pilot::{decode_process_message, decode_process_request};
use serde_json::{Value, json};

fn request(input: &Value) -> String {
    json!({
        "type": "provider.send",
        "connectionId": "connection-1",
        "acceptanceId": "acceptance-1",
        "input": input,
    })
    .to_string()
}

fn event(event: &Value) -> String {
    json!({
        "type": "provider.event",
        "connectionId": "connection-1",
        "event": event,
    })
    .to_string()
}

#[test]
fn every_provider_input_variant_roundtrips() {
    let inputs = [
        json!({"type":"catalog","requestId":"r","cwd":"/work"}),
        json!({"type":"sessions","requestId":"r","query":"q","cwd":"/work","limit":5}),
        json!({"type":"session.open","requestId":"r","sessionId":"s","config":{},"persistence":{"version":0,"data":null},"history":"replay"}),
        json!({"type":"session.prompt","sessionId":"s","prompt":{"clientMessageId":"m","delivery":"auto","input":{"type":"message","content":[]}}}),
        json!({"type":"session.interrupt","requestId":"r","sessionId":"s"}),
        json!({"type":"session.usage_reference","requestId":"r","sessionId":"s"}),
        json!({"type":"session.permission","sessionId":"s","permissionId":"p","response":{"behavior":"deny"}}),
        json!({"type":"session.configure","requestId":"r","sessionId":"s","changes":{}}),
        json!({"type":"session.revert","requestId":"r","sessionId":"s","token":null,"scope":"both"}),
        json!({"type":"session.archive","requestId":"r","persistence":{"version":0,"data":null}}),
        json!({"type":"session.unarchive","requestId":"r","persistence":{"version":0,"data":null}}),
        json!({"type":"session.close","requestId":"r","sessionId":"s"}),
    ];

    for input in inputs {
        decode_process_request(&request(&input)).expect("valid provider input");
    }
}

#[test]
fn provider_input_rejects_pinned_boundary_violations() {
    for input in [
        json!({"type":"catalog","requestId":"","cwd":"/work"}),
        json!({"type":"sessions","requestId":"r","limit":0}),
        json!({"type":"session.open","requestId":"r","sessionId":"s","config":{},"history":"restore"}),
        json!({"type":"session.revert","requestId":"r","sessionId":"s","token":null,"scope":"workspace"}),
        json!({"type":"session.close","requestId":"r","sessionId":"s","extra":true}),
        json!({"type":"unknown","requestId":"r"}),
    ] {
        assert!(decode_process_request(&request(&input)).is_err());
    }
}

#[test]
fn every_provider_event_variant_roundtrips() {
    let events = [
        json!({"type":"catalog","requestId":"r","catalog":{"models":[],"modes":[]}}),
        json!({"type":"sessions","requestId":"r","sessions":[]}),
        json!({"type":"request.completed","requestId":"r"}),
        json!({"type":"usage_reference","requestId":"r","reference":null}),
        json!({"type":"request.failed","requestId":"r","error":{"message":"failed"}}),
        json!({"type":"session.opened","sessionId":"s","capabilities":[],"restoration":"core","cwd":"/work"}),
        json!({"type":"session.ready","sessionId":"s"}),
        json!({"type":"session.closed","sessionId":"s"}),
        json!({"type":"session.runtime_failed","sessionId":"s","error":{"message":"failed"}}),
        json!({"type":"session.persistence","sessionId":"s","persistence":{"version":0,"data":null}}),
        json!({"type":"session.prompt_result","sessionId":"s","clientMessageId":"m","result":{"type":"completed"}}),
        json!({"type":"session.turn","sessionId":"s","turnId":"t","state":"completed"}),
        json!({"type":"session.usage","sessionId":"s","usage":{}}),
        json!({"type":"session.config","sessionId":"s","config":{"models":[],"modes":[],"thinkingOptions":[],"settings":[]}}),
        json!({"type":"session.commands","sessionId":"s","commands":[]}),
        json!({"type":"session.permission","sessionId":"s","request":{"id":"p","name":"tool","kind":"tool"}}),
        json!({"type":"session.permission_resolved","sessionId":"s","permissionId":"p"}),
        json!({"type":"session.notice","sessionId":"s","notice":{"id":"n","severity":"info","title":"Notice"}}),
        json!({"type":"timeline.item","sessionId":"s","item":{"type":"user_message","id":"i","text":"hello"}}),
    ];

    for payload in events {
        decode_process_message(&event(&payload)).expect("valid provider event");
    }
}

#[test]
fn provider_event_rejects_invalid_discriminants_and_ids_but_strips_extra_fields() {
    for payload in [
        json!({"type":"request.completed","requestId":""}),
        json!({"type":"session.opened","sessionId":"s","capabilities":[],"restoration":"disk","cwd":"/work"}),
        json!({"type":"session.turn","sessionId":"s","turnId":"t","state":"waiting"}),
        json!({"type":"unknown","requestId":"r"}),
    ] {
        assert!(decode_process_message(&event(&payload)).is_err());
    }

    let decoded = decode_process_message(&event(&json!({
        "type":"request.completed",
        "requestId":"r",
        "extra":true,
    })))
    .expect("event schemas strip unknown fields");
    let value = serde_json::to_value(decoded).expect("encode event");
    assert_eq!(
        value["event"],
        json!({"type":"request.completed","requestId":"r"})
    );
}
