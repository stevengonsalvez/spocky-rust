use std::collections::{BTreeMap, VecDeque};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::protocol::{CloseFrame, Message, frame::coding::CloseCode};

use spocky_crypto::{derive_shared_key, import_public_key};

use crate::NodeId;

const POLL_INTERVAL: Duration = Duration::from_millis(25);
const SOCKET_TIMEOUT: Duration = Duration::from_millis(100);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const FAILURE_THRESHOLD: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetworkConfig {
    pub minimum_cluster_size: usize,
    pub max_websockets: usize,
    pub max_frame_payload_bytes: usize,
    pub max_control_payload_bytes: usize,
    pub control_heartbeat_timeout: Duration,
    pub ingress_budget_bytes: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TopologyState {
    Discovered,
    Available,
    Lost,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TopologyEvent {
    pub sequence: u64,
    pub peer: NodeId,
    pub state: TopologyState,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            minimum_cluster_size: 1,
            max_websockets: 1_024,
            max_frame_payload_bytes: 32 * 1024 * 1024 - 14,
            max_control_payload_bytes: 64 * 1024,
            control_heartbeat_timeout: Duration::from_secs(15),
            ingress_budget_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug)]
struct Peer {
    address: SocketAddr,
    enabled: bool,
    available: bool,
    failures: u8,
}

#[derive(Clone, Debug)]
struct SocketSender {
    frames: mpsc::SyncSender<Message>,
    control: mpsc::Sender<SocketClose>,
}

#[derive(Clone, Debug)]
enum SocketClose {
    OwnerMoved,
    SlowConsumer,
    DataRouteUnavailable,
    InvalidHandshake,
    MessageTooLarge,
    ControlUnresponsive,
    RelayIngressCapacity,
    ClientDisconnected,
    Replaced,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ConnectionKind {
    Legacy,
    Control,
    Client(String),
    Data(String),
}

#[derive(Clone, Debug)]
struct AcceptedConnection {
    session: String,
    kind: ConnectionKind,
    client: bool,
    admitted: bool,
}

#[derive(Clone, Debug, Default)]
struct PendingFrames {
    frames: VecDeque<Message>,
    bytes: usize,
}

struct Shared {
    node: NodeId,
    peers: Mutex<BTreeMap<NodeId, Peer>>,
    owners: Mutex<BTreeMap<String, NodeId>>,
    sockets: Mutex<BTreeMap<String, BTreeMap<u64, SocketSender>>>,
    connections: Mutex<BTreeMap<u64, AcceptedConnection>>,
    pending: Mutex<BTreeMap<(String, String), PendingFrames>>,
    config: NetworkConfig,
    admissions: Mutex<usize>,
    draining: AtomicBool,
    connection_rejections: AtomicU64,
    reroute_responses: AtomicU64,
    frames_forwarded: AtomicU64,
    bytes_forwarded: AtomicU64,
    peer_losses: AtomicU64,
    topology: Mutex<Vec<TopologyEvent>>,
    ingress_reserved_bytes: Mutex<usize>,
    next_socket: AtomicU64,
    running: AtomicBool,
}

/// Local network runtime used to exercise relay ownership across real TCP and
/// WebSocket boundaries. Peers exchange ownership snapshots directly and
/// detect peer listener loss from bounded connection failures.
pub struct NetworkNode {
    shared: Arc<Shared>,
    peer_address: SocketAddr,
    websocket_address: SocketAddr,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl NetworkNode {
    /// Binds peer and WebSocket listeners to random loopback ports.
    ///
    /// # Errors
    ///
    /// Returns the listener binding error.
    pub fn bind(node: NodeId) -> io::Result<Self> {
        Self::bind_with_config(node, NetworkConfig::default())
    }

    /// Binds the selected runtime with explicit admission and cluster bounds.
    ///
    /// # Errors
    ///
    /// Returns the listener binding error.
    pub fn bind_with_config(node: NodeId, config: NetworkConfig) -> io::Result<Self> {
        let peer_listener = loopback_listener()?;
        let websocket_listener = loopback_listener()?;
        let peer_address = peer_listener.local_addr()?;
        let websocket_address = websocket_listener.local_addr()?;
        let shared = Arc::new(Shared {
            node,
            peers: Mutex::new(BTreeMap::new()),
            owners: Mutex::new(BTreeMap::new()),
            sockets: Mutex::new(BTreeMap::new()),
            connections: Mutex::new(BTreeMap::new()),
            pending: Mutex::new(BTreeMap::new()),
            config,
            admissions: Mutex::new(0),
            draining: AtomicBool::new(false),
            connection_rejections: AtomicU64::new(0),
            reroute_responses: AtomicU64::new(0),
            frames_forwarded: AtomicU64::new(0),
            bytes_forwarded: AtomicU64::new(0),
            peer_losses: AtomicU64::new(0),
            topology: Mutex::new(Vec::new()),
            ingress_reserved_bytes: Mutex::new(0),
            next_socket: AtomicU64::new(0),
            running: AtomicBool::new(true),
        });

        let workers = vec![
            spawn_peer_listener(Arc::clone(&shared), peer_listener),
            spawn_websocket_listener(Arc::clone(&shared), websocket_listener),
            spawn_gossip(Arc::clone(&shared)),
        ];
        Ok(Self {
            shared,
            peer_address,
            websocket_address,
            workers: Mutex::new(workers),
        })
    }

    #[must_use]
    pub const fn peer_address(&self) -> SocketAddr {
        self.peer_address
    }

    #[must_use]
    pub const fn websocket_address(&self) -> SocketAddr {
        self.websocket_address
    }

    pub fn connect_peer(&self, node: NodeId, address: SocketAddr) {
        let discovered = !locked(&self.shared.peers).contains_key(&node);
        locked(&self.shared.peers).insert(
            node.clone(),
            Peer {
                address,
                enabled: true,
                available: false,
                failures: 0,
            },
        );
        if discovered {
            record_topology(&self.shared, node, TopologyState::Discovered);
        }
    }

    pub fn discover_peers<I>(&self, peers: I)
    where
        I: IntoIterator<Item = (NodeId, SocketAddr)>,
    {
        for (node, address) in peers {
            self.connect_peer(node, address);
        }
    }

    pub fn disconnect_peer(&self, node: &NodeId) {
        if let Some(peer) = locked(&self.shared.peers).get_mut(node) {
            peer.enabled = false;
            peer.available = false;
            peer.failures = 0;
        }
    }

    pub fn reconnect_peer(&self, node: &NodeId) {
        if let Some(peer) = locked(&self.shared.peers).get_mut(node) {
            peer.enabled = true;
            peer.available = false;
            peer.failures = 0;
        }
    }

    pub fn begin_drain(&self) {
        self.shared.draining.store(true, Ordering::Relaxed);
    }

    pub fn cancel_drain(&self) {
        self.shared.draining.store(false, Ordering::Relaxed);
    }

    #[must_use]
    pub fn owner(&self, session: &str) -> Option<NodeId> {
        locked(&self.shared.owners).get(session).cloned()
    }

    #[must_use]
    pub fn topology_events(&self) -> Vec<TopologyEvent> {
        locked(&self.shared.topology).clone()
    }

    pub fn stop(&self) {
        self.stop_inner();
    }

    fn stop_inner(&self) {
        if !self.shared.running.swap(false, Ordering::SeqCst) {
            return;
        }
        for worker in self.workers.lock().unwrap().drain(..) {
            worker.join().expect("network worker must stop cleanly");
        }
    }
}

impl Drop for NetworkNode {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

fn loopback_listener() -> io::Result<TcpListener> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn spawn_peer_listener(shared: Arc<Shared>, listener: TcpListener) -> JoinHandle<()> {
    thread::spawn(move || {
        while shared.running.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => serve_snapshot(&shared, stream),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(POLL_INTERVAL);
                }
                Err(_) => return,
            }
        }
    })
}

fn serve_snapshot(shared: &Shared, mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(SOCKET_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SOCKET_TIMEOUT));
    let mut request = String::new();
    if BufReader::new(&stream).read_line(&mut request).is_err() || request.trim() != "SNAPSHOT" {
        return;
    }
    for (session, owner) in shared.owners.lock().unwrap().iter() {
        if writeln!(stream, "OWNER\t{session}\t{}", owner.as_str()).is_err() {
            return;
        }
    }
    let _ = writeln!(stream, "END");
}

