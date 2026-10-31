use resonant::consensus::ConsensusEngine;
use resonant::ledger::{Block, Ledger};
use resonant::p2p::P2pNode;
use resonant::p2p_consensus::{ConsensusEvent, ConsensusMessage, ConsensusP2pNode};
use resonant::wallet::Wallet;
use serde_json::json;
use std::collections::BTreeSet;
use std::time::Duration;

fn p2p_node(identity: Wallet, trusted: &[&Wallet]) -> P2pNode {
    P2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        identity,
        trusted
            .iter()
            .map(|wallet| wallet.public_key.clone())
            .collect(),
    )
    .unwrap()
}

fn consensus_fixture(stakes: &[u64]) -> (Ledger, ConsensusEngine, Vec<Wallet>) {
    let validators: Vec<_> = stakes
        .iter()
        .enumerate()
        .map(|(index, _)| Wallet::new(&format!("integration-validator-{index}")))
        .collect();
    let mut ledger = Ledger::new();
    let mut consensus = ConsensusEngine::new(3);

    for (wallet, stake) in validators.iter().zip(stakes) {
        ledger.import_wallet(wallet).unwrap();
        ledger.stake(&wallet.address, *stake).unwrap();
        consensus.register_validator(wallet, *stake).unwrap();
    }

    (ledger, consensus, validators)
}

fn receive_until_height(node: &mut ConsensusP2pNode, height: u64) {
    for _ in 0..20 {
        if node.ledger().height() >= height {
            return;
        }
        node.receive(Duration::from_millis(100)).unwrap();
    }
    assert_eq!(node.ledger().height(), height);
}

fn pending_follower(
    stakes: &[u64],
) -> (
    ConsensusP2pNode,
    P2pNode,
    ConsensusEngine,
    Vec<Wallet>,
    Block,
) {
    let (ledger, consensus, validators) = consensus_fixture(stakes);
    let sender_identity = Wallet::new("integration-block-sender");
    let follower_identity = Wallet::new("integration-block-follower");
    let mut follower = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        follower_identity.clone(),
        [sender_identity.public_key.clone()],
        ledger.clone(),
        consensus.clone(),
    )
    .unwrap();
    let sender = p2p_node(sender_identity, &[&follower_identity]);
    sender.discover(&[follower.local_addr()]).unwrap();
    let mut proposer = ledger;
    let block = proposer.mine_block().unwrap();
    sender
        .send(
            follower.local_addr(),
            serde_json::to_value(ConsensusMessage::Block(block.clone())).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        follower.receive(Duration::from_secs(1)).unwrap(),
        Some(ConsensusEvent::BlockPending { height: 1, .. })
    ));

    (follower, sender, consensus, validators, block)
}

#[test]
fn p2p_delivers_nested_json_over_authenticated_transport() {
    let sender_identity = Wallet::new("json-sender");
    let receiver_identity = Wallet::new("json-receiver");
    let sender = p2p_node(sender_identity.clone(), &[&receiver_identity]);
    let receiver = p2p_node(receiver_identity, &[&sender_identity]);
    let payload = json!({"nested": {"items": [1, true, null]}, "type": "event"});

    sender.send(receiver.local_addr(), payload.clone()).unwrap();

    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .payload,
        payload
    );
}

#[test]
fn p2p_preserves_unicode_payloads() {
    let sender_identity = Wallet::new("unicode-sender");
    let receiver_identity = Wallet::new("unicode-receiver");
    let sender = p2p_node(sender_identity.clone(), &[&receiver_identity]);
    let receiver = p2p_node(receiver_identity, &[&sender_identity]);
    let payload = json!({"message": "đồng thuận ngang hàng"});

    sender.send(receiver.local_addr(), payload.clone()).unwrap();

    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .payload,
        payload
    );
}

