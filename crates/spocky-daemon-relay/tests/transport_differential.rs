//! Differential of the daemon relay client (`relay-transport.ts`, `relay-runtime.ts`,
//! `encrypted-relay-socket.ts`) against the Rust port.
//!
//! Each scenario is a list of operations replayed on the pinned TypeScript (node 22.20.0,
//! virtual clock, fake relay sockets) and on the Rust state machines. After every
//! operation the entries must be identical as raw text: every socket created with its
//! URL, every ping, terminate, close and send, every log record with its bindings and
//! fields, every attach with its metadata, every frame the application or the channel
//! sees. Nothing is normalized.
//!
//! Needs `SPOCKY_PINNED_NODE` (node 22.20.0); `SPOCKY_ALLOW_SKIP=1` skips explicitly.
//! `SPOCKY_RELAY_DAEMON_EVIDENCE` names a directory that receives every raw transcript.

#![allow(clippy::too_many_lines)]

mod support;

use serde_json::{Value, json};
use support::{Endpoint, NodeEndpoint, RustEndpoint, differential, pinned};

const RELAY: &str = "relay.example.test:443";

fn start(endpoint: &str, tls: bool, key_pair: bool) -> Value {
    json!({ "op": "start", "endpoint": endpoint, "useTls": tls, "serverId": "srv-1", "keyPair": key_pair })
}

fn reset() -> Value {
    json!({ "op": "reset" })
}

fn open(id: u64) -> Value {
    json!({ "op": "socket", "id": id, "event": "open" })
}

fn text(id: u64, payload: &str) -> Value {
    json!({ "op": "socket", "id": id, "event": "message", "kind": "buffer", "text": payload })
}

fn message(id: u64, kind: &str, extra: &Value) -> Value {
    let mut op = json!({ "op": "socket", "id": id, "event": "message", "kind": kind });
    for (key, value) in extra.as_object().unwrap() {
        op[key] = value.clone();
    }
    op
}

fn close(id: u64, code: u64, reason: Option<&str>) -> Value {
    let mut op = json!({ "op": "socket", "id": id, "event": "close", "code": code });
    if let Some(reason) = reason {
        op["reason"] = json!(reason);
    }
    op
}

fn error(id: u64, message: &str) -> Value {
    json!({ "op": "socket", "id": id, "event": "error", "message": message })
}

fn pong(id: u64) -> Value {
    json!({ "op": "socket", "id": id, "event": "pong" })
}

fn advance(ms: u64) -> Value {
    json!({ "op": "advance", "ms": ms })
}

fn stop() -> Value {
    json!({ "op": "stop" })
}

fn modes(op: &str, id: Option<u64>, modes: &Value) -> Value {
    let mut value = json!({ "op": op, "modes": modes });
    if let Some(id) = id {
        value["id"] = json!(id);
    }
    value
}