fn spawn_gossip(shared: Arc<Shared>) -> JoinHandle<()> {
    thread::spawn(move || {
        while shared.running.load(Ordering::Relaxed) {
            let peers: Vec<_> = shared
                .peers
                .lock()
                .unwrap()
                .iter()
                .map(|(node, peer)| (node.clone(), peer.clone()))
                .collect();
            for (node, peer) in peers {
                if !peer.enabled {
                    continue;
                }
                match fetch_snapshot(peer.address) {
                    Ok(snapshot) => {
                        mark_peer_live(&shared, &node);
                        merge_snapshot(&shared, snapshot);
                    }
                    Err(_) => mark_peer_failure(&shared, &node),
                }
            }
            thread::sleep(POLL_INTERVAL);
        }
    })
}

fn fetch_snapshot(address: SocketAddr) -> io::Result<BTreeMap<String, NodeId>> {
    let mut stream = TcpStream::connect_timeout(&address, SOCKET_TIMEOUT)?;
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))?;
    writeln!(stream, "SNAPSHOT")?;
    let mut snapshot = BTreeMap::new();
    for line in BufReader::new(stream).lines() {
        let line = line?;
        if line == "END" {
            return Ok(snapshot);
        }
        let mut fields = line.split('\t');
        if fields.next() == Some("OWNER")
            && let (Some(session), Some(node), None) = (fields.next(), fields.next(), fields.next())
        {
            snapshot.insert(session.to_owned(), NodeId::from(node));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "peer snapshot ended before terminator",
    ))
}

