use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use spocky_crypto::{export_public_key, key_pair_from_secret};
use spocky_relay_pilot::{NetworkConfig, NetworkNode, NodeId, TopologyState};
use tungstenite::client;
use tungstenite::protocol::{CloseFrame, Message, frame::coding::CloseCode};

const DEADLINE: Duration = Duration::from_secs(3);

#[test]
fn selected_v2_runtime_pairs_control_client_and_data() {
    let node = NetworkNode::bind(NodeId::from("alpha")).unwrap();
    let address = node.websocket_address();
    let mut control = websocket(address, "server-1", "server", "");

    assert_eq!(
        control.read().unwrap(),
        Message::Text(r#"{"type":"sync","connectionIds":[]}"#.into())
    );
    control
        .send(Message::Text(r#"{"type":"ping"}"#.into()))
        .unwrap();
    let Message::Text(pong) = control.read().unwrap() else {
        panic!("expected text pong");
    };
    let timestamp = pong
        .strip_prefix(r#"{"type":"pong","ts":"#)
        .and_then(|value| value.strip_suffix('}'))
        .and_then(|value| value.parse::<u64>().ok())
        .expect("pong timestamp");
    assert!(timestamp > 0);

    let mut client = websocket(address, "server-1", "client", "client-1");
    assert_eq!(
        control.read().unwrap(),
        Message::Text(r#"{"type":"connected","connectionId":"client-1"}"#.into())
    );

    client.send(Message::Text("before-data".into())).unwrap();
    client
        .send(Message::Binary(vec![0, 0xff, 7].into()))
        .unwrap();
    let mut data = websocket(address, "server-1", "server", "client-1");
    assert_eq!(data.read().unwrap(), Message::Text("before-data".into()));
    assert_eq!(
        data.read().unwrap(),
        Message::Binary(vec![0, 0xff, 7].into())
    );

    data.send(Message::Binary(vec![9, 8, 7].into())).unwrap();
    assert_eq!(
        client.read().unwrap(),
        Message::Binary(vec![9, 8, 7].into())
    );

    let mut replacement = websocket(address, "server-1", "server", "client-1");
    let close = wait_for_close(&mut data);
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "Replaced by new connection");
    wait_until(|| http_get(address, "/metrics").contains("spocky_relay_active_websockets 3"));

    client.close(None).unwrap();
    let close = wait_for_close(&mut replacement);
    assert_eq!(close.code, CloseCode::Away);
    assert_eq!(close.reason, "Client disconnected");
    assert_eq!(
        control.read().unwrap(),
        Message::Text(r#"{"type":"disconnected","connectionId":"client-1"}"#.into())
    );
}

#[test]
fn selected_pre_attach_queue_fails_closed_at_bound() {
    let node = NetworkNode::bind(NodeId::from("alpha")).unwrap();
    let mut client = websocket(
        node.websocket_address(),
        "bounded-buffer",
        "client",
        "waiting",
    );
    for index in 0_u8..33 {
        client.send(Message::Binary(vec![index].into())).unwrap();
    }
    let close = wait_for_close(&mut client);
    assert_eq!(close.code, CloseCode::Again);
    assert_eq!(close.reason, "Data route unavailable");
}

#[test]
fn selected_runtime_rejects_invalid_client_handshake_keys() {
    let node = NetworkNode::bind(NodeId::from("alpha")).unwrap();
    let address = node.websocket_address();
    let mut daemon = versioned_websocket(address, "handshake", "server", "1", "");
    let mut client = versioned_websocket(address, "handshake", "client", "1", "");
    let valid_key = export_public_key(&key_pair_from_secret([7; 32]).public_key).unwrap();
    let valid = format!(r#"{{"type":"e2ee_hello","key":"{valid_key}"}}"#);

    client.send(Message::Text(valid.clone().into())).unwrap();
    assert_eq!(daemon.read().unwrap(), Message::Text(valid.into()));

    let invalid_key = export_public_key(&[0; 32]).unwrap();
    let nested = format!(r#"{{"nested":{{"type":"hello","key":"{invalid_key}"}}}}"#);
    client.send(Message::Text(nested.clone().into())).unwrap();
    assert_eq!(daemon.read().unwrap(), Message::Text(nested.into()));

    client
        .send(Message::Text(
            format!(r#"{{"type":"hello","key":"{invalid_key}"}}"#).into(),
        ))
        .unwrap();
    let close = wait_for_close(&mut client);
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "Invalid handshake key");
}

#[test]
fn selected_runtime_accepts_escaped_handshake_spellings() {
    let node = NetworkNode::bind(NodeId::from("alpha")).unwrap();
    let address = node.websocket_address();
    let mut daemon = versioned_websocket(address, "escaped-valid", "server", "1", "");
    let mut client = versioned_websocket(address, "escaped-valid", "client", "1", "");
    let valid_key = export_public_key(&key_pair_from_secret([7; 32]).public_key).unwrap();
    let escaped_key = escape_first_ascii(&valid_key);
    let payload = format!(r#"{{"t\u0079pe":"e2ee_\u0068ello","k\u0065y":"{escaped_key}"}}"#);

    client.send(Message::Text(payload.clone().into())).unwrap();
    assert_eq!(daemon.read().unwrap(), Message::Text(payload.into()));
}

#[test]
fn selected_runtime_rejects_invalid_keys_with_escaped_handshake_spellings() {
    let node = NetworkNode::bind(NodeId::from("alpha")).unwrap();
    let address = node.websocket_address();
    let invalid_key = export_public_key(&[0; 32]).unwrap();
    let escaped_key = escape_first_ascii(&invalid_key);
    let cases = [
        format!(r#"{{"type":"h\u0065llo","key":"{invalid_key}"}}"#),
        format!(r#"{{"t\u0079pe":"hello","key":"{invalid_key}"}}"#),
        format!(r#"{{"type":"hello","k\u0065y":"{invalid_key}"}}"#),
        format!(r#"{{"type":"hello","key":"{escaped_key}"}}"#),
    ];

    for (index, payload) in cases.into_iter().enumerate() {
        let session = format!("escaped-invalid-{index}");
        let _daemon = versioned_websocket(address, &session, "server", "1", "");
        let mut client = versioned_websocket(address, &session, "client", "1", "");
        client.send(Message::Text(payload.into())).unwrap();
        let close = wait_for_close(&mut client);
        assert_eq!(close.code, CloseCode::Policy);
        assert_eq!(close.reason, "Invalid handshake key");
    }
}

#[test]
fn selected_runtime_closes_oversized_data_and_control_frames() {
    let node = NetworkNode::bind_with_config(
        NodeId::from("alpha"),
        NetworkConfig {
            max_frame_payload_bytes: 8,
            max_control_payload_bytes: 4,
            ..NetworkConfig::default()
        },
    )
    .unwrap();
    let address = node.websocket_address();
    let mut client = websocket(address, "frame-limit", "client", "client-1");
    client.send(Message::Binary(vec![0xa5; 9].into())).unwrap();
    let close = wait_for_close(&mut client);
    assert_eq!(close.code, CloseCode::Size);
    assert_eq!(close.reason, "");

    let mut control = websocket(address, "control-limit", "server", "");
    let _sync = control.read().unwrap();
    control.send(Message::Text("12345".into())).unwrap();
    let close = wait_for_close(&mut control);
    assert_eq!(close.code, CloseCode::Size);
    assert_eq!(close.reason, "");
}

#[test]
fn selected_runtime_closes_unresponsive_control() {
    let node = NetworkNode::bind_with_config(
        NodeId::from("alpha"),
        NetworkConfig {
            control_heartbeat_timeout: Duration::from_millis(120),
            ..NetworkConfig::default()
        },
    )
    .unwrap();
    let address = node.websocket_address();
    let mut control = websocket(address, "watchdog", "server", "");
    let _sync = control.read().unwrap();
    let _client = websocket(address, "watchdog", "client", "waiting");
    let _connected = control.read().unwrap();

    let close = wait_for_close(&mut control);
    assert_eq!(close.code, CloseCode::Error);
    assert_eq!(close.reason, "Control unresponsive");
}

#[test]
fn selected_runtime_accounts_ingress_bytes_and_reconciles_on_close() {
    let node = NetworkNode::bind_with_config(
        NodeId::from("alpha"),
        NetworkConfig {
            ingress_budget_bytes: 4,
            ..NetworkConfig::default()
        },
    )
    .unwrap();
    let address = node.websocket_address();
    let mut client = websocket(address, "ingress", "client", "waiting");
    client
        .send(Message::Binary(vec![1, 2, 3, 4].into()))
        .unwrap();
    wait_until(|| http_get(address, "/metrics").contains("spocky_relay_ingress_reserved_bytes 4"));
    client.send(Message::Binary(vec![5].into())).unwrap();
    let close = wait_for_close(&mut client);
    assert_eq!(close.code, CloseCode::Again);
    assert_eq!(close.reason, "Relay ingress capacity");
    wait_until(|| http_get(address, "/metrics").contains("spocky_relay_ingress_reserved_bytes 0"));
}

#[test]
fn selected_runtime_drains_new_work_without_closing_established_links() {
    let node = NetworkNode::bind(NodeId::from("alpha")).unwrap();
    let address = node.websocket_address();
    let mut client = websocket(address, "drain-existing", "client", "client-1");
    let mut data = websocket(address, "drain-existing", "server", "client-1");

    node.begin_drain();
    assert!(http_get(address, "/ready").starts_with("HTTP/1.1 503"));
    assert!(http_get(address, "/metrics").contains("spocky_relay_draining 1"));
    let rejected = websocket_error(address, "drain-new", "client", "client-2");
    assert_eq!(rejected.status(), 503);
    assert_eq!(rejected.body().as_deref(), Some(b"draining".as_slice()));

    client.send(Message::Text("still-live".into())).unwrap();
    assert_eq!(data.read().unwrap(), Message::Text("still-live".into()));

    node.cancel_drain();
    assert!(http_get(address, "/ready").starts_with("HTTP/1.1 200"));
    let _new = websocket(address, "drain-new", "client", "client-2");
}

#[test]
fn selected_runtime_bounds_identifiers_and_tracks_admission() {
    let node = NetworkNode::bind_with_config(
        NodeId::from("alpha"),
        NetworkConfig {
            minimum_cluster_size: 1,
            max_websockets: 2,
            ..NetworkConfig::default()
        },
    )
    .unwrap();
    let address = node.websocket_address();

    assert!(http_get(address, "/health").starts_with("HTTP/1.1 200"));
    assert!(http_get(address, "/ready").starts_with("HTTP/1.1 200"));

    let control = websocket(address, "bounded", "server", "");
    let client = websocket(address, "bounded", "client", "client-1");
    assert!(http_get(address, "/ready").starts_with("HTTP/1.1 503"));

    let rejected = websocket_error(address, "rejected-before-owner", "client", "client-2");
    assert_eq!(rejected.status(), 503);
    assert_eq!(
        rejected.body().as_deref(),
        Some(b"Relay connection capacity".as_slice())
    );
    assert_eq!(node.owner("rejected-before-owner"), None);

    let metrics = http_get(address, "/metrics");
    assert!(metrics.contains("spocky_relay_active_websockets 2"));
    assert!(metrics.contains("spocky_relay_connection_rejections_total 1"));
    assert!(!metrics.contains("bounded"));
    assert!(!metrics.contains("client-1"));

    drop(client);
    wait_until(|| http_get(address, "/ready").starts_with("HTTP/1.1 200"));
    drop(control);

    let too_long = "s".repeat(257);
    let rejected = websocket_error(address, &too_long, "server", "");
    assert_eq!(rejected.status(), 400);
    assert_eq!(
        rejected.body().as_deref(),
        Some(b"serverId is too long".as_slice())
    );
}

#[test]
fn selected_discovery_traces_node_loss_under_bounded_load() {
    let alpha = NetworkNode::bind_with_config(
        NodeId::from("alpha"),
        NetworkConfig {
            minimum_cluster_size: 2,
            max_websockets: 32,
            ..NetworkConfig::default()
        },
    )
    .unwrap();
    let beta = NetworkNode::bind(NodeId::from("beta")).unwrap();
    alpha.discover_peers([(NodeId::from("beta"), beta.peer_address())]);
    wait_until(|| http_get(alpha.websocket_address(), "/ready").starts_with("HTTP/1.1 200"));

    let sockets = (0..16)
        .map(|index| legacy_websocket(beta.websocket_address(), &format!("load-{index}")))
        .collect::<Vec<_>>();
    wait_until(|| {
        (0..16).all(|index| alpha.owner(&format!("load-{index}")) == Some(NodeId::from("beta")))
    });
    assert_eq!(
        alpha
            .topology_events()
            .iter()
            .map(|event| event.state)
            .collect::<Vec<_>>(),
        vec![TopologyState::Discovered, TopologyState::Available]
    );

    beta.stop();
    wait_until(|| (0..16).all(|index| alpha.owner(&format!("load-{index}")).is_none()));
    assert!(http_get(alpha.websocket_address(), "/ready").starts_with("HTTP/1.1 503"));
    assert_eq!(
        alpha
            .topology_events()
            .iter()
            .map(|event| event.state)
            .collect::<Vec<_>>(),
        vec![
            TopologyState::Discovered,
            TopologyState::Available,
            TopologyState::Lost,
        ]
    );
    let metrics = http_get(alpha.websocket_address(), "/metrics");
    assert!(metrics.contains("spocky_relay_peer_losses_total 1"));
    assert!(!metrics.contains("beta"));
    drop(sockets);
}

#[test]
fn selected_linux_process_discovers_configured_peer() {
    let mut beta = SelectedProcess::spawn("beta", None, 1, 8);
    let peer = format!("beta={}", beta.peer_address);
    let mut alpha = SelectedProcess::spawn("alpha", Some(&peer), 2, 8);

    wait_until(|| http_get(alpha.websocket_address, "/ready").starts_with("HTTP/1.1 200"));
    beta.terminate();
    wait_until(|| http_get(alpha.websocket_address, "/ready").starts_with("HTTP/1.1 503"));
    alpha.terminate();
}

struct SelectedProcess {
    child: Child,
    peer_address: SocketAddr,
    websocket_address: SocketAddr,
}

impl SelectedProcess {
    fn spawn(node: &str, peers: Option<&str>, minimum_cluster_size: usize, max: usize) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_spocky-relay-network-node"));
        command
            .arg(node)
            .env(
                "SPOCKY_RELAY_MIN_CLUSTER_SIZE",
                minimum_cluster_size.to_string(),
            )
            .env("SPOCKY_RELAY_MAX_WEBSOCKETS", max.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(peers) = peers {
            command.env("SPOCKY_RELAY_PEERS", peers);
        }
        let mut child = command.spawn().unwrap();
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        let fields = ready.trim().split('\t').collect::<Vec<_>>();
        assert_eq!(fields.first(), Some(&"READY"));
        Self {
            child,
            peer_address: fields[3].parse().unwrap(),
            websocket_address: fields[4].parse().unwrap(),
        }
    }

    fn terminate(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
        }
    }
}

impl Drop for SelectedProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn websocket(
    address: SocketAddr,
    server_id: &str,
    role: &str,
    connection_id: &str,
) -> tungstenite::WebSocket<TcpStream> {
    versioned_websocket(address, server_id, role, "2", connection_id)
}

fn versioned_websocket(
    address: SocketAddr,
    server_id: &str,
    role: &str,
    version: &str,
    connection_id: &str,
) -> tungstenite::WebSocket<TcpStream> {
    let stream = TcpStream::connect(address).unwrap();
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    let url = format!(
        "ws://{address}/ws?serverId={server_id}&role={role}&v={version}&connectionId={connection_id}"
    );
    match client(url, stream) {
        Ok((socket, _)) => socket,
        Err(tungstenite::HandshakeError::Failure(error)) => panic!("handshake failed: {error}"),
        Err(tungstenite::HandshakeError::Interrupted(_)) => panic!("handshake interrupted"),
    }
}

fn legacy_websocket(address: SocketAddr, session: &str) -> tungstenite::WebSocket<TcpStream> {
    let stream = TcpStream::connect(address).unwrap();
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    match client(format!("ws://{address}/ws?session={session}"), stream) {
        Ok((socket, _)) => socket,
        Err(result) => panic!("legacy handshake failed: {result:?}"),
    }
}

fn websocket_error(
    address: SocketAddr,
    server_id: &str,
    role: &str,
    connection_id: &str,
) -> tungstenite::http::Response<Option<Vec<u8>>> {
    let stream = TcpStream::connect(address).unwrap();
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    let url = format!(
        "ws://{address}/ws?serverId={server_id}&role={role}&v=2&connectionId={connection_id}"
    );
    match client(url, stream) {
        Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(response))) => response,
        result => panic!("expected HTTP handshake error, got {result:?}"),
    }
}

fn http_get(address: SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + DEADLINE;
    while !predicate() {
        assert!(std::time::Instant::now() < deadline, "condition timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_close(socket: &mut tungstenite::WebSocket<TcpStream>) -> CloseFrame {
    let deadline = std::time::Instant::now() + DEADLINE;
    loop {
        match socket.read() {
            Ok(Message::Close(Some(close))) => return close,
            Ok(_) | Err(tungstenite::Error::Io(_)) if std::time::Instant::now() < deadline => {}
            result => panic!("expected close frame, got {result:?}"),
        }
    }
}

fn escape_first_ascii(value: &str) -> String {
    let first = value.as_bytes()[0];
    format!(r"\u{first:04x}{}", &value[1..])
}
