use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::protocol::{CloseFrame, Message, frame::coding::CloseCode};

use crate::NodeId;

const POLL_INTERVAL: Duration = Duration::from_millis(25);
const SOCKET_TIMEOUT: Duration = Duration::from_millis(100);
const FAILURE_THRESHOLD: u8 = 3;

#[derive(Clone, Debug)]
struct Peer {
    address: SocketAddr,
    enabled: bool,
    failures: u8,
}

type SocketSender = mpsc::SyncSender<Outbound>;

#[derive(Clone, Debug)]
enum Outbound {
    Frame(Message),
    OwnerMoved,
}

struct Shared {
    node: NodeId,
    peers: Mutex<BTreeMap<NodeId, Peer>>,
    owners: Mutex<BTreeMap<String, NodeId>>,
    sockets: Mutex<BTreeMap<String, BTreeMap<u64, SocketSender>>>,
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
        let peer_listener = loopback_listener()?;
        let websocket_listener = loopback_listener()?;
        let peer_address = peer_listener.local_addr()?;
        let websocket_address = websocket_listener.local_addr()?;
        let shared = Arc::new(Shared {
            node,
            peers: Mutex::new(BTreeMap::new()),
            owners: Mutex::new(BTreeMap::new()),
            sockets: Mutex::new(BTreeMap::new()),
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
        locked(&self.shared.peers).insert(
            node,
            Peer {
                address,
                enabled: true,
                failures: 0,
            },
        );
    }

    pub fn disconnect_peer(&self, node: &NodeId) {
        if let Some(peer) = locked(&self.shared.peers).get_mut(node) {
            peer.enabled = false;
            peer.failures = 0;
        }
    }

    pub fn reconnect_peer(&self, node: &NodeId) {
        if let Some(peer) = locked(&self.shared.peers).get_mut(node) {
            peer.enabled = true;
            peer.failures = 0;
        }
    }

    #[must_use]
    pub fn owner(&self, session: &str) -> Option<NodeId> {
        locked(&self.shared.owners).get(session).cloned()
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
    if let Some(peer) = shared.peers.lock().unwrap().get_mut(node) {
        peer.failures = 0;
    }
}

fn mark_peer_failure(shared: &Shared, node: &NodeId) {
    let failed = {
        let mut peers = shared.peers.lock().unwrap();
        let Some(peer) = peers.get_mut(node) else {
            return;
        };
        peer.failures = peer.failures.saturating_add(1);
        peer.failures >= FAILURE_THRESHOLD
    };
    if failed {
        shared
            .owners
            .lock()
            .unwrap()
            .retain(|_, owner| owner != node);
    }
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
            let _ = sender.try_send(Outbound::OwnerMoved);
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
fn serve_websocket(shared: &Arc<Shared>, stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(POLL_INTERVAL));
    let _ = stream.set_write_timeout(Some(SOCKET_TIMEOUT));
    let accepted_session = Arc::new(Mutex::new(None));
    let callback_session = Arc::clone(&accepted_session);
    let callback_shared = Arc::clone(shared);
    let accepted = tungstenite::accept_hdr(stream, move |request: &Request, response: Response| {
        authorize_upgrade(&callback_shared, request, response, &callback_session)
    });
    let Ok(mut socket) = accepted else {
        return;
    };
    let Some(session) = accepted_session.lock().unwrap().clone() else {
        return;
    };
    let socket_id = shared.next_socket.fetch_add(1, Ordering::Relaxed);
    let (sender, receiver) = mpsc::sync_channel(32);
    shared
        .sockets
        .lock()
        .unwrap()
        .entry(session.clone())
        .or_default()
        .insert(socket_id, sender);

    while shared.running.load(Ordering::Relaxed) {
        match receiver.try_recv() {
            Ok(Outbound::Frame(frame)) => {
                if socket.send(frame).is_err() {
                    break;
                }
            }
            Ok(Outbound::OwnerMoved) => {
                let _ = socket.close(Some(CloseFrame {
                    code: CloseCode::Restart,
                    reason: "Session owner moved".into(),
                }));
                break;
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                broadcast(shared, &session, socket_id, &Message::Text(text));
            }
            Ok(Message::Binary(bytes)) => {
                broadcast(shared, &session, socket_id, &Message::Binary(bytes));
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
            Err(_) => break,
        }
    }
    unregister_socket(shared, &session, socket_id);
}

#[allow(clippy::result_large_err)]
fn authorize_upgrade(
    shared: &Shared,
    request: &Request,
    response: Response,
    accepted_session: &Mutex<Option<String>>,
) -> Result<Response, ErrorResponse> {
    let Some(session) = request.uri().query().and_then(|query| {
        query
            .split('&')
            .find_map(|field| field.strip_prefix("session="))
    }) else {
        return Err(Response::builder()
            .status(400)
            .body(Some("missing session".to_owned()))
            .unwrap());
    };
    let owner = {
        let mut owners = shared.owners.lock().unwrap();
        owners
            .entry(session.to_owned())
            .or_insert_with(|| shared.node.clone())
            .clone()
    };
    if owner != shared.node {
        return Err(Response::builder()
            .status(409)
            .header("x-reroute-target", owner.as_str())
            .body(Some("Session owned elsewhere".to_owned()))
            .unwrap());
    }
    *accepted_session.lock().unwrap() = Some(session.to_owned());
    Ok(response)
}

fn broadcast(shared: &Shared, session: &str, source: u64, frame: &Message) {
    let sockets = shared.sockets.lock().unwrap();
    let Some(sockets) = sockets.get(session) else {
        return;
    };
    for (socket_id, sender) in sockets {
        if *socket_id != source {
            let _ = sender.try_send(Outbound::Frame(frame.clone()));
        }
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap()
}

fn unregister_socket(shared: &Shared, session: &str, socket_id: u64) {
    let mut sessions = shared.sockets.lock().unwrap();
    if let Some(sockets) = sessions.get_mut(session) {
        sockets.remove(&socket_id);
        if sockets.is_empty() {
            sessions.remove(session);
        }
    }
}