fn mark_peer_live(shared: &Shared, node: &NodeId) {
    let became_available = if let Some(peer) = shared.peers.lock().unwrap().get_mut(node) {
        let became_available = !peer.available;
        peer.failures = 0;
        peer.available = true;
        became_available
    } else {
        false
    };
    if became_available {
        record_topology(shared, node.clone(), TopologyState::Available);
    }
}

fn mark_peer_failure(shared: &Shared, node: &NodeId) {
    let failed = {
        let mut peers = shared.peers.lock().unwrap();
        let Some(peer) = peers.get_mut(node) else {
            return;
        };
        peer.failures = peer.failures.saturating_add(1);
        if peer.failures >= FAILURE_THRESHOLD {
            let was_available = peer.available;
            peer.available = false;
            was_available
        } else {
            false
        }
    };
    if failed {
        shared.peer_losses.fetch_add(1, Ordering::Relaxed);
        record_topology(shared, node.clone(), TopologyState::Lost);
        shared
            .owners
            .lock()
            .unwrap()
            .retain(|_, owner| owner != node);
    }
}

fn record_topology(shared: &Shared, peer: NodeId, state: TopologyState) {
    let mut events = locked(&shared.topology);
    let sequence = events.len() as u64 + 1;
    events.push(TopologyEvent {
        sequence,
        peer,
        state,
    });
}

fn merge_snapshot(shared: &Shared, snapshot: BTreeMap<String, NodeId>) {
    let mut moved = Vec::new();
    {
        let mut owners = shared.owners.lock().unwrap();
        for (session, remote) in snapshot {
            match owners.get(&session) {
                Some(local) if local <= &remote => {}
                Some(local) => {
                    if local == &shared.node {
                        moved.push(session.clone());
                    }
                    owners.insert(session, remote);
                }
                None => {
                    owners.insert(session, remote);
                }
            }
        }
    }
    for session in moved {
        close_moved_sockets(shared, &session);
    }
}

fn close_moved_sockets(shared: &Shared, session: &str) {
    if let Some(sockets) = shared.sockets.lock().unwrap().remove(session) {
        for sender in sockets.into_values() {
            let _ = sender.control.send(SocketClose::OwnerMoved);
        }
    }
}

fn spawn_websocket_listener(shared: Arc<Shared>, listener: TcpListener) -> JoinHandle<()> {
    thread::spawn(move || {
        while shared.running.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let connection = Arc::clone(&shared);
                    thread::spawn(move || serve_websocket(&connection, stream));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(POLL_INTERVAL);
                }
                Err(_) => return,
            }
        }
    })
}

#[allow(clippy::result_large_err)]
fn serve_websocket(shared: &Arc<Shared>, mut stream: TcpStream) {
    configure_websocket_stream(&stream);
    if serve_operation(shared, &mut stream) {
        return;
    }
    let Some((mut socket, connection)) = accept_connection(shared, stream) else {
        return;
    };
    let _ = socket.get_mut().set_read_timeout(Some(POLL_INTERVAL));
    let session = connection.session.clone();
    let socket_id = shared.next_socket.fetch_add(1, Ordering::Relaxed);
    let (frame_sender, frame_receiver) = mpsc::sync_channel(32);
    let (control_sender, control_receiver) = mpsc::channel();
    register_socket(
        shared,
        socket_id,
        connection.clone(),
        SocketSender {
            frames: frame_sender,
            control: control_sender,
        },
    );
    let mut control_wait_started = None;

    while shared.running.load(Ordering::Relaxed) {
        match control_receiver.try_recv() {
            Ok(reason) => {
                close_socket(&mut socket, &reason);
                break;
            }
            Err(mpsc::TryRecvError::Disconnected | mpsc::TryRecvError::Empty) => {}
        }
        if connection.kind == ConnectionKind::Control {
            if has_unattached_client(shared, &session) {
                let started = control_wait_started.get_or_insert_with(std::time::Instant::now);
                if started.elapsed() >= shared.config.control_heartbeat_timeout {
                    close_socket(&mut socket, &SocketClose::ControlUnresponsive);
                    break;
                }
            } else {
                control_wait_started = None;
            }
        }
        match frame_receiver.try_recv() {
            Ok(frame) => {
                if socket.send(frame).is_err() {
                    if let Ok(reason) = control_receiver.try_recv() {
                        close_socket(&mut socket, &reason);
                    }
                    break;
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                let frame = Message::Text(text);
                if connection.kind == ConnectionKind::Control {
                    control_wait_started = Some(std::time::Instant::now());
                }
                if frame_exceeds_limit(shared, &connection, &frame) {
                    close_socket(&mut socket, &SocketClose::MessageTooLarge);
                    break;
                }
                if rejects_client_handshake(&connection, &frame) {
                    close_socket(&mut socket, &SocketClose::InvalidHandshake);
                    break;
                }
                broadcast(shared, &connection, socket_id, &frame);
            }
            Ok(Message::Binary(bytes)) => {
                let frame = Message::Binary(bytes);
                if frame_exceeds_limit(shared, &connection, &frame) {
                    close_socket(&mut socket, &SocketClose::MessageTooLarge);
                    break;
                }
                if rejects_client_handshake(&connection, &frame) {
                    close_socket(&mut socket, &SocketClose::InvalidHandshake);
                    break;
                }
                broadcast(shared, &connection, socket_id, &frame);
            }
            Ok(Message::Close(_)) => break,
            Ok(Message::Ping(bytes)) => {
                let _ = socket.send(Message::Pong(bytes));
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(tungstenite::Error::Capacity(_)) => {
                close_socket(&mut socket, &SocketClose::MessageTooLarge);
                break;
            }
            Err(_) => break,
        }
    }
    unregister_socket(shared, &session, socket_id);
}

fn configure_websocket_stream(stream: &TcpStream) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(SOCKET_TIMEOUT));
}

