use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use tungstenite::client;
use tungstenite::protocol::{CloseFrame, Message, frame::coding::CloseCode};

const DEADLINE: Duration = Duration::from_secs(3);

struct NetworkProcess {
    child: Child,
    stdin: ChildStdin,
    responses: Receiver<String>,
    peer_address: SocketAddr,
    websocket_address: SocketAddr,
}

impl NetworkProcess {
    fn spawn(node: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_paseo-relay-network-node"))
            .arg(node)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("network relay process starts");
        let stdin = child.stdin.take().expect("network relay stdin");
        let stdout = child.stdout.take().expect("network relay stdout");
        let (sender, responses) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let ready = responses
            .recv_timeout(DEADLINE)
            .expect("network relay ready");
        let fields = ready.split('\t').collect::<Vec<_>>();
        assert_eq!(fields.first(), Some(&"READY"));
        assert_eq!(fields.get(1), Some(&node));
        Self {
            child,
            stdin,
            responses,
            peer_address: fields[3].parse().expect("peer address"),
            websocket_address: fields[4].parse().expect("websocket address"),
        }
    }

    fn request(&mut self, request: &str) -> String {
        writeln!(self.stdin, "{request}").expect("write network relay command");
        self.stdin.flush().expect("flush network relay command");
        self.responses
            .recv_timeout(DEADLINE)
            .expect("network relay response")
    }

    fn connect(&mut self, node: &str, address: SocketAddr) {
        assert_eq!(
            self.request(&format!("PEER\t{node}\t{address}")),
            format!("PEERED\t{node}\t{address}")
        );
    }

    fn owner(&mut self, session: &str) -> String {
        self.request(&format!("OWNER\t{session}"))
    }

    fn disconnect(&mut self, node: &str) {
        assert_eq!(
            self.request(&format!("DISCONNECT\t{node}")),
            format!("DISCONNECTED\t{node}")
        );
    }

    fn reconnect(&mut self, node: &str) {
        assert_eq!(
            self.request(&format!("RECONNECT\t{node}")),
            format!("RECONNECTED\t{node}")
        );
    }

    fn terminate(&mut self) {
        self.child.kill().expect("kill network relay process");
        self.child.wait().expect("wait for network relay process");
    }
}

impl Drop for NetworkProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn independent_network_processes_detect_owner_loss_without_controller_commands() {
    let mut alpha = NetworkProcess::spawn("alpha");
    let mut beta = NetworkProcess::spawn("beta");
    alpha.connect("beta", beta.peer_address);
    beta.connect("alpha", alpha.peer_address);

    let _alpha_socket = websocket(alpha.websocket_address, "failure");
    wait_until(|| beta.owner("failure") == "OWNER\talpha");

    alpha.terminate();
    wait_until(|| beta.owner("failure") == "UNOWNED");
    let _beta_socket = websocket(beta.websocket_address, "failure");
    wait_until(|| beta.owner("failure") == "OWNER\tbeta");
}

#[test]
fn independent_network_processes_heal_partition_close_loser_and_reroute() {
    let mut alpha = NetworkProcess::spawn("alpha");
    let mut beta = NetworkProcess::spawn("beta");
    alpha.connect("beta", beta.peer_address);
    beta.connect("alpha", alpha.peer_address);
    alpha.disconnect("beta");
    beta.disconnect("alpha");

    let _alpha_socket = websocket(alpha.websocket_address, "partition");
    let mut beta_socket = websocket(beta.websocket_address, "partition");
    assert_eq!(alpha.owner("partition"), "OWNER\talpha");
    assert_eq!(beta.owner("partition"), "OWNER\tbeta");

    alpha.reconnect("beta");
    beta.reconnect("alpha");
    wait_until(|| beta.owner("partition") == "OWNER\talpha");
    let close = wait_for_close(&mut beta_socket);
    assert_eq!(close.code, CloseCode::Restart);
    assert_eq!(close.reason, "Session owner moved");

    let error = websocket_result(beta.websocket_address, "partition").expect_err("reroute");
    let tungstenite::Error::Http(response) = error else {
        panic!("expected HTTP reroute")
    };
    assert_eq!(response.status(), 409);
    assert_eq!(response.headers()["x-reroute-target"], "alpha");
}

fn websocket(address: SocketAddr, session: &str) -> tungstenite::WebSocket<std::net::TcpStream> {
    websocket_result(address, session).expect("websocket connects")
}

#[allow(clippy::result_large_err)]
fn websocket_result(
    address: SocketAddr,
    session: &str,
) -> Result<tungstenite::WebSocket<std::net::TcpStream>, tungstenite::Error> {
    let stream = TcpStream::connect(address).expect("connect websocket");
    stream
        .set_read_timeout(Some(DEADLINE))
        .expect("set websocket timeout");
    match client(format!("ws://{address}/ws?session={session}"), stream) {
        Ok((socket, _)) => Ok(socket),
        Err(tungstenite::HandshakeError::Failure(error)) => Err(error),
        Err(tungstenite::HandshakeError::Interrupted(_)) => {
            panic!("blocking handshake cannot be interrupted")
        }
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

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !predicate() {
        assert!(Instant::now() < deadline, "condition missed deadline");
        std::thread::sleep(Duration::from_millis(20));
    }
}
