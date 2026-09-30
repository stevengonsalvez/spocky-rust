use spocky_crypto::{decrypt, derive_shared_key, encrypt_with_nonce, key_pair_from_secret};
use spocky_relay_pilot::{
    ClaimDecision, CloseCode, ClusterConfig, LinkConfig, LinkId, NodeConfig, NodeId,
    OpaqueCiphertext, OwnerToken, RelayError, RelayPilot, SessionId, ShedReason,
};

fn cluster() -> RelayPilot {
    let mut relay = RelayPilot::new(ClusterConfig {
        minimum_cluster_size: 2,
    });
    relay.add_node(NodeId::from("alpha"), NodeConfig { max_links: 3 });
    relay.add_node(NodeId::from("beta"), NodeConfig { max_links: 3 });
    relay
}

#[test]
fn ownership_converges_and_remote_landings_reroute() {
    let mut relay = cluster();
    let session = SessionId::from("daemon-1");

    let owner = relay
        .converge_claims(
            session.clone(),
            [NodeId::from("beta"), NodeId::from("alpha")],
        )
        .unwrap();
    assert_eq!(owner.node, NodeId::from("alpha"));
    assert_eq!(owner.generation, 1);
    assert_eq!(
        relay.route(&session, &NodeId::from("alpha")),
        ClaimDecision::Local(owner.clone())
    );
    assert_eq!(
        relay.route(&session, &NodeId::from("beta")),
        ClaimDecision::Reroute {
            target: NodeId::from("alpha"),
            owner,
        }
    );
}

#[test]
fn owner_loss_rejects_stale_epochs_and_reconnects_on_a_new_owner() {
    let mut relay = cluster();
    let session = SessionId::from("daemon-2");
    let first = relay
        .converge_claims(session.clone(), [NodeId::from("alpha")])
        .unwrap();
    relay
        .open_link(
            LinkId::from("old-link"),
            &session,
            first.clone(),
            LinkConfig {
                max_queued_bytes: 128,
            },
        )
        .unwrap();

    let closed = relay.lose_node(&NodeId::from("alpha"));
    assert_eq!(closed.len(), 1);
    assert_eq!(closed[0].link, LinkId::from("old-link"));
    assert_eq!(closed[0].code, CloseCode::SessionOwnerMoved);
    assert_eq!(closed[0].code.websocket_code(), 1012);
    assert_eq!(
        relay.enqueue(
            &LinkId::from("old-link"),
            &first,
            OpaqueCiphertext::from_bytes(vec![1; 40]),
        ),
        Err(RelayError::StaleOwner)
    );

    relay.add_node(NodeId::from("gamma"), NodeConfig { max_links: 3 });
    let second = relay
        .converge_claims(session.clone(), [NodeId::from("beta")])
        .unwrap();
    assert_eq!(second.node, NodeId::from("beta"));
    assert_eq!(second.generation, 2);
    relay
        .open_link(
            LinkId::from("new-link"),
            &session,
            second.clone(),
            LinkConfig {
                max_queued_bytes: 128,
            },
        )
        .unwrap();
    relay
        .enqueue(
            &LinkId::from("new-link"),
            &second,
            OpaqueCiphertext::from_bytes(vec![2; 40]),
        )
        .unwrap();
    assert_eq!(
        relay.dequeue(&LinkId::from("new-link")).unwrap().as_bytes(),
        &[2; 40]
    );
}

#[test]
fn ciphertext_delivery_is_ordered_opaque_and_byte_preserving() {
    let alice = key_pair_from_secret([7; 32]);
    let bob = key_pair_from_secret([9; 32]);
    let shared = derive_shared_key(&alice.secret_key, &bob.public_key).unwrap();
    let plaintexts = [
        b"TOP SECRET one".as_slice(),
        b"TOP SECRET two".as_slice(),
        b"TOP SECRET three".as_slice(),
    ];
    let ciphertexts: Vec<_> = plaintexts
        .iter()
        .enumerate()
        .map(|(index, plaintext)| {
            let mut nonce = [0_u8; 24];
            nonce[23] = u8::try_from(index).unwrap();
            OpaqueCiphertext::from_bytes(encrypt_with_nonce(&shared, &nonce, plaintext).unwrap())
        })
        .collect();

    let mut relay = cluster();
    let (link, owner) = open_link(&mut relay, "opaque", 256);
    for ciphertext in &ciphertexts {
        relay.enqueue(&link, &owner, ciphertext.clone()).unwrap();
    }

    let observations = relay.observations();
    assert_eq!(observations.len(), 3);
    assert!(observations.iter().all(|item| item.wire_bytes >= 40));
    assert!(
        observations
            .iter()
            .all(|item| !item.exposes_payload_bytes())
    );
    assert!(!format!("{observations:?}").contains("TOP SECRET"));

    for (expected_ciphertext, expected_plaintext) in ciphertexts.iter().zip(plaintexts) {
        let forwarded = relay.dequeue(&link).unwrap();
        assert_eq!(forwarded.as_bytes(), expected_ciphertext.as_bytes());
        assert_eq!(
            decrypt(&shared, forwarded.as_bytes()).unwrap(),
            expected_plaintext
        );
        assert!(
            !forwarded
                .as_bytes()
                .windows(expected_plaintext.len())
                .any(|window| window == expected_plaintext)
        );
    }
}