#[test]
fn p2p_reports_authenticated_sender_key() {
    let sender_identity = Wallet::new("reported-sender");
    let receiver_identity = Wallet::new("reported-receiver");
    let sender = p2p_node(sender_identity.clone(), &[&receiver_identity]);
    let receiver = p2p_node(receiver_identity, &[&sender_identity]);

    sender.send(receiver.local_addr(), json!("hello")).unwrap();

    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .peer_public_key,
        sender_identity.public_key
    );
}

#[test]
fn direct_discovery_records_and_returns_peer() {
    let first_identity = Wallet::new("direct-first");
    let second_identity = Wallet::new("direct-second");
    let first = p2p_node(first_identity.clone(), &[&second_identity]);
    let second = p2p_node(second_identity, &[&first_identity]);

    assert_eq!(
        first.discover(&[second.local_addr()]).unwrap(),
        vec![second.local_addr()]
    );
    assert!(first.peers().unwrap().contains(&second.local_addr()));
}

#[test]
fn bootstrap_discovery_finds_transitive_peer() {
    let a_identity = Wallet::new("bootstrap-a");
    let b_identity = Wallet::new("bootstrap-b");
    let c_identity = Wallet::new("bootstrap-c");
    let a = p2p_node(a_identity.clone(), &[&b_identity, &c_identity]);
    let b = p2p_node(b_identity.clone(), &[&a_identity, &c_identity]);
    let c = p2p_node(c_identity, &[&a_identity, &b_identity]);
    b.discover(&[a.local_addr()]).unwrap();

    let discovered = c.discover(&[a.local_addr()]).unwrap();

    assert!(discovered.contains(&a.local_addr()));
    assert!(discovered.contains(&b.local_addr()));
    assert!(c.peers().unwrap().contains(&b.local_addr()));
}

#[test]
fn broadcast_delivers_to_every_discovered_peer() {
    let source_identity = Wallet::new("broadcast-source");
    let first_identity = Wallet::new("broadcast-first");
    let second_identity = Wallet::new("broadcast-second");
    let source = p2p_node(
        source_identity.clone(),
        &[&first_identity, &second_identity],
    );
    let first = p2p_node(first_identity, &[&source_identity]);
    let second = p2p_node(second_identity, &[&source_identity]);
    source
        .discover(&[first.local_addr(), second.local_addr()])
        .unwrap();
    let payload = json!({"broadcast": 1});

    source.broadcast(payload.clone()).unwrap();

    assert_eq!(
        first.recv_timeout(Duration::from_secs(1)).unwrap().payload,
        payload
    );
    assert_eq!(
        second.recv_timeout(Duration::from_secs(1)).unwrap().payload,
        payload
    );
}

#[test]
fn p2p_delivers_multiple_messages_in_order() {
    let sender_identity = Wallet::new("ordered-sender");
    let receiver_identity = Wallet::new("ordered-receiver");
    let sender = p2p_node(sender_identity.clone(), &[&receiver_identity]);
    let receiver = p2p_node(receiver_identity, &[&sender_identity]);
    sender.send(receiver.local_addr(), json!(1)).unwrap();
    sender.send(receiver.local_addr(), json!(2)).unwrap();

    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .payload,
        json!(1)
    );
    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .payload,
        json!(2)
    );
}

#[test]
fn broadcast_without_peers_succeeds() {
    let node = p2p_node(Wallet::new("empty-broadcast"), &[]);

    assert!(node.broadcast(json!({"event": "empty"})).is_ok());
}

#[test]
fn p2p_receive_timeout_is_reported() {
    let node = p2p_node(Wallet::new("timeout-node"), &[]);

    assert!(node.recv_timeout(Duration::from_millis(10)).is_err());
}

#[test]
fn untrusted_peer_cannot_publish() {
    let sender_identity = Wallet::new("intruder");
    let receiver_identity = Wallet::new("protected-receiver");
    let sender = p2p_node(sender_identity, &[]);
    let receiver = p2p_node(receiver_identity, &[]);

    assert!(sender
        .send(receiver.local_addr(), json!({"type": "attack"}))
        .is_err());
}

