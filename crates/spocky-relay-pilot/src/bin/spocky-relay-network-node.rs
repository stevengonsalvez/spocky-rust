use std::io::{self, BufRead, Write};
use std::net::SocketAddr;

use spocky_relay_pilot::{NetworkConfig, NetworkNode, NodeId};

fn main() {
    let Some(local_node) = std::env::args().nth(1) else {
        eprintln!("usage: spocky-relay-network-node NODE_ID");
        std::process::exit(2);
    };
    let config = NetworkConfig {
        minimum_cluster_size: environment_usize("SPOCKY_RELAY_MIN_CLUSTER_SIZE", 1),
        max_websockets: environment_usize("SPOCKY_RELAY_MAX_WEBSOCKETS", 1_024),
        ..NetworkConfig::default()
    };
    let node = NetworkNode::bind_with_config(NodeId::from(local_node.as_str()), config)
        .expect("bind relay peer and websocket listeners");
    if let Ok(peers) = std::env::var("SPOCKY_RELAY_PEERS") {
        node.discover_peers(
            peers
                .split(',')
                .filter(|value| !value.is_empty())
                .map(|peer| {
                    let (node, address) = peer
                        .split_once('=')
                        .expect("SPOCKY_RELAY_PEERS entries use NODE=ADDRESS");
                    (
                        NodeId::from(node),
                        address
                            .parse::<SocketAddr>()
                            .expect("SPOCKY_RELAY_PEERS address must be a socket address"),
                    )
                }),
        );
    }
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

fn environment_usize(name: &str, default: usize) -> usize {
    std::env::var(name).map_or(default, |value| {
        value
            .parse()
            .unwrap_or_else(|_| panic!("{name} must be an unsigned integer"))
    })
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
