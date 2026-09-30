use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{self, BufRead, Write};

use paseo_relay_pilot::{
    ClaimDecision, ClusterConfig, LinkConfig, LinkId, NodeConfig, NodeId, OpaqueCiphertext,
    OwnerToken, RelayError, RelayPilot, SessionId, ShedReason,
};

const NODE_MAX_LINKS: usize = 16;

fn main() {
    let Some(local_node) = std::env::args().nth(1) else {
        eprintln!("usage: paseo-relay-node NODE_ID");
        std::process::exit(2);
    };
    let mut runtime = NodeRuntime::new();
    println!("READY\t{local_node}\t{}", std::process::id());
    io::stdout().flush().expect("flush startup response");

    for line in io::stdin().lock().lines() {
        let response = match line {
            Ok(line) => runtime.handle(&line),
            Err(error) => format!("ERROR\tread\t{error}"),
        };
        println!("{response}");
        io::stdout().flush().expect("flush command response");
    }
}

struct NodeRuntime {
    relay: RelayPilot,
    issued_owners: BTreeMap<SessionId, OwnerToken>,
}

impl NodeRuntime {
    fn new() -> Self {
        let mut relay = RelayPilot::new(ClusterConfig {
            minimum_cluster_size: 1,
        });
        for node in ["alpha", "beta", "gamma"] {
            relay.add_node(
                NodeId::from(node),
                NodeConfig {
                    max_links: NODE_MAX_LINKS,
                },
            );
        }
        Self {
            relay,
            issued_owners: BTreeMap::new(),
        }
    }

    fn handle(&mut self, input: &str) -> String {
        let fields: Vec<&str> = input.split('\t').collect();
        match fields.as_slice() {
            ["CLAIM", session, candidates] => self.claim(session, candidates),
            ["ROUTE", session, landing] => self.route(session, landing),
            ["OPEN", link, session, max_bytes] => self.open(link, session, max_bytes),
            ["ENQUEUE", link, session, ciphertext] => self.enqueue(link, session, ciphertext),
            ["DEQUEUE", link] => self.dequeue(link),
            ["LOSE", node] => self.lose(node),
            _ => "ERROR\tinvalid-command".to_owned(),
        }
    }

    fn claim(&mut self, session: &str, candidates: &str) -> String {
        let session = SessionId::from(session);
        let candidates = candidates.split(',').map(NodeId::from);
        match self.relay.converge_claims(session.clone(), candidates) {
            Ok(owner) => {
                let response = format!("OWNER\t{}\t{}", owner.node.as_str(), owner.generation);
                self.issued_owners.insert(session, owner);
                response
            }
            Err(error) => error_response(error),
        }
    }

    fn route(&self, session: &str, landing: &str) -> String {
        match self
            .relay
            .route(&SessionId::from(session), &NodeId::from(landing))
        {
            ClaimDecision::Local(owner) => {
                format!("LOCAL\t{}\t{}", owner.node.as_str(), owner.generation)
            }
            ClaimDecision::Reroute { target, owner } => {
                format!("REROUTE\t{}\t{}", target.as_str(), owner.generation)
            }
            ClaimDecision::Unowned => "UNOWNED".to_owned(),
        }
    }

    fn open(&mut self, link: &str, session: &str, max_bytes: &str) -> String {
        let session = SessionId::from(session);
        let Some(owner) = self.issued_owners.get(&session).cloned() else {
            return "ERROR\tunowned".to_owned();
        };
        let Ok(max_queued_bytes) = max_bytes.parse::<usize>() else {
            return "ERROR\tinvalid-limit".to_owned();
        };
        let link = LinkId::from(link);
        match self.relay.open_link(
            link.clone(),
            &session,
            owner.clone(),
            LinkConfig { max_queued_bytes },
        ) {
            Ok(()) => format!(
                "OPENED\t{}\t{}\t{}",
                link.as_str(),
                owner.node.as_str(),
                owner.generation
            ),
            Err(error) => error_response(error),
        }
    }

    fn enqueue(&mut self, link: &str, session: &str, ciphertext: &str) -> String {
        let session = SessionId::from(session);
        let Some(owner) = self.issued_owners.get(&session) else {
            return "ERROR\tunowned".to_owned();
        };
        let Ok(bytes) = decode_hex(ciphertext) else {
            return "ERROR\tinvalid-hex".to_owned();
        };
        let wire_bytes = bytes.len();
        let link = LinkId::from(link);
        match self
            .relay
            .enqueue(&link, owner, OpaqueCiphertext::from_bytes(bytes))
        {
            Ok(()) => {
                let sequence = self
                    .relay
                    .observations()
                    .last()
                    .map_or(0, |observation| observation.sequence);
                format!("QUEUED\t{sequence}\t{wire_bytes}")
            }
            Err(RelayError::Shed(reason)) => format!(
                "SHED\t1013\t{}",
                match reason {
                    ShedReason::SlowConsumer => "SlowConsumer",
                    ShedReason::MemoryPressure => "MemoryPressure",
                }
            ),
            Err(error) => error_response(error),
        }
    }

    fn dequeue(&mut self, link: &str) -> String {
        match self.relay.dequeue(&LinkId::from(link)) {
            Some(frame) => format!("FRAME\t{}", encode_hex(frame.as_bytes())),
            None => "EMPTY".to_owned(),
        }
    }

    fn lose(&mut self, node: &str) -> String {
        let closed = self.relay.lose_node(&NodeId::from(node));
        format!("LOST\t{node}\t{}", closed.len())
    }
}

fn error_response(error: RelayError) -> String {
    format!("ERROR\t{error:?}")
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to String cannot fail");
            output
        },
    )
}

fn decode_hex(value: &str) -> Result<Vec<u8>, ()> {
    if !value.len().is_multiple_of(2) {
        return Err(());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).map_err(|_| ())?;
            u8::from_str_radix(pair, 16).map_err(|_| ())
        })
        .collect()
}