#[test]
fn rejected_untrusted_publish_is_not_delivered() {
    let sender = p2p_node(Wallet::new("untrusted-sender"), &[]);
    let receiver = p2p_node(Wallet::new("untrusted-target"), &[]);
    let _ = sender.send(receiver.local_addr(), json!("blocked"));

    assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
}

#[test]
fn ephemeral_bind_advertises_the_bound_port() {
    let node = P2pNode::bind_with_advertise(
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1:0".parse().unwrap(),
        Wallet::new("advertised-port"),
        BTreeSet::new(),
    )
    .unwrap();

    assert_eq!(node.advertised_addr().port(), node.local_addr().port());
}

#[test]
fn p2p_refuses_to_send_to_its_own_address() {
    let node = p2p_node(Wallet::new("self-send"), &[]);

    assert!(node
        .send(node.advertised_addr(), json!({"type": "self"}))
        .is_err());
}

#[test]
fn discovery_ignores_own_address() {
    let node = p2p_node(Wallet::new("self-discovery"), &[]);

    assert!(node.discover(&[node.advertised_addr()]).unwrap().is_empty());
}

#[test]
fn repeated_discovery_does_not_duplicate_peer() {
    let first_identity = Wallet::new("repeat-first");
    let second_identity = Wallet::new("repeat-second");
    let first = p2p_node(first_identity.clone(), &[&second_identity]);
    let second = p2p_node(second_identity, &[&first_identity]);
    first.discover(&[second.local_addr()]).unwrap();
    first.discover(&[second.local_addr()]).unwrap();

    assert_eq!(
        first
            .peers()
            .unwrap()
            .iter()
            .filter(|address| **address == second.local_addr())
            .count(),
        1
    );
}

#[test]
fn discovery_reports_unreachable_seed() {
    let node = p2p_node(Wallet::new("unreachable-seed"), &[]);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    assert!(node.discover(&[address]).is_err());
}

#[test]
fn broadcast_reports_failure_after_known_peer_disconnects() {
    let sender_identity = Wallet::new("disconnect-sender");
    let receiver_identity = Wallet::new("disconnect-receiver");
    let sender = p2p_node(sender_identity.clone(), &[&receiver_identity]);
    let receiver = p2p_node(receiver_identity, &[&sender_identity]);
    sender.discover(&[receiver.local_addr()]).unwrap();
    drop(receiver);

    assert!(sender
        .broadcast(json!({"event": "after-disconnect"}))
        .is_err());
}

#[test]
fn one_validator_can_finalize_a_block_with_quorum() {
    let (ledger, consensus, validators) = consensus_fixture(&[100]);
    let mut node = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validators[0].clone(),
        [],
        ledger,
        consensus,
    )
    .unwrap();

    assert!(matches!(
        node.propose_block().unwrap(),
        ConsensusEvent::BlockFinalized { height: 1, .. }
    ));
    assert_eq!(node.ledger().height(), 1);
    assert_eq!(node.consensus().finalized_height(), 1);
}

#[test]
fn two_validator_nodes_converge_on_finality() {
    let (ledger, consensus, validators) = consensus_fixture(&[70, 30]);
    let mut node_a = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validators[0].clone(),
        [validators[1].public_key.clone()],
        ledger.clone(),
        consensus.clone(),
    )
    .unwrap();
    let mut node_b = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validators[1].clone(),
        [validators[0].public_key.clone()],
        ledger,
        consensus,
    )
    .unwrap();
    node_a.discover(&[node_b.local_addr()]).unwrap();

    node_a.propose_block().unwrap();
    receive_until_height(&mut node_b, 1);

    assert_eq!(node_a.ledger().height(), node_b.ledger().height());
    assert_eq!(
        node_a.ledger().block_hash_at(1),
        node_b.ledger().block_hash_at(1)
    );
}

