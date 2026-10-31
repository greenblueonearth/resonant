use resonant::consensus::ConsensusEngine;
use resonant::ledger::{Block, Ledger};
use resonant::p2p::P2pNode;
use resonant::p2p_consensus::{ConsensusEvent, ConsensusMessage, ConsensusP2pNode};
use resonant::wallet::Wallet;
use serde_json::json;
use std::collections::BTreeSet;
use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;

fn node(identity: Wallet, trusted: &[&Wallet]) -> P2pNode {
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

fn one_validator_fixture(label: &str) -> (Ledger, ConsensusEngine, Wallet) {
    let validator = Wallet::new(label);
    let mut ledger = Ledger::new();
    ledger.import_wallet(&validator).unwrap();
    ledger.stake(&validator.address, 100).unwrap();
    let mut consensus = ConsensusEngine::new(3);
    consensus.register_validator(&validator, 100).unwrap();
    (ledger, consensus, validator)
}

fn empty_block(ledger: &Ledger) -> Block {
    ledger.clone().mine_block().unwrap()
}

#[test]
fn signed_minimum_transfer_finalizes_and_burns_fee() {
    let mut ledger = Ledger::new();
    let owner = Wallet::new("minimum-amount-owner");
    ledger.import_wallet(&owner).unwrap();
    let recipient_before = ledger.balance_of("bob");
    ledger.transfer_signed(&owner, "bob", 1).unwrap();

    let block = ledger.mine_block().unwrap();

    assert_eq!(block.transactions[0].amount, 1);
    assert_eq!(ledger.balance_of("bob"), recipient_before + 1);
    assert_eq!(ledger.balance_of(&owner.address), 9_998);
}

#[test]
fn signed_transfer_to_unicode_account_roundtrips() {
    let mut ledger = Ledger::new();
    let owner = Wallet::new("unicode-recipient-owner");
    ledger.import_wallet(&owner).unwrap();
    let recipient = "tài_khoản_日本語";
    ledger.transfer_signed(&owner, recipient, 7).unwrap();

    let restored: Ledger = serde_json::from_slice(&serde_json::to_vec(&ledger).unwrap()).unwrap();

    assert!(restored.validate().is_ok());
    assert_eq!(restored.pending[0].recipient, recipient);
}

#[test]
fn whitespace_only_signed_transfer_recipient_is_rejected_atomically() {
    let mut ledger = Ledger::new();
    let owner = Wallet::new("whitespace-recipient-owner");
    ledger.import_wallet(&owner).unwrap();
    let before = serde_json::to_value(&ledger).unwrap();

    assert!(ledger.transfer_signed(&owner, " \t\n", 1).is_err());
    assert_eq!(serde_json::to_value(&ledger).unwrap(), before);
}

#[test]
fn signed_self_transfer_is_rejected_atomically() {
    let mut ledger = Ledger::new();
    let owner = Wallet::new("self-transfer-owner");
    ledger.import_wallet(&owner).unwrap();
    let before = serde_json::to_value(&ledger).unwrap();

    assert!(ledger.transfer_signed(&owner, &owner.address, 1).is_err());
    assert_eq!(serde_json::to_value(&ledger).unwrap(), before);
}

#[test]
fn signed_transfer_of_zero_is_rejected_without_nonce_consumption() {
    let mut ledger = Ledger::new();
    let owner = Wallet::new("zero-transfer-owner");
    ledger.import_wallet(&owner).unwrap();
    let before = serde_json::to_value(&ledger).unwrap();

    assert!(ledger.transfer_signed(&owner, "bob", 0).is_err());
    assert_eq!(serde_json::to_value(&ledger).unwrap(), before);
}

#[test]
fn pending_incoming_funds_cannot_be_spent_before_finality() {
    let mut ledger = Ledger::new();
    let sender = Wallet::new("incoming-sender");
    let recipient = Wallet::new("incoming-recipient");
    ledger.import_wallet(&sender).unwrap();
    ledger.import_wallet(&recipient).unwrap();
    ledger
        .transfer_signed(&sender, &recipient.address, 100)
        .unwrap();

    assert!(ledger.transfer_signed(&recipient, "bob", 10_001).is_err());
    assert_eq!(ledger.balance_of(&recipient.address), 10_000);
    assert_eq!(ledger.pending.len(), 1);
}

#[test]
fn incoming_funds_can_be_spent_after_block_application() {
    let mut source = Ledger::new();
    let sender = Wallet::new("settled-sender");
    let recipient = Wallet::new("settled-recipient");
    source.import_wallet(&sender).unwrap();
    source.import_wallet(&recipient).unwrap();
    source
        .transfer_signed(&sender, &recipient.address, 100)
        .unwrap();
    let first = source.mine_block().unwrap();
    let mut receiver = Ledger::new();
    receiver.import_wallet(&sender).unwrap();
    receiver.import_wallet(&recipient).unwrap();
    receiver.apply_consensus_block(first).unwrap();

    let spent = receiver.transfer_signed(&recipient, "bob", 50).unwrap();

    assert_eq!(spent.nonce, 2);
    assert!(receiver.validate().is_ok());
}

#[test]
fn multiple_transactions_from_one_owner_finalize_in_nonce_order() {
    let mut source = Ledger::new();
    let owner = Wallet::new("multi-tx-owner");
    source.import_wallet(&owner).unwrap();
    source.transfer_signed(&owner, "bob", 10).unwrap();
    source.transfer_signed(&owner, "charlie", 20).unwrap();
    let block = source.mine_block().unwrap();
    let mut receiver = Ledger::new();
    receiver.import_wallet(&owner).unwrap();

    receiver.apply_consensus_block(block).unwrap();

    assert_eq!(receiver.height(), 1);
    assert_eq!(receiver.balance_of("bob"), 1_000_000_010);
    assert_eq!(receiver.balance_of("charlie"), 1_000_000_020);
    assert_eq!(receiver.balance_of(&owner.address), 9_968);
}

#[test]
fn independent_wallet_signers_share_global_nonce_order() {
    let mut source = Ledger::new();
    let first = Wallet::new("first-global-nonce");
    let second = Wallet::new("second-global-nonce");
    source.import_wallet(&first).unwrap();
    source.import_wallet(&second).unwrap();
    let first_tx = source.transfer_signed(&first, "bob", 2).unwrap();
    let second_tx = source.transfer_signed(&second, "charlie", 3).unwrap();
    let block = source.mine_block().unwrap();
    let mut receiver = Ledger::new();
    receiver.import_wallet(&first).unwrap();
    receiver.import_wallet(&second).unwrap();

    receiver.apply_consensus_block(block).unwrap();

    assert_eq!((first_tx.nonce, second_tx.nonce), (1, 2));
    assert_eq!(receiver.balance_of("bob"), 1_000_000_002);
    assert_eq!(receiver.balance_of("charlie"), 1_000_000_003);
}

#[test]
fn unrelated_empty_block_preserves_pending_signed_transfer() {
    let mut ledger = Ledger::new();
    let owner = Wallet::new("pending-drop-owner");
    ledger.import_wallet(&owner).unwrap();
    ledger.transfer_signed(&owner, "bob", 9).unwrap();
    let empty = empty_block(&Ledger::new());

    ledger.apply_consensus_block(empty).unwrap();

    assert_eq!(ledger.pending.len(), 1);
    assert!(ledger.validate().is_ok());
    assert_eq!(ledger.balance_of(&owner.address), 10_000);
}

#[test]
fn peer_can_send_json_null_payload() {
    let sender_identity = Wallet::new("null-sender");
    let receiver_identity = Wallet::new("null-receiver");
    let sender = node(sender_identity.clone(), &[&receiver_identity]);
    let receiver = node(receiver_identity, &[&sender_identity]);

    sender.send(receiver.local_addr(), json!(null)).unwrap();

    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .payload,
        json!(null)
    );
}

