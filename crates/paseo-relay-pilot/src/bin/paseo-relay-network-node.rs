use std::io::{self, BufRead, Write};
use std::net::SocketAddr;

use paseo_relay_pilot::{NetworkNode, NodeId};

fn main() {
    let Some(local_node) = std::env::args().nth(1) else {
        eprintln!("usage: paseo-relay-network-node NODE_ID");
        std::process::exit(2);
    };
    let node = NetworkNode::bind(NodeId::from(local_node.as_str()))
        .expect("bind relay peer and websocket listeners");
    println!(
        "READY\t{}\t{}\t{}\t{}",
        local_node,
        std::process::id(),
        node.peer_address(),
        node.websocket_address()
    );
    io::stdout().flush().expect("flush startup response");

    for line in io::stdin().lock().lines() {
        let response = match line {
            Ok(line) => handle(&node, &line),
            Err(error) => format!("ERROR\tread\t{error}"),
        };
        println!("{response}");
        io::stdout().flush().expect("flush command response");
    }
}

fn handle(node: &NetworkNode, input: &str) -> String {
    let fields = input.split('\t').collect::<Vec<_>>();
    match fields.as_slice() {
        ["PEER", peer, address] => match address.parse::<SocketAddr>() {
            Ok(address) => {
                node.connect_peer(NodeId::from(*peer), address);
                format!("PEERED\t{peer}\t{address}")
            }
            Err(_) => "ERROR\tinvalid-address".into(),
        },
        ["OWNER", session] => node.owner(session).map_or_else(
            || "UNOWNED".into(),
            |owner| format!("OWNER\t{}", owner.as_str()),
        ),
        ["DISCONNECT", peer] => {
            node.disconnect_peer(&NodeId::from(*peer));
            format!("DISCONNECTED\t{peer}")
        }
        ["RECONNECT", peer] => {
            node.reconnect_peer(&NodeId::from(*peer));
            format!("RECONNECTED\t{peer}")
        }
        _ => "ERROR\tinvalid-command".into(),
    }
}
