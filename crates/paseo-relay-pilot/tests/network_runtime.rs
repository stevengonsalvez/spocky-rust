use std::thread;
use std::time::{Duration, Instant};

use paseo_relay_pilot::{NetworkNode, NodeId};
use tungstenite::client;
use tungstenite::protocol::{CloseFrame, Message, frame::coding::CloseCode};

const DEADLINE: Duration = Duration::from_secs(3);

#[test]
fn peers_detect_owner_failure_and_accept_takeover() {
    let alpha = NetworkNode::bind(NodeId::from("alpha")).unwrap();
    let beta = NetworkNode::bind(NodeId::from("beta")).unwrap();
    alpha.connect_peer(NodeId::from("beta"), beta.peer_address());
    beta.connect_peer(NodeId::from("alpha"), alpha.peer_address());

    let (_alpha_socket, _) = websocket(alpha.websocket_address(), "failure").unwrap();
    wait_until(|| beta.owner("failure") == Some(NodeId::from("alpha")));

    alpha.stop();
    wait_until(|| beta.owner("failure").is_none());
    let (_beta_socket, _) = websocket(beta.websocket_address(), "failure").unwrap();
    wait_until(|| beta.owner("failure") == Some(NodeId::from("beta")));
    println!("peer failure: automatic detection=true takeover=beta");
}

#[test]
fn websocket_frames_cross_the_owner_unchanged_and_in_order() {
    let node = NetworkNode::bind(NodeId::from("alpha")).unwrap();
    let (mut daemon, _) = websocket(node.websocket_address(), "frames").unwrap();
    let (mut client, _) = websocket(node.websocket_address(), "frames").unwrap();

    client.send(Message::Text("first".into())).unwrap();
    client.send(Message::Binary(vec![1, 2, 3].into())).unwrap();
    assert_eq!(daemon.read().unwrap(), Message::Text("first".into()));
    assert_eq!(
        daemon.read().unwrap(),
        Message::Binary(vec![1, 2, 3].into())
    );

    daemon.send(Message::Binary(vec![4, 5].into())).unwrap();
    assert_eq!(client.read().unwrap(), Message::Binary(vec![4, 5].into()));
    println!("websocket forwarding: ordered_frames=3 byte_preserving=true");
}

#[test]
fn partition_healing_closes_loser_and_reroutes_new_websockets() {
    let alpha = NetworkNode::bind(NodeId::from("alpha")).unwrap();
    let beta = NetworkNode::bind(NodeId::from("beta")).unwrap();
    alpha.connect_peer(NodeId::from("beta"), beta.peer_address());
    beta.connect_peer(NodeId::from("alpha"), alpha.peer_address());
    alpha.disconnect_peer(&NodeId::from("beta"));
    beta.disconnect_peer(&NodeId::from("alpha"));

    let (_alpha_socket, _) = websocket(alpha.websocket_address(), "partition").unwrap();
    let (mut beta_socket, _) = websocket(beta.websocket_address(), "partition").unwrap();
    assert_eq!(alpha.owner("partition"), Some(NodeId::from("alpha")));
    assert_eq!(beta.owner("partition"), Some(NodeId::from("beta")));

    alpha.reconnect_peer(&NodeId::from("beta"));
    beta.reconnect_peer(&NodeId::from("alpha"));
    wait_until(|| beta.owner("partition") == Some(NodeId::from("alpha")));
    let close = wait_for_close(&mut beta_socket);
    assert_eq!(close.code, CloseCode::Restart);
    assert_eq!(close.reason, "Session owner moved");

    let error = websocket(beta.websocket_address(), "partition").unwrap_err();
    let tungstenite::Error::Http(response) = error else {
        panic!("expected HTTP reroute, got {error:?}");
    };
    assert_eq!(response.status(), 409);
    assert_eq!(response.headers()["x-reroute-target"], "alpha");
    println!("partition recovery: winner=alpha loser_close=1012 reroute=alpha");
}

#[allow(clippy::result_large_err)]
fn websocket(
    address: std::net::SocketAddr,
    session: &str,
) -> Result<
    (
        tungstenite::WebSocket<std::net::TcpStream>,
        tungstenite::handshake::client::Response,
    ),
    tungstenite::Error,
> {
    let stream = std::net::TcpStream::connect(address)?;
    stream.set_read_timeout(Some(DEADLINE))?;
    match client(format!("ws://{address}/ws?session={session}"), stream) {
        Ok(connected) => Ok(connected),
        Err(tungstenite::HandshakeError::Failure(error)) => Err(error),
        Err(tungstenite::HandshakeError::Interrupted(_)) => {
            panic!("blocking handshake cannot be interrupted")
        }
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !predicate() {
        assert!(Instant::now() < deadline, "condition missed deadline");
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_close(socket: &mut tungstenite::WebSocket<std::net::TcpStream>) -> CloseFrame {
    let deadline = Instant::now() + DEADLINE;
    loop {
        match socket.read() {
            Ok(Message::Close(Some(close))) => return close,
            Ok(_) | Err(tungstenite::Error::Io(_)) if Instant::now() < deadline => {}
            result => panic!("expected close frame, got {result:?}"),
        }
    }
}