#[test]
fn per_link_backpressure_is_bounded_and_sheds_slow_consumers() {
    let mut relay = cluster();
    let (link, owner) = open_link(&mut relay, "slow", 64);
    let first = OpaqueCiphertext::from_bytes(vec![0xa5; 48]);
    relay.enqueue(&link, &owner, first).unwrap();
    assert_eq!(relay.queued_bytes(&link), Some(48));

    assert_eq!(
        relay.enqueue(&link, &owner, OpaqueCiphertext::from_bytes(vec![0x5a; 32]),),
        Err(RelayError::Shed(ShedReason::SlowConsumer))
    );
    assert_eq!(relay.queued_bytes(&link), None);
    assert_eq!(relay.last_close().unwrap().code, CloseCode::TryAgainLater);
    assert_eq!(relay.last_close().unwrap().code.websocket_code(), 1013);
}

#[test]
fn capacity_drain_load_shedding_and_recovery_are_deterministic() {
    let mut relay = RelayPilot::new(ClusterConfig {
        minimum_cluster_size: 2,
    });
    relay.add_node(NodeId::from("alpha"), NodeConfig { max_links: 2 });
    relay.add_node(NodeId::from("beta"), NodeConfig { max_links: 2 });
    assert!(relay.is_live(&NodeId::from("alpha")));
    assert!(relay.is_ready(&NodeId::from("alpha")));

    let session = SessionId::from("capacity");
    let owner = relay
        .converge_claims(session.clone(), [NodeId::from("alpha")])
        .unwrap();
    for link in ["old-blocked", "new-blocked"] {
        relay
            .open_link(
                LinkId::from(link),
                &session,
                owner.clone(),
                LinkConfig {
                    max_queued_bytes: 128,
                },
            )
            .unwrap();
        relay
            .enqueue(
                &LinkId::from(link),
                &owner,
                OpaqueCiphertext::from_bytes(vec![3; 40]),
            )
            .unwrap();
    }
    assert_eq!(
        relay.open_link(
            LinkId::from("over-capacity"),
            &session,
            owner.clone(),
            LinkConfig {
                max_queued_bytes: 128,
            },
        ),
        Err(RelayError::Capacity)
    );
    let shed = relay.shed_one(&NodeId::from("alpha")).unwrap();
    assert_eq!(shed.link, LinkId::from("old-blocked"));
    assert_eq!(shed.reason, ShedReason::MemoryPressure);
    assert_eq!(relay.last_close().unwrap().code.websocket_code(), 1013);

    relay.begin_drain(&NodeId::from("alpha")).unwrap();
    assert!(relay.is_live(&NodeId::from("alpha")));
    assert!(!relay.is_ready(&NodeId::from("alpha")));
    assert_eq!(
        relay.converge_claims(SessionId::from("new-during-drain"), [NodeId::from("alpha")],),
        Err(RelayError::Draining)
    );
    assert!(relay.dequeue(&LinkId::from("new-blocked")).is_some());

    relay.cancel_drain(&NodeId::from("alpha")).unwrap();
    assert!(relay.is_ready(&NodeId::from("alpha")));
    relay
        .open_link(
            LinkId::from("recovered"),
            &session,
            owner,
            LinkConfig {
                max_queued_bytes: 128,
            },
        )
        .unwrap();
}

fn open_link(
    relay: &mut RelayPilot,
    suffix: &str,
    max_queued_bytes: usize,
) -> (LinkId, OwnerToken) {
    let session = SessionId::from(format!("session-{suffix}"));
    let owner = relay
        .converge_claims(session.clone(), [NodeId::from("alpha")])
        .unwrap();
    let link = LinkId::from(format!("link-{suffix}"));
    relay
        .open_link(
            link.clone(),
            &session,
            owner.clone(),
            LinkConfig { max_queued_bytes },
        )
        .unwrap();
    (link, owner)
}