#[test]
fn finalized_block_hash_matches_between_peers() {
    let (ledger, consensus, validators) = consensus_fixture(&[70, 30]);
    let mut proposer = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validators[0].clone(),
        [validators[1].public_key.clone()],
        ledger.clone(),
        consensus.clone(),
    )
    .unwrap();
    let mut follower = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validators[1].clone(),
        [validators[0].public_key.clone()],
        ledger,
        consensus,
    )
    .unwrap();
    proposer.discover(&[follower.local_addr()]).unwrap();
    proposer.propose_block().unwrap();
    receive_until_height(&mut follower, 1);

    assert_eq!(
        proposer.ledger().block_hash_at(1),
        follower.ledger().block_hash_at(1)
    );
}

#[test]
fn consensus_node_starts_with_matching_genesis_state() {
    let (ledger, consensus, validators) = consensus_fixture(&[100]);
    let node = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validators[0].clone(),
        [],
        ledger,
        consensus,
    )
    .unwrap();

    assert_eq!(node.ledger().height(), 0);
    assert_eq!(node.consensus().finalized_height(), 0);
    assert!(node.ledger().block_hash_at(0).is_some());
}

#[test]
fn pending_transfer_is_included_and_balances_converge() {
    let (mut ledger, consensus, validators) = consensus_fixture(&[70, 30]);
    let sender = Wallet::new("signed-transfer-sender");
    ledger.import_wallet(&sender).unwrap();
    ledger.transfer_signed(&sender, "bob", 123).unwrap();
    let mut proposer = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validators[0].clone(),
        [validators[1].public_key.clone()],
        ledger.clone(),
        consensus.clone(),
    )
    .unwrap();
    let mut follower = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validators[1].clone(),
        [validators[0].public_key.clone()],
        ledger,
        consensus,
    )
    .unwrap();
    proposer.discover(&[follower.local_addr()]).unwrap();
    proposer.propose_block().unwrap();
    receive_until_height(&mut follower, 1);

    assert_eq!(
        proposer.ledger().balance_of(&sender.address),
        follower.ledger().balance_of(&sender.address)
    );
    assert_eq!(
        proposer.ledger().balance_of("bob"),
        follower.ledger().balance_of("bob")
    );
}

#[test]
fn sequential_blocks_advance_both_finality_heights() {
    let (ledger, consensus, validators) = consensus_fixture(&[70, 30]);
    let mut proposer = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validators[0].clone(),
        [validators[1].public_key.clone()],
        ledger.clone(),
        consensus.clone(),
    )
    .unwrap();
    let mut follower = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validators[1].clone(),
        [validators[0].public_key.clone()],
        ledger,
        consensus,
    )
    .unwrap();
    proposer.discover(&[follower.local_addr()]).unwrap();

    proposer.propose_block().unwrap();
    receive_until_height(&mut follower, 1);
    proposer.propose_block().unwrap();
    receive_until_height(&mut follower, 2);

    assert_eq!(proposer.consensus().finalized_height(), 2);
    assert_eq!(follower.consensus().finalized_height(), 2);
    assert_eq!(
        proposer.ledger().block_hash_at(2),
        follower.ledger().block_hash_at(2)
    );
}

#[test]
fn non_validator_proposal_remains_pending() {
    let (ledger, consensus, _) = consensus_fixture(&[70, 30]);
    let mut node = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        Wallet::new("integration-non-validator"),
        [],
        ledger,
        consensus,
    )
    .unwrap();

    assert!(matches!(
        node.propose_block().unwrap(),
        ConsensusEvent::BlockPending { height: 1, .. }
    ));
    assert_eq!(node.ledger().height(), 0);
    assert_eq!(node.consensus().finalized_height(), 0);
}

#[test]
fn follower_keeps_received_block_unapplied_until_certificate() {
    let (follower, _, _, _, _) = pending_follower(&[70, 30]);

    assert_eq!(follower.ledger().height(), 0);
    assert_eq!(follower.consensus().finalized_height(), 0);
}

