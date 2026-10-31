use resonant::consensus::ConsensusEngine;
use resonant::ledger::{validate_transaction_authorization, Ledger, Transaction};
use resonant::p2p::P2pNode;
use resonant::p2p_consensus::{ConsensusEvent, ConsensusMessage, ConsensusP2pNode};
use resonant::wallet::Wallet;
use serde_json::json;
use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;

fn signed_transaction() -> (Wallet, Transaction) {
    let mut ledger = Ledger::new();
    let wallet = Wallet::new("operational-sender");
    ledger.import_wallet(&wallet).unwrap();
    let transaction = ledger.transfer_signed(&wallet, "bob", 25).unwrap();
    (wallet, transaction)
}

fn transport_node(identity: Wallet, trusted: &[&Wallet]) -> P2pNode {
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

fn secured_consensus_fixture() -> (Ledger, ConsensusEngine, Wallet) {
    let validator = Wallet::new("operational-validator");
    let mut ledger = Ledger::new();
    ledger.import_wallet(&validator).unwrap();
    ledger.stake(&validator.address, 100).unwrap();
    let mut consensus = ConsensusEngine::new(3);
    consensus.register_validator(&validator, 100).unwrap();
    (ledger, consensus, validator)
}

fn assert_rejected_after_tampering(change: impl FnOnce(&mut Transaction)) {
    let (_, mut transaction) = signed_transaction();
    change(&mut transaction);
    assert!(validate_transaction_authorization(&transaction).is_err());
}

#[test]
fn authorized_signed_transaction_passes_validation() {
    let (_, transaction) = signed_transaction();

    assert!(validate_transaction_authorization(&transaction).is_ok());
}

#[test]
fn local_demo_transaction_is_not_authorized_for_consensus() {
    let mut ledger = Ledger::new();
    let transaction = ledger.transfer("alice", "bob", 5).unwrap();

    assert!(validate_transaction_authorization(&transaction).is_err());
}

#[test]
fn authorization_rejects_modified_signature() {
    assert_rejected_after_tampering(|tx| tx.signature.replace_range(0..2, "00"));
}

#[test]
fn authorization_rejects_modified_amount() {
    assert_rejected_after_tampering(|tx| tx.amount += 1);
}

#[test]
fn authorization_rejects_modified_recipient() {
    assert_rejected_after_tampering(|tx| tx.recipient = "mallory".to_string());
}

#[test]
fn authorization_rejects_modified_fee() {
    assert_rejected_after_tampering(|tx| tx.fee += 1);
}

#[test]
fn authorization_rejects_modified_nonce() {
    assert_rejected_after_tampering(|tx| tx.nonce += 1);
}

#[test]
fn authorization_rejects_modified_timestamp() {
    assert_rejected_after_tampering(|tx| tx.timestamp += 1);
}

#[test]
fn authorization_rejects_modified_stealth_commitment() {
    assert_rejected_after_tampering(|tx| tx.stealth_commitment.push('0'));
}

#[test]
fn authorization_rejects_modified_nullifier() {
    assert_rejected_after_tampering(|tx| tx.nullifier.push('0'));
}

#[test]
fn authorization_rejects_modified_transaction_id() {
    assert_rejected_after_tampering(|tx| tx.id.push('x'));
}

#[test]
fn authorization_rejects_modified_sender() {
    assert_rejected_after_tampering(|tx| tx.sender = "alice".to_string());
}

#[test]
fn authorization_rejects_public_key_from_another_wallet() {
    assert_rejected_after_tampering(|tx| tx.public_key = Wallet::new("other-key").public_key);
}

#[test]
fn authorization_rejects_malformed_public_key() {
    assert_rejected_after_tampering(|tx| tx.public_key = "not-hex".to_string());
}

#[test]
fn authorization_rejects_empty_signature() {
    assert_rejected_after_tampering(|tx| tx.signature.clear());
}

#[test]
fn signed_transfer_rejects_wallet_address_mismatch_atomically() {
    let mut ledger = Ledger::new();
    let wallet = Wallet::new("mismatched-address");
    ledger.import_wallet(&wallet).unwrap();
    let mut altered = wallet.clone();
    altered.address.push('x');
    let original_nonce = serde_json::to_value(&ledger).unwrap()["nonce"].clone();

    assert!(ledger.transfer_signed(&altered, "bob", 10).is_err());
    assert!(ledger.pending.is_empty());
    assert_eq!(
        serde_json::to_value(&ledger).unwrap()["nonce"],
        original_nonce
    );
}

#[test]
fn signed_transfer_rejects_invalid_secret_without_state_change() {
    let mut ledger = Ledger::new();
    let wallet = Wallet::new("invalid-secret");
    ledger.import_wallet(&wallet).unwrap();
    let mut altered = wallet.clone();
    altered.secret = "invalid".to_string();
    let before = serde_json::to_value(&ledger).unwrap();

    assert!(ledger.transfer_signed(&altered, "bob", 10).is_err());
    assert_eq!(serde_json::to_value(&ledger).unwrap(), before);
}

#[test]
fn consensus_block_validation_rejects_unsigned_demo_transaction() {
    let mut ledger = Ledger::new();
    ledger.transfer("alice", "bob", 5).unwrap();
    let block = ledger.mine_block().unwrap();
    let fresh = Ledger::new();

    assert!(fresh.validate_block_for_consensus(&block).is_err());
}

#[test]
fn applying_unauthorized_consensus_block_is_atomic() {
    let mut proposer = Ledger::new();
    proposer.transfer("alice", "bob", 5).unwrap();
    let block = proposer.mine_block().unwrap();
    let mut receiver = Ledger::new();
    let before = serde_json::to_value(&receiver).unwrap();

    assert!(receiver.apply_consensus_block(block).is_err());
    assert_eq!(serde_json::to_value(&receiver).unwrap(), before);
}

#[test]
fn signed_transaction_is_preserved_in_ledger_serialization() {
    let mut ledger = Ledger::new();
    let wallet = Wallet::new("serialized-signer");
    ledger.import_wallet(&wallet).unwrap();
    ledger.transfer_signed(&wallet, "bob", 4).unwrap();
    let restored: Ledger = serde_json::from_slice(&serde_json::to_vec(&ledger).unwrap()).unwrap();

    assert!(validate_transaction_authorization(&restored.pending[0]).is_ok());
    assert!(restored.validate().is_ok());
}

#[test]
fn legacy_transaction_serialization_remains_backward_compatible() {
    let mut ledger = Ledger::new();
    let transaction = ledger.transfer("alice", "bob", 4).unwrap();
    let value = serde_json::to_value(&transaction).unwrap();
    let restored: Transaction = serde_json::from_value(value.clone()).unwrap();

    assert!(value.get("public_key").is_none());
    assert!(restored.public_key.is_empty());
    assert!(validate_transaction_authorization(&restored).is_err());
}

#[test]
fn replayed_signed_transaction_fails_ledger_nonce_validation() {
    let mut ledger = Ledger::new();
    let wallet = Wallet::new("replay-signer");
    ledger.import_wallet(&wallet).unwrap();
    let transaction = ledger.transfer_signed(&wallet, "bob", 4).unwrap();
    let mut candidate = ledger.clone();
    candidate.mine_block().unwrap();
    candidate.pending.push(transaction);

    assert!(candidate.validate().is_err());
}

#[test]
fn trusted_peer_cannot_finalize_unsigned_funds_transfer() {
    let (ledger, consensus, validator) = secured_consensus_fixture();
    let sender_identity = Wallet::new("untrusted-transfer-proposer");
    let follower_identity = validator.clone();
    let mut follower = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        follower_identity.clone(),
        [sender_identity.public_key.clone()],
        ledger.clone(),
        consensus,
    )
    .unwrap();
    let sender = transport_node(sender_identity, &[&follower_identity]);
    sender.discover(&[follower.local_addr()]).unwrap();
    let mut proposer = ledger;
    proposer.transfer("alice", "mallory", 900_000_000).unwrap();
    let forged_block = proposer.mine_block().unwrap();
    sender
        .send(
            follower.local_addr(),
            serde_json::to_value(ConsensusMessage::Block(forged_block)).unwrap(),
        )
        .unwrap();

    assert!(follower.receive(Duration::from_secs(1)).is_err());
    assert_eq!(follower.ledger().height(), 0);
    assert_eq!(follower.ledger().balance_of("alice"), 1_000_000_000);
    assert_eq!(follower.consensus().finalized_height(), 0);
    assert!(follower
        .consensus()
        .vote_for(1, &validator.address)
        .is_none());
}