#[allow(clippy::result_large_err)]
fn accept_connection(
    shared: &Arc<Shared>,
    stream: TcpStream,
) -> Option<(tungstenite::WebSocket<TcpStream>, AcceptedConnection)> {
    let accepted_connection = Arc::new(Mutex::new(None));
    let callback_connection = Arc::clone(&accepted_connection);
    let callback_shared = Arc::clone(shared);
    let websocket_config = tungstenite::protocol::WebSocketConfig::default()
        .max_frame_size(Some(shared.config.max_frame_payload_bytes))
        .max_message_size(Some(shared.config.max_frame_payload_bytes));
    let socket = tungstenite::accept_hdr_with_config(
        stream,
        move |request: &Request, response: Response| {
            authorize_upgrade(&callback_shared, request, response, &callback_connection)
        },
        Some(websocket_config),
    )
    .ok()?;
    let connection = accepted_connection.lock().unwrap().clone()?;
    Some((socket, connection))
}

fn has_unattached_client(shared: &Shared, session: &str) -> bool {
    let connections = locked(&shared.connections);
    connections.values().any(|candidate| {
        let ConnectionKind::Client(connection_id) = &candidate.kind else {
            return false;
        };
        candidate.session == session
            && !connections.values().any(|data| {
                data.session == session && data.kind == ConnectionKind::Data(connection_id.clone())
            })
    })
}

fn frame_exceeds_limit(shared: &Shared, connection: &AcceptedConnection, frame: &Message) -> bool {
    let limit = if connection.kind == ConnectionKind::Control {
        shared.config.max_control_payload_bytes
    } else {
        shared.config.max_frame_payload_bytes
    };
    match frame {
        Message::Text(text) => text.len() > limit,
        Message::Binary(bytes) => bytes.len() > limit,
        _ => false,
    }
}

fn rejects_client_handshake(connection: &AcceptedConnection, frame: &Message) -> bool {
    if !connection.client {
        return false;
    }
    let payload = match frame {
        Message::Text(text) => text.as_bytes(),
        Message::Binary(bytes) => bytes.as_ref(),
        _ => return false,
    };
    let Ok(payload) = std::str::from_utf8(payload) else {
        return false;
    };
    let Some(handshake_type) = json_string_field(payload, "type") else {
        return false;
    };
    if !matches!(handshake_type.as_str(), "hello" | "e2ee_hello") {
        return false;
    }
    json_string_field(payload, "key").is_none_or(|encoded| match import_public_key(&encoded) {
        Ok(key) => !canonical_x25519_coordinate(&key) || derive_shared_key(&[7; 32], &key).is_err(),
        Err(_) => true,
    })
}