fn connected(id: &str) -> String {
    format!(r#"{{"type":"connected","connectionId":{}}}"#, json!(id))
}

fn sync(ids: &[&str]) -> String {
    format!(r#"{{"type":"sync","connectionIds":{}}}"#, json!(ids))
}

/// A started transport with its control socket open and ready.
fn ready(key_pair: bool) -> Vec<Value> {
    vec![
        reset(),
        start(RELAY, true, key_pair),
        open(1),
        text(1, r#"{"type":"sync","connectionIds":[]}"#),
    ]
}

fn scenario_lifecycle() -> Vec<Value> {
    let mut ops = ready(false);
    ops.extend([
        advance(10_000),
        text(1, r#"{"type":"ping"}"#),
        text(1, &connected("c1")),
        open(2),
        text(2, "from-relay"),
        message(2, "buffer", &json!({ "hex": "00ff10", "isBinary": true })),
        text(1, &sync(&["c1", "c2", " c3 ", "", "c4"])),
        open(3),
        open(4),
        text(1, r#"{"type":"disconnected","connectionId":"c1"}"#),
        close(2, 1001, Some("Client disconnected")),
        close(3, 1006, None),
        error(4, "boom"),
        advance(10_000),
        pong(1),
        advance(30_000),
        close(1, 1006, Some("gone")),
        advance(999),
        advance(1),
        open(5),
        text(5, "late"),
        stop(),
        open(5),
        text(5, "after stop"),
        close(5, 1000, None),
    ]);
    ops
}

fn scenario_ready_timeout() -> Vec<Value> {
    vec![
        reset(),
        start("localhost:6767", false, false),
        open(1),
        advance(7_999),
        advance(1),
        close(1, 1006, None),
        advance(1_000),
        open(2),
        text(2, r#"{"type":"pong"}"#),
        advance(8_000),
        advance(10_000),
    ]
}

fn scenario_stale() -> Vec<Value> {
    vec![
        reset(),
        start(RELAY, true, false),
        open(1),
        text(1, r#"{"type":"pong","ts":1}"#),
        advance(10_000),
        advance(10_000),
        advance(10_000),
        advance(10_000),
        advance(10_000),
        close(1, 1006, Some("stale")),
        advance(1_000),
        open(2),
        text(2, &sync(&[])),
        advance(10_000),
        pong(2),
        advance(10_000),
        pong(2),
        advance(10_000),
        pong(2),
        advance(10_000),
        json!({ "op": "socketState", "id": 2, "readyState": 2 }),
        advance(40_000),
    ]
}

fn scenario_ping_throws() -> Vec<Value> {
    vec![
        reset(),
        modes(
            "socketDefaults",
            None,
            &json!({ "ping": "throw", "terminate": "throw" }),
        ),
        start(RELAY, true, false),
        open(1),
        advance(10_000),
        close(1, 1006, None),
        advance(1_000),
        open(2),
        modes(
            "socketMode",
            Some(2),
            &json!({ "ping": "ok", "terminate": "ok" }),
        ),
        advance(10_000),
        modes("socketMode", Some(2), &json!({ "ping": "throw" })),
        advance(10_000),
    ]
}

fn scenario_backoff() -> Vec<Value> {
    let mut ops = vec![reset(), start("[::1]:6767", false, false)];
    let mut socket = 1;
    for attempt in 1..=34_u64 {
        ops.push(close(socket, 1006, Some("down")));
        ops.push(advance((attempt * 1_000).min(30_000) - 1));
        ops.push(advance(1));
        socket += 1;
    }
    ops.extend([
        open(socket),
        text(socket, r#"{"type":"ping"}"#),
        close(socket, 1006, None),
        advance(999),
        advance(1),
        close(socket + 1, 1006, None),
        advance(2_000),
    ]);
    ops
}

fn scenario_stop_states() -> Vec<Value> {
    let mut all = vec![
        reset(),
        start(RELAY, true, false),
        stop(),
        open(1),
        close(1, 1006, None),
        advance(5_000),
    ];
    all.extend(ready(false));
    all.extend([
        text(1, &sync(&["a", "b", "c"])),
        modes("socketMode", Some(2), &json!({ "close": "throw" })),
        stop(),
        open(2),
        open(3),
        text(1, &connected("late")),
        close(1, 1006, None),
        advance(60_000),
        stop(),
    ]);
    all
}

fn scenario_data_timeouts() -> Vec<Value> {
    let mut ops = ready(false);
    ops.extend([
        text(1, &sync(&["a", "b", "c"])),
        open(2),
        advance(14_999),
        advance(1),
        json!({ "op": "socketState", "id": 4, "readyState": 1 }),
        close(3, 1006, Some("x")),
        text(1, &connected("b")),
        advance(15_000),
        close(4, 1006, None),
        text(1, &connected("a")),
        text(1, r#"{"type":"disconnected","connectionId":"zzz"}"#),
        text(1, r#"{"type":"disconnected","connectionId":"a"}"#),
        text(1, &connected(" a ")),
        text(1, &connected("a")),
        open(2),
        open(2),
        stop(),
    ]);
    ops
}

fn scenario_send_failures() -> Vec<Value> {
    vec![
        reset(),
        modes(
            "socketDefaults",
            None,
            &json!({ "send": "throw", "close": "throw" }),
        ),
        start(RELAY, false, false),
        open(1),
        text(1, r#"{"type":"ping"}"#),
        text(1, &connected("x")),
        text(1, r#"{"type":"disconnected","connectionId":"x"}"#),
        stop(),
    ]
}

fn scenario_invalid_endpoints() -> Vec<Value> {
    let mut ops = Vec::new();
    for endpoint in [
        "",
        "nohostport",
        "host:0",
        "host:70000",
        "[::1]",
        "a b:80",
        "bad host:80",
        "h:80/x",
    ] {
        ops.push(reset());
        ops.push(start(endpoint, true, false));
        ops.push(advance(60_000));
    }
    ops
}

fn scenario_e2ee() -> Vec<Value> {
    let attach_ready = |channel: &str, attach: &str| -> Vec<Value> {
        vec![
            reset(),
            json!({ "op": "channelMode", "mode": channel, "message": "handshake exploded" }),
            json!({ "op": "attachMode", "mode": attach }),
            start(RELAY, true, true),
            open(1),
            text(1, &connected("e1")),
            open(2),
        ]
    };
    let mut ops = Vec::new();
    // Pending channel: frames before the handshake finishes, then ready.
    ops.extend(attach_ready("pending", "ok"));
    ops.extend([
        text(2, "early-text"),
        message(2, "buffer", &json!({ "hex": "0102", "isBinary": true })),
        message(
            2,
            "arraybuffer",
            &json!({ "hex": "0304", "isBinary": true }),
        ),
        message(
            2,
            "fragments",
            &json!({ "parts": ["6162", "6364"], "isBinary": false }),
        ),
        message(
            2,
            "fragments",
            &json!({ "parts": ["c3", "a9"], "isBinary": false }),
        ),
        message(
            2,
            "fragments",
            &json!({ "parts": ["01", "02"], "isBinary": true }),
        ),
        message(2, "string", &json!({ "text": "plain", "isBinary": true })),
        message(2, "string", &json!({ "text": "plain", "isBinary": false })),
        message(2, "buffer", &json!({ "hex": "ff", "isBinary": false })),
        json!({ "op": "channelEvent", "n": 1, "event": "message", "text": "queued-1" }),
        json!({ "op": "channelEvent", "n": 1, "event": "message", "binary": "aa" }),
        json!({ "op": "channel", "n": 1, "result": "ok" }),
        json!({ "op": "channelEvent", "n": 1, "event": "message", "text": "direct" }),
        json!({ "op": "app.read", "id": "e1" }),
        json!({ "op": "app.send", "id": "e1", "text": "hello" }),
        json!({ "op": "app.send", "id": "e1", "binary": "0102" }),
        json!({ "op": "socketState", "id": 2, "bufferedAmount": 67_108_864 - 41 }),
        json!({ "op": "app.send", "id": "e1", "text": "x" }),
        json!({ "op": "app.read", "id": "e1" }),
        json!({ "op": "app.send", "id": "e1", "text": "after" }),
        json!({ "op": "channelEvent", "n": 1, "event": "error", "message": "decrypt failed" }),
        json!({ "op": "channelEvent", "n": 1, "event": "close", "code": 4000, "reason": "bye" }),
        close(2, 1006, Some("wire")),
        error(2, "wire error"),
    ]);
    // Boundary: exact hard bound accepted, one byte over rejected.
    ops.extend(attach_ready("ok", "ok"));
    ops.extend([
        json!({ "op": "socketState", "id": 2, "bufferedAmount": 67_108_864 - 42 }),
        json!({ "op": "app.send", "id": "e1", "text": "x" }),
        json!({ "op": "app.send", "id": "e1", "text": "xx" }),
        json!({ "op": "app.read", "id": "e1" }),
        json!({ "op": "app.close", "id": "e1", "code": 1000, "reason": "done" }),
        json!({ "op": "app.send", "id": "e1", "text": "closed" }),
    ]);
    ops.extend(attach_ready("ok", "ok"));
    ops.extend([
        json!({ "op": "app.terminate", "id": "e1" }),
        json!({ "op": "app.close", "id": "e1" }),
        json!({ "op": "app.read", "id": "e1" }),
    ]);
    ops.extend(attach_ready("ok", "ok"));
    ops.extend([
        json!({ "op": "app.close", "id": "e1" }),
        json!({ "op": "app.terminate", "id": "e1" }),
        json!({ "op": "channelEvent", "n": 1, "event": "close", "code": 1000, "reason": "" }),
    ]);
    // Channel failure closes the relay socket with 1011.
    ops.extend(attach_ready("fail", "ok"));
    ops.extend([
        advance(0),
        text(2, "after-failure"),
        close(2, 1011, Some("E2EE handshake failed")),
    ]);
    ops.extend(attach_ready("pending", "ok"));
    ops.extend([
        modes("socketMode", Some(2), &json!({ "close": "throw" })),
        json!({ "op": "channel", "n": 1, "result": "fail", "message": "late failure" }),
    ]);
    // Attach rejection.
    ops.extend(attach_ready("ok", "reject"));
    ops.extend([text(2, "after-reject")]);
    // Attach pending: frames queue until it settles, in order.
    ops.extend(attach_ready("ok", "pending"));
    ops.extend([
        json!({ "op": "channelEvent", "n": 1, "event": "message", "text": "one" }),
        json!({ "op": "channelEvent", "n": 1, "event": "message", "text": "two" }),
        json!({ "op": "channelEvent", "n": 1, "event": "close", "code": 1001, "reason": "away" }),
        json!({ "op": "attachSettle", "result": "ok" }),
        json!({ "op": "channelEvent", "n": 1, "event": "message", "text": "three" }),
    ]);
    ops.extend(attach_ready("ok", "pending"));
    ops.extend([
        json!({ "op": "attachSettle", "result": "reject" }),
        json!({ "op": "channelEvent", "n": 1, "event": "message", "text": "lost" }),
    ]);
    // Several clients at once and a data socket that closes before the handshake ends.
    ops.extend(attach_ready("pending", "ok"));
    ops.extend([
        text(1, &connected("e2")),
        open(3),
        close(2, 1006, Some("gone")),
        json!({ "op": "channel", "n": 1, "result": "ok" }),
        json!({ "op": "channel", "n": 2, "result": "ok" }),
        json!({ "op": "app.read", "id": "e1" }),
        json!({ "op": "app.read", "id": "e2" }),
    ]);
    ops
}

fn scenario_runtime() -> Vec<Value> {
    let config = |enabled: bool, endpoint: &str| {
        json!({
            "enabled": enabled, "endpoint": endpoint, "publicEndpoint": endpoint,
            "useTls": true, "publicUseTls": false,
        })
    };
    let runtime = |enabled: bool, mode: &str| {
        json!({
            "op": "runtime", "config": config(enabled, "relay.example.test:443"),
            "serverId": "relay-runtime-test", "startMode": mode,
        })
    };
    let enable = |enabled: bool| json!({ "op": "setEnabled", "enabled": enabled });
    vec![
        reset(),
        runtime(false, "ok"),
        enable(true),
        enable(true),
        enable(false),
        enable(false),
        enable(true),
        json!({ "op": "runtimeStop" }),
        json!({ "op": "runtimeStop" }),
        reset(),
        runtime(true, "ok"),
        enable(false),
        enable(true),
        json!({ "op": "runtimeStartMode", "mode": "throw" }),
        enable(false),
        enable(true),
        json!({ "op": "runtimeStartMode", "mode": "stop-rejects" }),
        enable(true),
        enable(false),
        reset(),
        runtime(false, "throw"),
        enable(true),
        json!({ "op": "runtimeStartMode", "mode": "stop-rejects" }),
        enable(true),
        json!({ "op": "runtimeStop" }),
        reset(),
        runtime(false, "stop-rejects"),
        enable(true),
        json!({ "op": "runtimeStop" }),
        json!({ "op": "runtimeStop" }),
    ]
}

fn scenario_encrypted_socket() -> Vec<Value> {
    let create = |send_mode: &str, buffered: Option<u64>, listeners: bool| {
        let mut op = json!({ "op": "enc.create", "sendMode": send_mode, "listeners": listeners });
        if let Some(buffered) = buffered {
            op["buffered"] = json!(buffered);
        }
        op
    };
    let max = 64_u64 * 1024 * 1024;
    let send_text = |payload: &str| json!({ "op": "enc.send", "text": payload });
    let send_bytes = |count: usize| json!({ "op": "enc.send", "binary": "ab".repeat(count) });
    let mut ops = vec![
        reset(),
        json!({ "op": "constants" }),
        create("sync", Some(0), true),
        send_text("hello"),
        json!({ "op": "enc.read" }),
        // Exact hard bound accepted, one byte over rejected.
        json!({ "op": "enc.state", "buffered": max - 45 }),
        send_text("hello"),
        json!({ "op": "enc.read" }),
        reset(),
        create("sync", Some(0), true),
        json!({ "op": "enc.state", "buffered": max - 46 }),
        send_text("hello"),
        json!({ "op": "enc.state", "buffered": max - 44 }),
        send_text("hello"),
        json!({ "op": "enc.read" }),
        send_text("after termination"),
        json!({ "op": "enc.close", "code": 1000, "reason": "x" }),
        json!({ "op": "enc.terminate" }),
    ];
    for (mode, listeners) in [("reject", true), ("reject", false), ("pending", true)] {
        ops.extend([
            reset(),
            create(mode, None, listeners),
            send_text("a"),
            send_bytes(3),
        ]);
        if mode == "pending" {
            ops.extend([
                json!({ "op": "enc.settle", "result": "ok" }),
                json!({ "op": "enc.settle", "result": "reject" }),
            ]);
        }
        ops.push(json!({ "op": "enc.read" }));
    }
    ops.extend([
        reset(),
        create("sync", Some(max - 1), true),
        send_bytes(1),
        json!({ "op": "enc.read" }),
        reset(),
        create("sync", Some(0), true),
        json!({ "op": "enc.terminate" }),
        json!({ "op": "enc.terminate" }),
        json!({ "op": "enc.close", "code": 1001 }),
        json!({ "op": "enc.read" }),
        reset(),
        create("sync", None, true),
        json!({ "op": "enc.close" }),
        json!({ "op": "enc.close", "code": 4000, "reason": "again" }),
        send_text("x"),
        reset(),
        create("sync", Some(5), true),
        json!({ "op": "enc.emit", "event": "close", "code": 1006, "reason": "wire" }),
        json!({ "op": "enc.emit", "event": "close", "code": 1000 }),
        json!({ "op": "enc.emit", "event": "message", "text": "late" }),
        json!({ "op": "enc.emit", "event": "error", "message": "late error" }),
        send_text("x"),
        json!({ "op": "enc.read" }),
        json!({ "op": "enc.state", "buffered": null }),
        json!({ "op": "enc.read" }),
        reset(),
        create("sync", Some(0), false),
        json!({ "op": "enc.emit", "event": "close", "code": 1006, "reason": "" }),
        json!({ "op": "enc.emit", "event": "error", "message": "unlistened" }),
    ]);
    // Frames with multi-byte text count their UTF-8 bytes.
    ops.extend([
        reset(),
        create("sync", Some(max - 40 - 8), true),
        send_text("éé😀"),
        send_text("éé"),
        json!({ "op": "enc.read" }),
    ]);
    ops
}

/// One mini scenario per control message: a ready control socket, then the message.
fn control_message_scenarios() -> Vec<Vec<Value>> {
    let ws = [
        "\u{a0}", "\u{2028}", "\u{feff}", "\u{85}", "\u{180e}", "\t", "\u{b}", "\u{3000}",
        "\u{200b}",
    ];
    let mut texts: Vec<String> = vec![
        r#"{"type":"ping"}"#.into(),
        r#"{"type":"pong"}"#.into(),
        r#"{"type":"ping","type":"pong"}"#.into(),
        r#"{"type":"pong","type":"ping"}"#.into(),
        r#"{"type":"Ping"}"#.into(),
        r#"{"type":["ping"]}"#.into(),
        r#"{"type":"ping"}"#.into(),
        r#"{"type":"ping","extra":{"a":[1,2,{"b":null}]}}"#.into(),
        r#"{"x":{"type":"ping"}}"#.into(),
        r#"["ping"]"#.into(),
        r#"[{"type":"ping"}]"#.into(),
        r#""ping""#.into(),
        "null".into(),
        "1".into(),
        "true".into(),
        String::new(),
        " ".into(),
        "{".into(),
        r#"{"type":"ping"} x"#.into(),
        "\u{feff}{\"type\":\"ping\"}".into(),
        r#"{"type":"sync"}"#.into(),
        r#"{"type":"sync","connectionIds":null}"#.into(),
        r#"{"type":"sync","connectionIds":"a"}"#.into(),
        r#"{"type":"sync","connectionIds":{}}"#.into(),
        r#"{"type":"sync","connectionIds":[]}"#.into(),
        r#"{"type":"sync","connectionIds":["a"]}"#.into(),
        r#"{"type":"sync","connectionIds":["a","a","b"]}"#.into(),
        r#"{"type":"sync","connectionIds":[1,null,true,{},[],"x"," "]}"#.into(),
        r#"{"type":"sync","connectionIds":["a"],"connectionIds":["b"]}"#.into(),
        r#"{"type":"sync","connectionIds":["a b","é","😀","a/b","a?b=c&d","%41","+"]}"#.into(),
        r#"{"type":"sync","connectionIds":["\ud800","\udc00x","ok"]}"#.into(),
        r#"{"type":"sync","connectionIds":["😀","\u0000","\u001f"]}"#.into(),
        r#"{"type":"sync","connectionIds":[["a"],["b"]]}"#.into(),
        r#"{"type":"connected"}"#.into(),
        r#"{"type":"connected","connectionId":null}"#.into(),
        r#"{"type":"connected","connectionId":5}"#.into(),
        r#"{"type":"connected","connectionId":""}"#.into(),
        r#"{"type":"connected","connectionId":"   "}"#.into(),
        r#"{"type":"connected","connectionId":"c"}"#.into(),
        r#"{"type":"connected","connectionId":"c","connectionId":"d"}"#.into(),
        r#"{"type":"connected","connectionId":"\ud800"}"#.into(),
        r#"{"type":"connected","connectionId":"\ud800 "}"#.into(),
        r#"{"type":"connected","connectionId":"a\ud83d"}"#.into(),
        r#"{"type":"connected","connectionId":"a😀b"}"#.into(),
        r#"{"type":"connected","connectionId":" x "}"#.into(),
        r#"{"type":"connected","connectionId":"x"}x"#.into(),
        r#"{"type":"disconnected","connectionId":"c"}"#.into(),
        r#"{"type":"disconnected","connectionId":""}"#.into(),
        r#"{"type":"disconnected"}"#.into(),
        r#"{"type":"unknown","connectionId":"c"}"#.into(),
        r#"{"connectionId":"c"}"#.into(),
        r#"{"type":1}"#.into(),
        r#"{"type":null}"#.into(),
        r#"{"type":"connected","connectionId":"c","v":1e400}"#.into(),
        r#"{"type":"connected","connectionId":"c","v":01}"#.into(),
        r#"{"type":"connected","connectionId":"c","v":-0}"#.into(),
        r#"{"type":"connected","connectionId":"c","v":1.}"#.into(),
        r#"{"__proto__":{"type":"ping"},"type":"pong"}"#.into(),
        r#"{"type":"connected","connectionId":"__proto__"}"#.into(),
        r#"{"type":"connected","connectionId":"constructor"}"#.into(),
        format!(
            r#"{{"type":"connected","connectionId":"{}"}}"#,
            "a".repeat(300)
        ),
        format!(
            r#"{{"type":"sync","connectionIds":[{}]}}"#,
            (0..40)
                .map(|i| format!("\"id{i}\""))
                .collect::<Vec<_>>()
                .join(",")
        ),
    ];
    for space in ws {
        texts.push(format!(
            r#"{{"type":"connected","connectionId":"{space}c{space}"}}"#
        ));
        texts.push(format!(
            r#"{{"type":"connected","connectionId":"{space}"}}"#
        ));
        texts.push(format!(
            r#"{{"type":"sync","connectionIds":["{space}","a{space}b","{space}c"]}}"#
        ));
    }
    let mut scenarios: Vec<Vec<Value>> = Vec::new();
    for payload in &texts {
        let mut ops = ready(false);
        ops.pop();
        ops.extend([text(1, payload), advance(0)]);
        scenarios.push(ops);
    }
    // Message data kinds.
    let kinds = [
        json!({ "kind": "string", "text": r#"{"type":"connected","connectionId":"s"}"# }),
        json!({ "kind": "arraybuffer", "hex": "7b2274797065223a2270696e67227d" }),
        json!({ "kind": "fragments", "parts": ["7b2274797065223a2270696e67227d"] }),
        json!({ "kind": "fragments", "parts": ["7b2274797065223a22", "70696e67227d"] }),
        json!({ "kind": "fragments", "parts": [] }),
        json!({ "kind": "fragments", "parts": ["5b22", "70696e67225d"] }),
        json!({ "kind": "buffer", "hex": "7b2274797065223a2270696e67227d", "isBinary": true }),
        json!({ "kind": "buffer", "hex": "7b2274797065223a22c3a9227d" }),
        json!({ "kind": "buffer", "hex": "7b2274797065223a22ff227d" }),
        json!({ "kind": "buffer", "hex": "7b2274797065223a2263",}),
        json!({ "kind": "buffer", "hex": "efbbbf7b2274797065223a2270696e67227d" }),
        json!({ "kind": "buffer", "hex": "7b2274797065223a22636f6e6e6563746564222c22636f6e6e656374696f6e4964223a22ffc3a9227d" }),
        json!({ "kind": "buffer", "hex": "7b2274797065223a22636f6e6e6563746564222c22636f6e6e656374696f6e4964223a22e29" }),
        json!({ "kind": "buffer", "hex": "" }),
    ];
    for extra in kinds {
        let mut ops = ready(false);
        ops.pop();
        ops.extend([
            message(1, extra["kind"].as_str().unwrap(), &extra),
            advance(0),
        ]);
        scenarios.push(ops);
    }
    scenarios
}

fn run(
    name: &str,
    node: &mut NodeEndpoint,
    rust: &mut RustEndpoint,
    ops: &[Value],
    evidence: &mut Vec<String>,
    failures: &mut Vec<String>,
) {
    let (transcript, mismatches) = differential(node, rust, ops);
    evidence.push(format!("# {name}"));
    evidence.extend(transcript);
    if let Some(first) = mismatches.first() {
        failures.push(format!(
            "{name}: {} differences, first:\n{first}",
            mismatches.len()
        ));
    }
}

#[test]
fn relay_client_matches_the_pinned_typescript() {
    let Some(pinned) = pinned() else { return };
    let mut node = NodeEndpoint::spawn(&pinned);
    let mut rust = RustEndpoint::new();
    let mut evidence = Vec::new();
    let mut failures = Vec::new();
    let named: Vec<(&str, Vec<Value>)> = vec![
        ("lifecycle", scenario_lifecycle()),
        ("ready-timeout", scenario_ready_timeout()),
        ("stale", scenario_stale()),
        ("ping-throws", scenario_ping_throws()),
        ("backoff", scenario_backoff()),
        ("stop-states", scenario_stop_states()),
        ("data-timeouts", scenario_data_timeouts()),
        ("send-failures", scenario_send_failures()),
        ("invalid-endpoints", scenario_invalid_endpoints()),
        ("e2ee", scenario_e2ee()),
        ("runtime", scenario_runtime()),
        ("encrypted-socket", scenario_encrypted_socket()),
    ];
    for (name, ops) in &named {
        run(
            name,
            &mut node,
            &mut rust,
            ops,
            &mut evidence,
            &mut failures,
        );
    }
    for (index, ops) in control_message_scenarios().iter().enumerate() {
        run(
            &format!("control-message-{index}"),
            &mut node,
            &mut rust,
            ops,
            &mut evidence,
            &mut failures,
        );
    }
    assert!(
        failures.is_empty(),
        "{} scenarios differ:\n{}",
        failures.len(),
        failures
            .iter()
            .take(25)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    if let Some(directory) = std::env::var_os("SPOCKY_RELAY_DAEMON_EVIDENCE") {
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            std::path::Path::new(&directory).join("transport.txt"),
            evidence.join("\n"),
        )
        .unwrap();
    }
}

#[allow(dead_code)]
fn unused(_: &mut dyn Endpoint) {}