#[test]
fn peer_can_send_empty_json_array_payload() {
    let sender_identity = Wallet::new("array-sender");
    let receiver_identity = Wallet::new("array-receiver");
    let sender = node(sender_identity.clone(), &[&receiver_identity]);
    let receiver = node(receiver_identity, &[&sender_identity]);

    sender.send(receiver.local_addr(), json!([])).unwrap();

    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .payload,
        json!([])
    );
}

#[test]
fn near_frame_limit_payload_is_delivered() {
    let sender_identity = Wallet::new("large-payload-sender");
    let receiver_identity = Wallet::new("large-payload-receiver");
    let sender = node(sender_identity.clone(), &[&receiver_identity]);
    let receiver = node(receiver_identity, &[&sender_identity]);
    let payload = json!({"blob": "a".repeat(60_000)});

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
fn over_frame_limit_payload_is_rejected_without_delivery() {
    let sender_identity = Wallet::new("oversize-payload-sender");
    let receiver_identity = Wallet::new("oversize-payload-receiver");
    let sender = node(sender_identity.clone(), &[&receiver_identity]);
    let receiver = node(receiver_identity, &[&sender_identity]);

    assert!(sender
        .send(receiver.local_addr(), json!({"blob": "a".repeat(70_000)}))
        .is_err());
    assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
}

#[test]
fn empty_raw_frame_does_not_break_following_authenticated_connection() {
    let receiver_identity = Wallet::new("empty-frame-receiver");
    let sender_identity = Wallet::new("empty-frame-sender");
    let receiver = node(receiver_identity.clone(), &[&sender_identity]);
    let mut raw = TcpStream::connect(receiver.local_addr()).unwrap();
    raw.write_all(&0_u32.to_be_bytes()).unwrap();
    drop(raw);
    let sender = node(sender_identity, &[&receiver_identity]);

    sender
        .send(receiver.local_addr(), json!({"after": "empty frame"}))
        .unwrap();

    assert_eq!(
        receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .payload,
        json!({"after": "empty frame"})
    );
}

#[test]
fn repeated_same_peer_sends_are_received_once_per_request() {
    let sender_identity = Wallet::new("repeat-send-sender");
    let receiver_identity = Wallet::new("repeat-send-receiver");
    let sender = node(sender_identity.clone(), &[&receiver_identity]);
    let receiver = node(receiver_identity, &[&sender_identity]);

    for sequence in 0..4 {
        sender
            .send(receiver.local_addr(), json!({"sequence": sequence}))
            .unwrap();
    }

    for sequence in 0..4 {
        assert_eq!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .payload,
            json!({"sequence": sequence})
        );
    }
}