#[test]
fn signed_transfer_can_be_finalized_by_its_validator_owner() {
    let (mut ledger, consensus, validator) = secured_consensus_fixture();
    ledger.transfer_signed(&validator, "bob", 125).unwrap();
    let before = ledger.balance_of("bob");
    let mut node = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validator,
        [],
        ledger,
        consensus,
    )
    .unwrap();

    assert!(matches!(
        node.propose_block().unwrap(),
        ConsensusEvent::BlockFinalized { height: 1, .. }
    ));
    assert_eq!(node.ledger().balance_of("bob"), before + 125);
}

#[test]
fn slow_handshake_does_not_block_other_peers() {
    let identity = Wallet::new("handshake-resilience");
    let peer_identity = Wallet::new("valid-after-malformed");
    let node = transport_node(identity.clone(), &[&peer_identity]);
    let slow_connection = TcpStream::connect(node.local_addr()).unwrap();
    let peer = transport_node(peer_identity.clone(), &[&identity]);

    peer.send(node.local_addr(), json!({"valid": true}))
        .unwrap();

    assert_eq!(
        node.recv_timeout(Duration::from_secs(1)).unwrap().payload,
        json!({"valid": true})
    );
    drop(slow_connection);
}

#[test]
fn incomplete_frame_does_not_block_other_peers() {
    let identity = Wallet::new("frame-resilience");
    let peer_identity = Wallet::new("valid-after-oversized");
    let node = transport_node(identity.clone(), &[&peer_identity]);
    let mut oversized = TcpStream::connect(node.local_addr()).unwrap();
    oversized.write_all(&20_u32.to_be_bytes()).unwrap();
    let peer = transport_node(peer_identity.clone(), &[&identity]);

    peer.send(node.local_addr(), json!({"valid": true}))
        .unwrap();

    assert_eq!(
        node.recv_timeout(Duration::from_secs(1)).unwrap().payload,
        json!({"valid": true})
    );
    drop(oversized);
}

