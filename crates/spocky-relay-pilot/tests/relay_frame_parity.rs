use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use spocky_crypto::{export_public_key, key_pair_from_secret};
use spocky_relay_pilot::{NetworkConfig, NetworkNode, NodeId};
use tungstenite::client;
use tungstenite::protocol::{CloseFrame, Message, frame::coding::CloseCode};

const DEADLINE: Duration = Duration::from_secs(3);

#[test]
fn escaped_handshake_spellings_match_decoded_json_and_preserve_opaque_bytes() {
    let node = NetworkNode::bind(NodeId::from("alpha")).unwrap();
    let address = node.websocket_address();
    let mut daemon = websocket(address, "escaped-handshake", "server", "1", "");
    let mut client = websocket(address, "escaped-handshake", "client", "1", "");
    let valid_key = export_public_key(&key_pair_from_secret([7; 32]).public_key)
        .unwrap()
        .replace('=', r"\u003d");
    let valid = format!(
        r#"{{"t\u0079pe":"e2ee\u005fhello","k\u0065y":"{valid_key}","capabilities":{{}}}}"#
    );

    client.send(Message::Text(valid.clone().into())).unwrap();
    assert_eq!(daemon.read().unwrap(), Message::Text(valid.into()));

    let invalid_key = export_public_key(&[0; 32]).unwrap();
    let nested = format!(r#"{{"nested":{{"t\u0079pe":"h\u0065llo","k\u0065y":"{invalid_key}"}}}}"#);
    client.send(Message::Text(nested.clone().into())).unwrap();
    assert_eq!(daemon.read().unwrap(), Message::Text(nested.into()));

    let invalid =
        format!(r#"{{"t\u0079pe":"h\u0065llo","k\u0065y":"{invalid_key}","capabilities":{{}}}}"#);
    client
        .send(Message::Binary(invalid.into_bytes().into()))
        .unwrap();
    let close = wait_for_close(&mut client);
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "Invalid handshake key");
}

#[test]
fn malformed_json_handshake_lookalikes_remain_opaque() {
    let node = NetworkNode::bind(NodeId::from("alpha")).unwrap();
    let address = node.websocket_address();
    let mut daemon = websocket(address, "malformed-json", "server", "1", "");
    let mut client = websocket(address, "malformed-json", "client", "1", "");
    let invalid_key = export_public_key(&[0; 32]).unwrap();
    let payloads = [
        r#"{"type":"hello","key":"\uD800"}"#.to_owned(),
        format!(r#"{{"type":"hello","key":"{invalid_key}","ignored":[}}"#),
        format!(r#"{{"type":"hello","key":"{invalid_key}",}}"#),
        format!(r#"{{"type":"hello","key":"{invalid_key}"}} trailing"#),
        format!("{{\"type\":\"hello\",\"key\":\"{invalid_key}\",\u{000c}\"ignored\":null}}"),
    ];

    for payload in payloads {
        client.send(Message::Text(payload.clone().into())).unwrap();
        assert_eq!(daemon.read().unwrap(), Message::Text(payload.into()));
    }
}

#[test]
fn duplicate_handshake_fields_use_first_value() {
    let node = NetworkNode::bind(NodeId::from("alpha")).unwrap();
    let address = node.websocket_address();
    let valid_key = export_public_key(&key_pair_from_secret([7; 32]).public_key).unwrap();
    let invalid_key = export_public_key(&[0; 32]).unwrap();
    let mut daemon = websocket(address, "duplicate-forward", "server", "1", "");
    let mut client = websocket(address, "duplicate-forward", "client", "1", "");
    let first_non_handshake = format!(r#"{{"type":"ping","type":"hello","key":"{invalid_key}"}}"#);
    let first_valid_key =
        format!(r#"{{"type":"hello","key":"{valid_key}","key":"{invalid_key}"}}"#);

    for payload in [first_non_handshake, first_valid_key] {
        client.send(Message::Text(payload.clone().into())).unwrap();
        assert_eq!(daemon.read().unwrap(), Message::Text(payload.into()));
    }

    let mut first_handshake = websocket(address, "duplicate-type-reject", "client", "1", "");
    first_handshake
        .send(Message::Text(
            format!(r#"{{"type":"hello","type":"ping","key":"{invalid_key}"}}"#).into(),
        ))
        .unwrap();
    let close = wait_for_close(&mut first_handshake);
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "Invalid handshake key");

    let mut first_invalid_key = websocket(address, "duplicate-key-reject", "client", "1", "");
    first_invalid_key
        .send(Message::Text(
            format!(r#"{{"type":"hello","key":"{invalid_key}","key":"{valid_key}"}}"#).into(),
        ))
        .unwrap();
    let close = wait_for_close(&mut first_invalid_key);
    assert_eq!(close.code, CloseCode::Policy);
    assert_eq!(close.reason, "Invalid handshake key");
}

#[test]
fn fragmented_message_at_limit_crosses_unchanged_with_interleaved_ping() {
    let node = NetworkNode::bind_with_config(
        NodeId::from("alpha"),
        NetworkConfig {
            max_frame_payload_bytes: 8,
            ..NetworkConfig::default()
        },
    )
    .unwrap();
    let address = node.websocket_address();
    let mut source = websocket(address, "fragment-limit", "client", "2", "shared");
    let mut destination = websocket(address, "fragment-limit", "server", "2", "shared");

    send_raw_frame(&mut source, 0x2, b"abcd", false);
    send_raw_frame(&mut source, 0x9, b"alive", true);
    assert_eq!(
        source.read().unwrap(),
        Message::Pong(b"alive".to_vec().into())
    );
    send_raw_frame(&mut source, 0x0, b"efgh", true);

    assert_eq!(
        destination.read().unwrap(),
        Message::Binary(b"abcdefgh".to_vec().into())
    );
}

#[test]
fn fragmented_message_over_limit_closes_offending_route_with_empty_1009() {
    let node = NetworkNode::bind_with_config(
        NodeId::from("alpha"),
        NetworkConfig {
            max_frame_payload_bytes: 8,
            ..NetworkConfig::default()
        },
    )
    .unwrap();
    let address = node.websocket_address();
    let mut healthy_client = websocket(address, "healthy", "client", "2", "shared");
    let mut healthy_data = websocket(address, "healthy", "server", "2", "shared");
    let mut rejected = websocket(address, "fragment-over", "client", "2", "shared");
    let mut rejected_destination = websocket(address, "fragment-over", "server", "2", "shared");

    send_raw_frame(&mut rejected, 0x2, b"abcd", false);
    send_raw_frame(&mut rejected, 0x0, b"efghi", true);
    let close = wait_for_close(&mut rejected);
    assert_eq!(close.code, CloseCode::Size);
    assert_eq!(close.reason, "");
    let paired_close = wait_for_close(&mut rejected_destination);
    assert_eq!(paired_close.code, CloseCode::Away);
    assert_eq!(paired_close.reason, "Client disconnected");

    healthy_client
        .send(Message::Binary(vec![0x00, 0xff, 0x7e].into()))
        .unwrap();
    assert_eq!(
        healthy_data.read().unwrap(),
        Message::Binary(vec![0x00, 0xff, 0x7e].into())
    );
}

#[test]
fn fragmented_control_message_over_limit_closes_with_empty_1009() {
    let node = NetworkNode::bind_with_config(
        NodeId::from("alpha"),
        NetworkConfig {
            max_frame_payload_bytes: 8,
            max_control_payload_bytes: 4,
            ..NetworkConfig::default()
        },
    )
    .unwrap();
    let mut control = websocket(
        node.websocket_address(),
        "fragmented-control",
        "server",
        "2",
        "",
    );
    let _sync = control.read().unwrap();

    send_raw_frame(&mut control, 0x1, b"123", false);
    send_raw_frame(&mut control, 0x0, b"45", false);
    let close = wait_for_close(&mut control);
    assert_eq!(close.code, CloseCode::Size);
    assert_eq!(close.reason, "");
}

fn websocket(
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

fn send_raw_frame(
    socket: &mut tungstenite::WebSocket<TcpStream>,
    opcode: u8,
    payload: &[u8],
    finished: bool,
) {
    assert!(
        payload.len() <= 125,
        "test helper only supports short frames"
    );
    let mask = [0x11, 0x22, 0x33, 0x44];
    let mut frame = Vec::with_capacity(payload.len() + 6);
    frame.push(if finished { 0x80 | opcode } else { opcode });
    frame.push(0x80 | u8::try_from(payload.len()).unwrap());
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % mask.len()]),
    );
    socket.get_mut().write_all(&frame).unwrap();
    socket.get_mut().flush().unwrap();
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