#[test]
fn follower_applies_valid_certificate_and_block() {
    let (mut follower, sender, mut consensus, validators, block) = pending_follower(&[70, 30]);
    let vote = consensus.make_vote(&validators[0], 1, &block.hash).unwrap();
    let certificate = consensus.submit_vote(vote).unwrap().unwrap();
    sender
        .send(
            follower.local_addr(),
            serde_json::to_value(ConsensusMessage::Certificate(certificate)).unwrap(),
        )
        .unwrap();

    assert!(matches!(
        follower.receive(Duration::from_secs(1)).unwrap(),
        Some(ConsensusEvent::BlockFinalized { height: 1, .. })
    ));
    assert_eq!(follower.ledger().height(), 1);
    assert_eq!(
        follower.ledger().block_hash_at(1),
        Some(block.hash.as_str())
    );
}

#[test]
fn follower_rejects_tampered_certificate_without_applying_block() {
    let (mut follower, sender, mut consensus, validators, block) = pending_follower(&[70, 30]);
    let vote = consensus.make_vote(&validators[0], 1, &block.hash).unwrap();
    let mut certificate = consensus.submit_vote(vote).unwrap().unwrap();
    certificate.signed_stake -= 1;
    sender
        .send(
            follower.local_addr(),
            serde_json::to_value(ConsensusMessage::Certificate(certificate)).unwrap(),
        )
        .unwrap();

    assert!(follower.receive(Duration::from_secs(1)).is_err());
    assert_eq!(follower.ledger().height(), 0);
    assert_eq!(follower.consensus().finalized_height(), 0);
}

#[test]
fn follower_rejects_invalid_block_received_from_trusted_peer() {
    let (ledger, consensus, _) = consensus_fixture(&[70, 30]);
    let sender_identity = Wallet::new("invalid-block-sender");
    let follower_identity = Wallet::new("invalid-block-follower");
    let mut follower = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        follower_identity.clone(),
        [sender_identity.public_key.clone()],
        ledger.clone(),
        consensus,
    )
    .unwrap();
    let sender = p2p_node(sender_identity, &[&follower_identity]);
    sender.discover(&[follower.local_addr()]).unwrap();
    let mut proposer = ledger;
    let mut block = proposer.mine_block().unwrap();
    block.hash.push('x');
    sender
        .send(
            follower.local_addr(),
            serde_json::to_value(ConsensusMessage::Block(block)).unwrap(),
        )
        .unwrap();

    assert!(follower.receive(Duration::from_secs(1)).is_err());
    assert_eq!(follower.ledger().height(), 0);
    assert_eq!(follower.consensus().finalized_height(), 0);
}

#[test]
fn follower_rejects_unknown_consensus_message_type() {
    let sender_identity = Wallet::new("malformed-message-sender");
    let follower_identity = Wallet::new("malformed-message-follower");
    let (ledger, consensus, _) = consensus_fixture(&[100]);
    let mut follower = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        follower_identity.clone(),
        [sender_identity.public_key.clone()],
        ledger,
        consensus,
    )
    .unwrap();
    let sender = p2p_node(sender_identity, &[&follower_identity]);
    sender.discover(&[follower.local_addr()]).unwrap();
    sender
        .send(
            follower.local_addr(),
            json!({"type": "UnknownConsensusMessage", "data": {}}),
        )
        .unwrap();

    assert!(follower.receive(Duration::from_secs(1)).is_err());
    assert_eq!(follower.ledger().height(), 0);
    assert_eq!(follower.consensus().finalized_height(), 0);
}

#[test]
fn consensus_ledger_and_registry_stake_are_synchronized_after_finality() {
    let (ledger, consensus, validators) = consensus_fixture(&[70, 30]);
    let mut node = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validators[0].clone(),
        [],
        ledger,
        consensus,
    )
    .unwrap();
    node.propose_block().unwrap();

    for validator in validators {
        assert_eq!(
            node.ledger().stake_of(&validator.address),
            node.consensus()
                .validator(&validator.address)
                .unwrap()
                .stake
        );
    }
}