#[test]
fn malformed_consensus_payload_is_rejected_without_state_change() {
    let (ledger, consensus, _) = secured_consensus_fixture();
    let sender_identity = Wallet::new("malformed-consensus-sender");
    let follower_identity = Wallet::new("malformed-consensus-follower");
    let mut follower = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        follower_identity.clone(),
        [sender_identity.public_key.clone()],
        ledger,
        consensus,
    )
    .unwrap();
    let sender = transport_node(sender_identity, &[&follower_identity]);
    sender.discover(&[follower.local_addr()]).unwrap();
    sender
        .send(
            follower.local_addr(),
            json!({"type": "Block", "data": null}),
        )
        .unwrap();

    assert!(follower.receive(Duration::from_secs(1)).is_err());
    assert_eq!(follower.ledger().height(), 0);
    assert_eq!(follower.consensus().finalized_height(), 0);
}

#[test]
fn valid_empty_block_finality_updates_both_ledgers() {
    let (ledger, consensus, validator) = secured_consensus_fixture();
    let follower_identity = Wallet::new("empty-block-follower");
    let mut proposer = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validator.clone(),
        [follower_identity.public_key.clone()],
        ledger.clone(),
        consensus.clone(),
    )
    .unwrap();
    let mut follower = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        follower_identity.clone(),
        [validator.public_key.clone()],
        ledger,
        consensus,
    )
    .unwrap();
    proposer.discover(&[follower.local_addr()]).unwrap();

    proposer.propose_block().unwrap();
    for _ in 0..15 {
        if follower.ledger().height() == 1 {
            break;
        }
        follower.receive(Duration::from_millis(100)).unwrap();
    }

    assert_eq!(follower.ledger().height(), 1);
    assert_eq!(follower.consensus().finalized_height(), 1);
}

#[test]
fn consensus_rejects_untrusted_p2p_sender_before_queueing_message() {
    let untrusted = transport_node(Wallet::new("not-trusted"), &[]);
    let receiver = transport_node(Wallet::new("strict-receiver"), &[]);

    assert!(untrusted
        .send(receiver.local_addr(), json!({"type": "Vote"}))
        .is_err());
    assert!(receiver.recv_timeout(Duration::from_millis(50)).is_err());
}

#[test]
fn validator_stake_remains_unchanged_after_rejected_forgery() {
    let (ledger, consensus, validator) = secured_consensus_fixture();
    let before_stake = ledger.stake_of(&validator.address);
    let sender_identity = Wallet::new("stake-forgery-sender");
    let follower_identity = Wallet::new("stake-forgery-follower");
    let mut follower = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        follower_identity.clone(),
        [sender_identity.public_key.clone()],
        ledger,
        consensus,
    )
    .unwrap();
    let sender = transport_node(sender_identity, &[&follower_identity]);
    sender.discover(&[follower.local_addr()]).unwrap();
    let mut forged_ledger = Ledger::new();
    forged_ledger.transfer("alice", "mallory", 10).unwrap();
    let block = forged_ledger.mine_block().unwrap();
    sender
        .send(
            follower.local_addr(),
            serde_json::to_value(ConsensusMessage::Block(block)).unwrap(),
        )
        .unwrap();
    assert!(follower.receive(Duration::from_secs(1)).is_err());

    assert_eq!(follower.ledger().stake_of(&validator.address), before_stake);
    assert_eq!(follower.ledger().height(), 0);
}
