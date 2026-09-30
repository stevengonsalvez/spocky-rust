use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use spocky_crypto::{decrypt, derive_shared_key, encrypt_with_nonce, key_pair_from_secret};

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);

struct RelayNode {
    child: Child,
    stdin: ChildStdin,
    responses: Receiver<String>,
}

impl RelayNode {
    fn spawn(node: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_spocky-relay-node"))
            .arg(node)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("relay node must start");
        let stdin = child.stdin.take().expect("relay node stdin");
        let stdout = child.stdout.take().expect("relay node stdout");
        let (sender, responses) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let node = Self {
            child,
            stdin,
            responses,
        };
        let ready = node.response();
        assert!(ready.starts_with("READY\t"), "unexpected startup: {ready}");
        node
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn request(&mut self, command: &str) -> String {
        writeln!(self.stdin, "{command}").expect("write relay command");
        self.stdin.flush().expect("flush relay command");
        self.response()
    }

    fn response(&self) -> String {
        self.responses
            .recv_timeout(RESPONSE_TIMEOUT)
            .expect("relay response before deadline")
    }

    fn terminate(&mut self) {
        self.child.kill().expect("kill relay node");
        let status = self.child.wait().expect("wait for relay node");
        assert!(!status.success(), "killed relay node must fail");
    }
}

impl Drop for RelayNode {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn independent_node_processes_converge_and_remote_landing_reroutes() {
    let mut alpha = RelayNode::spawn("alpha");
    let mut beta = RelayNode::spawn("beta");
    assert_ne!(alpha.pid(), beta.pid());

    let alpha_claim = alpha.request("CLAIM\tsession-1\tbeta,alpha");
    let beta_claim = beta.request("CLAIM\tsession-1\talpha,beta");
    assert_eq!(alpha_claim, "OWNER\talpha\t1");
    assert_eq!(beta_claim, alpha_claim);
    assert_eq!(beta.request("ROUTE\tsession-1\tbeta"), "REROUTE\talpha\t1");

    println!(
        "runtime ownership: alpha_pid={} beta_pid={} claim={} landing_beta=REROUTE_alpha_1",
        alpha.pid(),
        beta.pid(),
        alpha_claim.replace('\t', "_")
    );
}

#[test]
fn ciphertext_crosses_process_boundary_unchanged_without_plaintext_observation() {
    let alice = key_pair_from_secret([7; 32]);
    let bob = key_pair_from_secret([9; 32]);
    let shared = derive_shared_key(&alice.secret_key, &bob.public_key).unwrap();
    let plaintext = b"runtime secret payload";
    let ciphertext = encrypt_with_nonce(&shared, &[4; 24], plaintext).unwrap();
    let ciphertext_hex = encode_hex(&ciphertext);

    let mut alpha = RelayNode::spawn("alpha");
    assert_eq!(
        alpha.request("CLAIM\topaque-session\talpha,beta"),
        "OWNER\talpha\t1"
    );
    assert_eq!(
        alpha.request("OPEN\topaque-link\topaque-session\t256"),
        "OPENED\topaque-link\talpha\t1"
    );
    let queued = alpha.request(&format!(
        "ENQUEUE\topaque-link\topaque-session\t{ciphertext_hex}"
    ));
    assert_eq!(queued, format!("QUEUED\t1\t{}", ciphertext.len()));
    assert!(!queued.contains("runtime secret payload"));

    let frame = alpha.request("DEQUEUE\topaque-link");
    assert_eq!(frame, format!("FRAME\t{ciphertext_hex}"));
    assert!(!frame.contains("runtime secret payload"));
    let forwarded = decode_hex(frame.strip_prefix("FRAME\t").unwrap());
    assert_eq!(forwarded, ciphertext);
    assert_eq!(decrypt(&shared, &forwarded).unwrap(), plaintext);

    println!(
        "runtime ciphertext: node_pid={} wire_bytes={} ciphertext={ciphertext_hex} plaintext_observed=false",
        alpha.pid(),
        ciphertext.len()
    );
}

#[test]
fn owner_loss_pressure_shedding_and_generation_recovery_are_bounded() {
    let mut alpha = RelayNode::spawn("alpha");
    let mut beta = RelayNode::spawn("beta");
    for node in [&mut alpha, &mut beta] {
        assert_eq!(
            node.request("CLAIM\trecovery-session\talpha,beta"),
            "OWNER\talpha\t1"
        );
    }

    assert_eq!(
        alpha.request("OPEN\tslow-link\trecovery-session\t64"),
        "OPENED\tslow-link\talpha\t1"
    );
    assert_eq!(
        alpha.request(&format!(
            "ENQUEUE\tslow-link\trecovery-session\t{}",
            "a5".repeat(48)
        )),
        "QUEUED\t1\t48"
    );
    assert_eq!(
        alpha.request(&format!(
            "ENQUEUE\tslow-link\trecovery-session\t{}",
            "5a".repeat(32)
        )),
        "SHED\t1013\tSlowConsumer"
    );

    let dead_pid = alpha.pid();
    alpha.terminate();
    assert_eq!(beta.request("LOSE\talpha"), "LOST\talpha\t0");
    assert_eq!(
        beta.request("CLAIM\trecovery-session\tbeta"),
        "OWNER\tbeta\t2"
    );
    assert_eq!(
        beta.request("OPEN\trecovered-link\trecovery-session\t64"),
        "OPENED\trecovered-link\tbeta\t2"
    );
    assert_eq!(
        beta.request("ENQUEUE\trecovered-link\trecovery-session\t01020304"),
        "QUEUED\t1\t4"
    );
    assert_eq!(beta.request("DEQUEUE\trecovered-link"), "FRAME\t01020304");

    println!(
        "runtime recovery: killed_pid={dead_pid} survivor_pid={} pressure_close=1013 new_owner=beta generation=2 recovered_bytes=4",
        beta.pid()
    );
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

fn decode_hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(pair, 16).unwrap()
        })
        .collect()
}
