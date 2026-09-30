use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

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
fn selected_runtime_bounds_identifiers_and_tracks_admission() {
    let node = NetworkNode::bind_with_config(
        NodeId::from("alpha"),
        NetworkConfig {
            minimum_cluster_size: 1,
            max_websockets: 2,
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
    let stream = TcpStream::connect(address).unwrap();
    stream.set_read_timeout(Some(DEADLINE)).unwrap();
    let url = format!(
        "ws://{address}/ws?serverId={server_id}&role={role}&v=2&connectionId={connection_id}"
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