#[test]
fn validator_can_restart_from_finalized_state_and_advance_height() {
    let (ledger, consensus, validator) = one_validator_fixture("restart-edge-validator");
    let mut first = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validator.clone(),
        [],
        ledger,
        consensus,
    )
    .unwrap();
    first.propose_block().unwrap();
    let restored_ledger: Ledger =
        serde_json::from_slice(&serde_json::to_vec(first.ledger()).unwrap()).unwrap();
    let restored_consensus: ConsensusEngine =
        serde_json::from_slice(&serde_json::to_vec(first.consensus()).unwrap()).unwrap();
    drop(first);
    let mut restarted = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validator,
        [],
        restored_ledger,
        restored_consensus,
    )
    .unwrap();

    assert!(matches!(
        restarted.propose_block().unwrap(),
        ConsensusEvent::BlockFinalized { height: 2, .. }
    ));
}

#[test]
fn valid_duplicate_finalized_block_is_idempotent_over_network() {
    let (ledger, consensus, validator) = one_validator_fixture("duplicate-finalized-validator");
    let sender_identity = Wallet::new("duplicate-block-sender");
    let follower_identity = Wallet::new("duplicate-block-follower");
    let mut proposer = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validator.clone(),
        [
            follower_identity.public_key.clone(),
            sender_identity.public_key.clone(),
        ],
        ledger.clone(),
        consensus.clone(),
    )
    .unwrap();
    let mut follower = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        follower_identity.clone(),
        [
            validator.public_key.clone(),
            sender_identity.public_key.clone(),
        ],
        ledger,
        consensus,
    )
    .unwrap();
    proposer.discover(&[follower.local_addr()]).unwrap();
    let sender = node(sender_identity.clone(), &[&follower_identity]);
    let event = proposer.propose_block().unwrap();
    let ConsensusEvent::BlockFinalized { height: 1, hash } = event else {
        panic!("single-validator block should finalize");
    };
    let block: Block = serde_json::from_value(
        serde_json::to_value(proposer.ledger()).unwrap()["chain"][1].clone(),
    )
    .unwrap();
    for _ in 0..10 {
        if follower.ledger().height() == 1 {
            break;
        }
        follower.receive(Duration::from_millis(100)).unwrap();
    }
    assert_eq!(follower.ledger().block_hash_at(1), Some(hash.as_str()));
    let before = serde_json::to_value(follower.ledger()).unwrap();
    sender
        .send(
            follower.local_addr(),
            serde_json::to_value(ConsensusMessage::Block(block)).unwrap(),
        )
        .unwrap();

    assert!(matches!(
        follower.receive(Duration::from_secs(1)).unwrap(),
        Some(ConsensusEvent::BlockFinalized { height: 1, hash: received_hash })
            if received_hash == hash
    ));
    assert_eq!(serde_json::to_value(follower.ledger()).unwrap(), before);
}

#[test]
fn isolated_consensus_node_times_out_without_mutating_finality() {
    let (ledger, consensus, validator) = one_validator_fixture("idle-consensus-validator");
    let mut node = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validator,
        [],
        ledger,
        consensus,
    )
    .unwrap();

    assert!(node.receive(Duration::from_millis(10)).unwrap().is_none());
    assert_eq!(node.ledger().height(), 0);
    assert_eq!(node.consensus().finalized_height(), 0);
}

#[test]
fn discovery_with_no_seeds_does_not_change_peer_set() {
    let node = P2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        Wallet::new("empty-seed-node"),
        BTreeSet::new(),
    )
    .unwrap();

    assert!(node.discover(&[]).unwrap().is_empty());
    assert!(node.peers().unwrap().is_empty());
}