fn json_string_field(payload: &str, field: &str) -> Option<String> {
    let payload = payload.trim();
    let inner = payload.strip_prefix('{')?.strip_suffix('}')?;
    let bytes = inner.as_bytes();
    let mut index = 0;
    let mut depth = 0_usize;
    while index < bytes.len() {
        match bytes[index] {
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.saturating_sub(1),
            b'"' => {
                let end = json_string_end(bytes, index + 1)?;
                if depth == 0
                    && json_key_position(bytes, index)
                    && decode_json_string(&inner[index + 1..end]).as_deref() == Some(field)
                {
                    let mut value = end + 1;
                    while bytes.get(value).is_some_and(u8::is_ascii_whitespace) {
                        value += 1;
                    }
                    if bytes.get(value) != Some(&b':') {
                        return None;
                    }
                    value += 1;
                    while bytes.get(value).is_some_and(u8::is_ascii_whitespace) {
                        value += 1;
                    }
                    if bytes.get(value) != Some(&b'"') {
                        return None;
                    }
                    let value_end = json_string_end(bytes, value + 1)?;
                    return decode_json_string(&inner[value + 1..value_end]);
                }
                index = end;
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn decode_json_string(encoded: &str) -> Option<String> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            0x00..=0x1f => return None,
            b'\\' => {
                index += 1;
                match *bytes.get(index)? {
                    b'"' => decoded.push(b'"'),
                    b'\\' => decoded.push(b'\\'),
                    b'/' => decoded.push(b'/'),
                    b'b' => decoded.push(0x08),
                    b'f' => decoded.push(0x0c),
                    b'n' => decoded.push(b'\n'),
                    b'r' => decoded.push(b'\r'),
                    b't' => decoded.push(b'\t'),
                    b'u' => {
                        let high = decode_hex_quad(bytes, index + 1)?;
                        index += 4;
                        let scalar = if (0xd800..=0xdbff).contains(&high) {
                            if bytes.get(index + 1..index + 3) != Some(b"\\u") {
                                return None;
                            }
                            let low = decode_hex_quad(bytes, index + 3)?;
                            if !(0xdc00..=0xdfff).contains(&low) {
                                return None;
                            }
                            index += 6;
                            0x1_0000 + (u32::from(high - 0xd800) << 10) + u32::from(low - 0xdc00)
                        } else if (0xdc00..=0xdfff).contains(&high) {
                            return None;
                        } else {
                            u32::from(high)
                        };
                        let character = char::from_u32(scalar)?;
                        let mut buffer = [0_u8; 4];
                        decoded.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
                    }
                    _ => return None,
                }
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8(decoded).ok()
}

fn decode_hex_quad(bytes: &[u8], start: usize) -> Option<u16> {
    let digits = bytes.get(start..start + 4)?;
    digits.iter().try_fold(0_u16, |value, digit| {
        Some((value << 4) | u16::try_from(char::from(*digit).to_digit(16)?).ok()?)
    })
}

fn json_string_end(bytes: &[u8], mut index: usize) -> Option<usize> {
    let mut escaped = false;
    while let Some(byte) = bytes.get(index) {
        if escaped {
            escaped = false;
        } else if *byte == b'\\' {
            escaped = true;
        } else if *byte == b'"' {
            return Some(index);
        }
        index += 1;
    }
    None
}

fn json_key_position(bytes: &[u8], index: usize) -> bool {
    let prefix = &bytes[..index];
    let Some(previous) = prefix.iter().rfind(|byte| !byte.is_ascii_whitespace()) else {
        return true;
    };
    if *previous != b',' {
        return false;
    }
    true
}

fn canonical_x25519_coordinate(key: &[u8; 32]) -> bool {
    key[31] < 0x7f
        || (key[31] == 0x7f && (key[1..31].iter().any(|byte| *byte != 0xff) || key[0] < 0xed))
}

fn serve_operation(shared: &Shared, stream: &mut TcpStream) -> bool {
    let mut preview = [0_u8; 512];
    let Ok(received) = stream.peek(&mut preview) else {
        return false;
    };
    let Some(first_line_end) = preview[..received]
        .windows(2)
        .position(|pair| pair == b"\r\n")
    else {
        return false;
    };
    let first_line = String::from_utf8_lossy(&preview[..first_line_end]);
    let Some(path) = first_line
        .strip_prefix("GET ")
        .and_then(|line| line.split_once(' ').map(|(path, _)| path))
    else {
        return false;
    };
    if !matches!(path, "/health" | "/ready" | "/metrics") {
        return false;
    }
    let (status, reason, content_type, body) = operation_parts(shared, path);
    let mut request = [0_u8; 2_048];
    let _ = stream.read(&mut request);
    let _ = write!(
        stream,
        concat!(
            "HTTP/1.1 {} {}\r\n",
            "content-type: {}\r\n",
            "content-length: {}\r\n",
            "connection: close\r\n\r\n",
            "{}"
        ),
        status,
        reason,
        content_type,
        body.len(),
        body
    );
    let _ = stream.flush();
    true
}

#[allow(clippy::result_large_err)]
fn authorize_upgrade(
    shared: &Shared,
    request: &Request,
    response: Response,
    accepted_connection: &Mutex<Option<AcceptedConnection>>,
) -> Result<Response, ErrorResponse> {
    if request.uri().path() != "/ws" {
        return operation_response(shared, request.uri().path());
    }
    let mut connection = parse_connection(request)?;
    let session = connection.session.as_str();
    let (owner, claimed_here) = {
        let mut owners = shared.owners.lock().unwrap();
        if let Some(owner) = owners.get(session) {
            (owner.clone(), false)
        } else {
            owners.insert(session.to_owned(), shared.node.clone());
            (shared.node.clone(), true)
        }
    };
    if owner != shared.node {
        shared.reroute_responses.fetch_add(1, Ordering::Relaxed);
        return Err(Response::builder()
            .status(409)
            .header("x-reroute-target", owner.as_str())
            .body(Some("Session owned elsewhere".to_owned()))
            .unwrap());
    }
    if shared.draining.load(Ordering::Relaxed) {
        if claimed_here {
            locked(&shared.owners).remove(session);
        }
        return handshake_error(503, "draining");
    }
    {
        let mut admissions = locked(&shared.admissions);
        if *admissions >= shared.config.max_websockets {
            shared.connection_rejections.fetch_add(1, Ordering::Relaxed);
            if claimed_here {
                locked(&shared.owners).remove(session);
            }
            return handshake_error(503, "Relay connection capacity");
        }
        *admissions += 1;
        connection.admitted = true;
    }
    *accepted_connection.lock().unwrap() = Some(connection);
    Ok(response)
}

#[allow(clippy::result_large_err)]
fn operation_response<T>(shared: &Shared, path: &str) -> Result<T, ErrorResponse> {
    let (status, _reason, content_type, body) = operation_parts(shared, path);
    Err(Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Some(body))
        .unwrap())
}

fn operation_parts(shared: &Shared, path: &str) -> (u16, &'static str, &'static str, String) {
    match path {
        "/health" => (
            200,
            "OK",
            "application/json",
            r#"{"status":"ok"}"#.to_owned(),
        ),
        "/ready" if ready(shared) => (
            200,
            "OK",
            "application/json",
            r#"{"status":"ready"}"#.to_owned(),
        ),
        "/ready" => (
            503,
            "Service Unavailable",
            "application/json",
            r#"{"status":"unready"}"#.to_owned(),
        ),
        "/metrics" => (
            200,
            "OK",
            "text/plain; version=0.0.4",
            render_metrics(shared),
        ),
        _ => (404, "Not Found", "text/plain", "not found\n".to_owned()),
    }
}

fn ready(shared: &Shared) -> bool {
    let visible_nodes = 1 + locked(&shared.peers)
        .values()
        .filter(|peer| peer.enabled && peer.available)
        .count();
    !shared.draining.load(Ordering::Relaxed)
        && visible_nodes >= shared.config.minimum_cluster_size
        && *locked(&shared.admissions) < shared.config.max_websockets
}

fn render_metrics(shared: &Shared) -> String {
    let active = *locked(&shared.admissions);
    let sessions = locked(&shared.owners)
        .values()
        .filter(|owner| *owner == &shared.node)
        .count();
    format!(
        concat!(
            "# TYPE spocky_relay_ready gauge\n",
            "spocky_relay_ready {}\n",
            "# TYPE spocky_relay_draining gauge\n",
            "spocky_relay_draining {}\n",
            "# TYPE spocky_relay_active_websockets gauge\n",
            "spocky_relay_active_websockets {}\n",
            "# TYPE spocky_relay_active_sessions gauge\n",
            "spocky_relay_active_sessions {}\n",
            "# TYPE spocky_relay_connection_rejections_total counter\n",
            "spocky_relay_connection_rejections_total {}\n",
            "# TYPE spocky_relay_reroute_responses_total counter\n",
            "spocky_relay_reroute_responses_total {}\n",
            "# TYPE spocky_relay_frames_forwarded_total counter\n",
            "spocky_relay_frames_forwarded_total {}\n",
            "# TYPE spocky_relay_bytes_forwarded_total counter\n",
            "spocky_relay_bytes_forwarded_total {}\n",
            "# TYPE spocky_relay_peer_losses_total counter\n",
            "spocky_relay_peer_losses_total {}\n",
            "# TYPE spocky_relay_ingress_reserved_bytes gauge\n",
            "spocky_relay_ingress_reserved_bytes {}\n"
        ),
        usize::from(ready(shared)),
        usize::from(shared.draining.load(Ordering::Relaxed)),
        active,
        sessions,
        shared.connection_rejections.load(Ordering::Relaxed),
        shared.reroute_responses.load(Ordering::Relaxed),
        shared.frames_forwarded.load(Ordering::Relaxed),
        shared.bytes_forwarded.load(Ordering::Relaxed),
        shared.peer_losses.load(Ordering::Relaxed),
        *locked(&shared.ingress_reserved_bytes),
    )
}

#[allow(clippy::result_large_err)]
fn parse_connection(request: &Request) -> Result<AcceptedConnection, ErrorResponse> {
    let fields = request
        .uri()
        .query()
        .map(|query| {
            query
                .split('&')
                .filter_map(|field| field.split_once('='))
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    if let Some(session) = fields.get("session") {
        return Ok(AcceptedConnection {
            session: (*session).to_owned(),
            kind: ConnectionKind::Legacy,
            client: false,
            admitted: false,
        });
    }
    let role = fields.get("role").copied().unwrap_or_default();
    if !matches!(role, "server" | "client") {
        return handshake_error(400, "Missing or invalid role parameter");
    }
    let Some(server_id) = fields
        .get("serverId")
        .copied()
        .filter(|value| !value.is_empty())
    else {
        return handshake_error(400, "Missing serverId parameter");
    };
    if server_id.len() > 256 {
        return handshake_error(400, "serverId is too long");
    }
    let version = fields.get("v").copied().unwrap_or("1").trim();
    if !matches!(version, "" | "1" | "2") {
        return handshake_error(400, "Invalid v parameter (expected 1 or 2)");
    }
    if version != "2" {
        return Ok(AcceptedConnection {
            session: server_id.to_owned(),
            kind: ConnectionKind::Legacy,
            client: role == "client",
            admitted: false,
        });
    }
    let connection_id = fields
        .get("connectionId")
        .copied()
        .unwrap_or_default()
        .trim();
    if connection_id.len() > 256 {
        return handshake_error(400, "connectionId is too long");
    }
    let kind = match (role, connection_id) {
        ("server", "") => ConnectionKind::Control,
        ("server", value) => ConnectionKind::Data(value.to_owned()),
        ("client", "") => ConnectionKind::Client(format!(
            "conn_{:016x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        )),
        ("client", value) => ConnectionKind::Client(value.to_owned()),
        _ => unreachable!(),
    };
    Ok(AcceptedConnection {
        session: server_id.to_owned(),
        kind,
        client: role == "client",
        admitted: false,
    })
}

#[allow(clippy::result_large_err)]
fn handshake_error<T>(status: u16, message: &str) -> Result<T, ErrorResponse> {
    Err(Response::builder()
        .status(status)
        .body(Some(message.to_owned()))
        .unwrap())
}

fn register_socket(
    shared: &Shared,
    socket_id: u64,
    connection: AcceptedConnection,
    sender: SocketSender,
) {
    let session = connection.session.clone();
    match &connection.kind {
        ConnectionKind::Control => {
            let connection_ids = locked(&shared.connections)
                .values()
                .filter_map(|candidate| match &candidate.kind {
                    ConnectionKind::Client(connection_id) if candidate.session == session => {
                        Some(format!(r#"\"{connection_id}\""#))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(",");
            let _ = sender.frames.try_send(Message::Text(
                format!(r#"{{"type":"sync","connectionIds":[{connection_ids}]}}"#).into(),
            ));
        }
        ConnectionKind::Client(_) | ConnectionKind::Legacy => {}
        ConnectionKind::Data(connection_id) => {
            let replacements = locked(&shared.connections)
                .iter()
                .filter_map(|(id, candidate)| {
                    (candidate.session == session
                        && candidate.kind == ConnectionKind::Data(connection_id.clone()))
                    .then_some(*id)
                })
                .collect::<Vec<_>>();
            for replacement in replacements {
                if let Some(previous) = locked(&shared.sockets)
                    .get(&session)
                    .and_then(|sockets| sockets.get(&replacement))
                {
                    let _ = previous.control.send(SocketClose::Replaced);
                }
                if let Some(sockets) = locked(&shared.sockets).get_mut(&session) {
                    sockets.remove(&replacement);
                }
                let replaced = locked(&shared.connections).remove(&replacement);
                release_admission(shared, replaced.as_ref());
            }
            if let Some(mut pending) =
                locked(&shared.pending).remove(&(session.clone(), connection_id.clone()))
            {
                release_ingress(shared, pending.bytes);
                while let Some(frame) = pending.frames.pop_front() {
                    let _ = sender.frames.try_send(frame);
                }
            }
        }
    }
    locked(&shared.sockets)
        .entry(session.clone())
        .or_default()
        .insert(socket_id, sender);
    locked(&shared.connections).insert(socket_id, connection);
    let client_id = match &locked(&shared.connections)
        .get(&socket_id)
        .expect("registered connection")
        .kind
    {
        ConnectionKind::Client(connection_id) => Some(connection_id.clone()),
        _ => None,
    };
    if let Some(connection_id) = client_id {
        notify_controls(
            shared,
            &session,
            &Message::Text(
                format!(r#"{{"type":"connected","connectionId":"{connection_id}"}}"#).into(),
            ),
        );
    }
}

fn notify_controls(shared: &Shared, session: &str, frame: &Message) {
    let controls = locked(&shared.connections)
        .iter()
        .filter_map(|(socket_id, connection)| {
            (connection.session == session && connection.kind == ConnectionKind::Control)
                .then_some(*socket_id)
        })
        .collect::<Vec<_>>();
    if let Some(sockets) = locked(&shared.sockets).get(session) {
        for socket_id in controls {
            if let Some(sender) = sockets.get(&socket_id) {
                let _ = sender.frames.try_send(frame.clone());
            }
        }
    }
}

fn broadcast(shared: &Shared, connection: &AcceptedConnection, source: u64, frame: &Message) {
    let session = connection.session.as_str();
    if connection.kind == ConnectionKind::Control {
        if matches!(frame, Message::Text(text) if text.contains(r#""type":"ping""#)) {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            send_to_socket(
                shared,
                session,
                source,
                Message::Text(format!(r#"{{"type":"pong","ts":{timestamp}}}"#).into()),
            );
        }
        return;
    }
    shared.frames_forwarded.fetch_add(1, Ordering::Relaxed);
    let frame_bytes = match frame {
        Message::Text(text) => text.len(),
        Message::Binary(bytes) => bytes.len(),
        _ => 0,
    };
    shared
        .bytes_forwarded
        .fetch_add(frame_bytes as u64, Ordering::Relaxed);
    let targets = locked(&shared.connections)
        .iter()
        .filter_map(|(socket_id, candidate)| {
            if *socket_id == source || candidate.session != session {
                return None;
            }
            let matches = match (&connection.kind, &candidate.kind) {
                (ConnectionKind::Legacy, ConnectionKind::Legacy) => true,
                (ConnectionKind::Client(left), ConnectionKind::Data(right))
                | (ConnectionKind::Data(left), ConnectionKind::Client(right)) => left == right,
                _ => false,
            };
            matches.then_some(*socket_id)
        })
        .collect::<Vec<_>>();
    if targets.is_empty()
        && let ConnectionKind::Client(connection_id) = &connection.kind
    {
        let overflow = {
            let mut pending = locked(&shared.pending);
            let queue = pending
                .entry((session.to_owned(), connection_id.clone()))
                .or_default();
            if queue.frames.len() >= 32 {
                Some(SocketClose::DataRouteUnavailable)
            } else if reserve_ingress(shared, frame_bytes) {
                queue.frames.push_back(frame.clone());
                queue.bytes += frame_bytes;
                None
            } else {
                Some(SocketClose::RelayIngressCapacity)
            }
        };
        if let Some(reason) = overflow
            && let Some(sender) = locked(&shared.sockets)
                .get(session)
                .and_then(|sockets| sockets.get(&source))
        {
            let _ = sender.control.send(reason);
        }
        return;
    }
    let mut sessions = shared.sockets.lock().unwrap();
    let Some(sockets) = sessions.get_mut(session) else {
        return;
    };
    let mut remove = Vec::new();
    for socket_id in targets {
        if let Some(sender) = sockets.get(&socket_id) {
            match sender.frames.try_send(frame.clone()) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(_)) => {
                    let _ = sender.control.send(SocketClose::SlowConsumer);
                    remove.push(socket_id);
                }
                Err(mpsc::TrySendError::Disconnected(_)) => remove.push(socket_id),
            }
        }
    }
    let sockets = sessions.get_mut(session).expect("session still registered");
    for socket_id in remove {
        sockets.remove(&socket_id);
    }
    if sockets.is_empty() {
        sessions.remove(session);
    }
}

fn send_to_socket(shared: &Shared, session: &str, socket_id: u64, frame: Message) {
    if let Some(sender) = locked(&shared.sockets)
        .get(session)
        .and_then(|sockets| sockets.get(&socket_id))
    {
        let _ = sender.frames.try_send(frame);
    }
}

fn close_socket(socket: &mut tungstenite::WebSocket<TcpStream>, reason: &SocketClose) {
    let (code, reason) = match reason {
        SocketClose::OwnerMoved => (CloseCode::Restart, "Session owner moved"),
        SocketClose::SlowConsumer => (CloseCode::Again, "Slow consumer"),
        SocketClose::DataRouteUnavailable => (CloseCode::Again, "Data route unavailable"),
        SocketClose::InvalidHandshake => (CloseCode::Policy, "Invalid handshake key"),
        SocketClose::MessageTooLarge => (CloseCode::Size, ""),
        SocketClose::ControlUnresponsive => (CloseCode::Error, "Control unresponsive"),
        SocketClose::RelayIngressCapacity => (CloseCode::Again, "Relay ingress capacity"),
        SocketClose::ClientDisconnected => (CloseCode::Away, "Client disconnected"),
        SocketClose::Replaced => (CloseCode::Policy, "Replaced by new connection"),
    };
    let _ = socket.close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }));
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap()
}

fn unregister_socket(shared: &Shared, session: &str, socket_id: u64) {
    let connection = locked(&shared.connections).remove(&socket_id);
    release_admission(shared, connection.as_ref());
    if let Some(AcceptedConnection {
        kind: ConnectionKind::Client(connection_id),
        ..
    }) = connection
    {
        let data_ids = locked(&shared.connections)
            .iter()
            .filter_map(|(id, candidate)| {
                (candidate.session == session
                    && candidate.kind == ConnectionKind::Data(connection_id.clone()))
                .then_some(*id)
            })
            .collect::<Vec<_>>();
        if let Some(sockets) = locked(&shared.sockets).get(session) {
            for data_id in data_ids {
                if let Some(data) = sockets.get(&data_id) {
                    let _ = data.control.send(SocketClose::ClientDisconnected);
                }
            }
        }
        notify_controls(
            shared,
            session,
            &Message::Text(
                format!(r#"{{"type":"disconnected","connectionId":"{connection_id}"}}"#).into(),
            ),
        );
        if let Some(pending) = locked(&shared.pending).remove(&(session.to_owned(), connection_id))
        {
            release_ingress(shared, pending.bytes);
        }
    }
    let mut sessions = shared.sockets.lock().unwrap();
    if let Some(sockets) = sessions.get_mut(session) {
        sockets.remove(&socket_id);
        if sockets.is_empty() {
            sessions.remove(session);
        }
    }
}

fn release_admission(shared: &Shared, connection: Option<&AcceptedConnection>) {
    if connection.is_some_and(|connection| connection.admitted) {
        let mut admissions = locked(&shared.admissions);
        *admissions = admissions.saturating_sub(1);
    }
}

fn reserve_ingress(shared: &Shared, bytes: usize) -> bool {
    let mut reserved = locked(&shared.ingress_reserved_bytes);
    if reserved.saturating_add(bytes) > shared.config.ingress_budget_bytes {
        false
    } else {
        *reserved += bytes;
        true
    }
}

fn release_ingress(shared: &Shared, bytes: usize) {
    let mut reserved = locked(&shared.ingress_reserved_bytes);
    *reserved = reserved.saturating_sub(bytes);
}
