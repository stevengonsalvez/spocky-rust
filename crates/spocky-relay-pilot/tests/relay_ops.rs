use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::process::Command;
use std::time::{Duration, Instant};

use spocky_relay_pilot::{NetworkConfig, NetworkNode, NodeId};
use tungstenite::client;
use tungstenite::protocol::Message;

const DEADLINE: Duration = Duration::from_secs(5);

#[test]
fn attached_delivery_exposes_baseline_gauges_and_wait_histogram() {
    let node = NetworkNode::bind(NodeId::from("metrics")).unwrap();
    let address = node.websocket_address();
    let mut source = websocket(address, "delivery-metrics").unwrap();
    let mut destination = websocket(address, "delivery-metrics").unwrap();

    source
        .send(Message::Binary(vec![0xa5; 8 * 1024].into()))
        .unwrap();
    assert_eq!(
        destination.read().unwrap(),
        Message::Binary(vec![0xa5; 8 * 1024].into())
    );

    wait_until(|| metric(address, "spocky_relay_delivery_wait_seconds_count") >= 1);
    let metrics = http_get(address, "/metrics");
    assert!(metrics.contains("spocky_relay_inflight_delivery_bytes 0"));
    assert!(metrics.contains("spocky_relay_backpressured_sources 0"));
    for bucket in ["0.001", "0.01", "0.1", "1", "10", "+Inf"] {
        assert!(
            metrics.contains(&format!(
                "spocky_relay_delivery_wait_seconds_bucket{{le=\"{bucket}\"}}"
            )),
            "missing delivery wait bucket {bucket}"
        );
    }
}

#[test]
fn listener_binds_and_connects_over_a_non_loopback_interface() {
    let host = non_loopback_ipv4().expect("a non-loopback local IPv4 address is required");
    let node = NetworkNode::bind_on(
        NodeId::from("non-loopback"),
        NetworkConfig::default(),
        IpAddr::V4(host),
    )
    .unwrap();

    assert_eq!(node.peer_address().ip(), IpAddr::V4(host));
    assert_eq!(node.websocket_address().ip(), IpAddr::V4(host));
    let mut left = websocket(node.websocket_address(), "interface-route").unwrap();
    let mut right = websocket(node.websocket_address(), "interface-route").unwrap();
    left.send(Message::Text("non-loopback".into())).unwrap();
    assert_eq!(right.read().unwrap(), Message::Text("non-loopback".into()));
}

#[test]
fn bounded_hundreds_profile_caps_admission_and_reconciles_cleanly() {
    const LIMIT: usize = 200;
    const ATTEMPTS: usize = 224;
    let node = NetworkNode::bind_on(
        NodeId::from("bounded-load"),
        NetworkConfig {
            max_websockets: LIMIT,
            ..NetworkConfig::default()
        },
        IpAddr::V4(Ipv4Addr::LOCALHOST),
    )
    .unwrap();
    let address = node.websocket_address();
    let mut admitted = Vec::with_capacity(LIMIT);
    let mut rejected = 0;

    for index in 0..ATTEMPTS {
        match websocket(address, &format!("load-{index}")) {
            Ok(socket) => admitted.push(socket),
            Err(tungstenite::Error::Http(response)) => {
                assert_eq!(response.status(), 503);
                assert_eq!(
                    response.body().as_deref(),
                    Some(b"Relay connection capacity".as_slice())
                );
                rejected += 1;
            }
            Err(error) => panic!("unexpected admission result: {error}"),
        }
    }

    assert_eq!(admitted.len(), LIMIT);
    assert_eq!(rejected, ATTEMPTS - LIMIT);
    assert_eq!(
        metric(address, "spocky_relay_active_websockets"),
        u64::try_from(LIMIT).unwrap()
    );
    assert_eq!(
        metric(address, "spocky_relay_connection_rejections_total"),
        u64::try_from(ATTEMPTS - LIMIT).unwrap()
    );
    drop(admitted);
    wait_until(|| metric(address, "spocky_relay_active_websockets") == 0);
}

#[allow(clippy::result_large_err)]
fn websocket(
    address: SocketAddr,
    session: &str,
) -> Result<tungstenite::WebSocket<TcpStream>, tungstenite::Error> {
    let stream = TcpStream::connect(address)?;
    stream.set_read_timeout(Some(DEADLINE))?;
    match client(format!("ws://{address}/ws?session={session}"), stream) {
        Ok((socket, _)) => Ok(socket),
        Err(tungstenite::HandshakeError::Failure(error)) => Err(error),
        Err(tungstenite::HandshakeError::Interrupted(_)) => {
            panic!("blocking handshake cannot be interrupted")
        }
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

fn metric(address: SocketAddr, name: &str) -> u64 {
    http_get(address, "/metrics")
        .lines()
        .find_map(|line| {
            line.strip_prefix(name)
                .and_then(|value| value.strip_prefix(' '))
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or_else(|| panic!("metric missing: {name}"))
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !predicate() {
        assert!(Instant::now() < deadline, "condition timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn non_loopback_ipv4() -> Option<Ipv4Addr> {
    if let Ok(value) = std::env::var("SPOCKY_RELAY_TEST_HOST") {
        return value
            .parse()
            .ok()
            .filter(|address: &Ipv4Addr| !address.is_loopback());
    }
    let route = Command::new("route")
        .args(["-n", "get", "default"])
        .output()
        .ok()?;
    let interface = String::from_utf8(route.stdout)
        .ok()?
        .lines()
        .find_map(|line| line.trim().strip_prefix("interface:").map(str::trim))?
        .to_owned();
    let address = Command::new("ipconfig")
        .args(["getifaddr", &interface])
        .output()
        .ok()?;
    String::from_utf8(address.stdout).ok()?.trim().parse().ok()
}
