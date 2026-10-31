use resonant::consensus::ConsensusEngine;
use resonant::ledger::{validate_transaction_authorization, Block, Ledger};
use resonant::p2p::P2pNode;
use resonant::p2p_consensus::{ConsensusEvent, ConsensusMessage, ConsensusP2pNode};
use resonant::wallet::Wallet;
use serde_json::json;
use std::collections::BTreeSet;
use std::io::Write;
use std::net::TcpStream;
use std::thread;
use std::time::{Duration, Instant};

fn network_node(identity: Wallet, trusted: &[&Wallet]) -> P2pNode {
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

fn validator_state(label: &str, stake: u64) -> (Ledger, ConsensusEngine, Wallet) {
    let wallet = Wallet::new(label);
    let mut ledger = Ledger::new();
    ledger.import_wallet(&wallet).unwrap();
    ledger.stake(&wallet.address, stake).unwrap();
    let mut consensus = ConsensusEngine::new(3);
    consensus.register_validator(&wallet, stake).unwrap();
    (ledger, consensus, wallet)
}

fn apply_signed_transfer(ledger: &mut Ledger, owner: &Wallet, amount: u64) -> Block {
    ledger
        .transfer_signed(owner, "bob", amount)
        .expect("owner-authorized transfer should be accepted");
    ledger
        .mine_block()
        .expect("signed transfer should be mineable")
}

fn send_block(sender: &P2pNode, recipient: &ConsensusP2pNode, block: &Block) {
    sender
        .send(
            recipient.local_addr(),
            serde_json::to_value(ConsensusMessage::Block(block.clone())).unwrap(),
        )
        .unwrap();
}

#[test]
fn restored_signed_ledger_can_continue_with_next_nonce() {
    let owner = Wallet::new("recovery-owner");
    let mut ledger = Ledger::new();
    ledger.import_wallet(&owner).unwrap();
    ledger.transfer_signed(&owner, "bob", 10).unwrap();
    let restored: Ledger = serde_json::from_slice(&serde_json::to_vec(&ledger).unwrap()).unwrap();

    let next = restored.clone().transfer_signed(&owner, "bob", 11).unwrap();

    assert_eq!(next.nonce, 2);
    assert!(restored.validate().is_ok());
}

#[test]
fn repeated_signed_transfers_reserve_balance_and_nonce() {
    let owner = Wallet::new("repeat-owner");
    let mut ledger = Ledger::new();
    ledger.import_wallet(&owner).unwrap();

    let first = ledger.transfer_signed(&owner, "bob", 100).unwrap();
    let second = ledger.transfer_signed(&owner, "bob", 200).unwrap();

    assert_eq!((first.nonce, second.nonce), (1, 2));
    assert_eq!(ledger.pending.len(), 2);
    assert!(ledger.validate().is_ok());
}

#[test]
fn failed_insufficient_signed_transfer_preserves_nonce_and_pending() {
    let owner = Wallet::new("insufficient-owner");
    let mut ledger = Ledger::new();
    ledger.import_wallet(&owner).unwrap();
    ledger.stake(&owner.address, 9_999).unwrap();
    let before = serde_json::to_value(&ledger).unwrap();

    assert!(ledger.transfer_signed(&owner, "bob", 1).is_err());
    assert_eq!(serde_json::to_value(&ledger).unwrap(), before);
}

#[test]
fn signed_transfer_fee_is_included_in_balance_reservation() {
    let owner = Wallet::new("fee-owner");
    let mut ledger = Ledger::new();
    ledger.import_wallet(&owner).unwrap();
    ledger.stake(&owner.address, 9_998).unwrap();

    assert!(ledger.transfer_signed(&owner, "bob", 1).is_ok());
    assert_eq!(ledger.available_balance(&owner.address), 2);
    assert!(ledger.transfer_signed(&owner, "bob", 1).is_err());
}

#[test]
fn signed_transaction_can_be_applied_to_an_independent_replica() {
    let owner = Wallet::new("replicated-owner");
    let mut source = Ledger::new();
    source.import_wallet(&owner).unwrap();
    let replica = source.clone();
    let initial_balance = source.balance_of("bob");
    let block = apply_signed_transfer(&mut source, &owner, 55);
    let mut receiver = replica;

    receiver.apply_consensus_block(block).unwrap();

    assert_eq!(receiver.height(), 1);
    assert_eq!(receiver.balance_of("bob"), initial_balance + 55);
    assert!(receiver.validate().is_ok());
}

#[test]
fn replica_missing_signer_account_rejects_signed_block() {
    let owner = Wallet::new("missing-replica-owner");
    let mut source = Ledger::new();
    source.import_wallet(&owner).unwrap();
    let block = apply_signed_transfer(&mut source, &owner, 5);
    let mut receiver = Ledger::new();

    assert!(receiver.apply_consensus_block(block).is_err());
    assert_eq!(receiver.height(), 0);
}

#[test]
fn modified_previous_hash_is_rejected_atomically() {
    let mut source = Ledger::new();
    let mut block = source.mine_block().unwrap();
    block.prev_hash.push('x');
    let mut receiver = Ledger::new();
    let before = serde_json::to_value(&receiver).unwrap();

    assert!(receiver.apply_consensus_block(block).is_err());
    assert_eq!(serde_json::to_value(&receiver).unwrap(), before);
}

#[test]
fn modified_stake_snapshot_is_rejected() {
    let owner = Wallet::new("snapshot-owner");
    let mut source = Ledger::new();
    source.import_wallet(&owner).unwrap();
    let mut receiver = source.clone();
    let mut block = source.mine_block().unwrap();
    block.stake_snapshot[0].1 += 1;

    assert!(receiver.apply_consensus_block(block).is_err());
    assert_eq!(receiver.height(), 0);
}

#[test]
fn conflicting_local_pending_transaction_prevents_block_application() {
    let owner = Wallet::new("conflicting-pending-owner");
    let mut base = Ledger::new();
    base.import_wallet(&owner).unwrap();
    let mut source = base.clone();
    let remote_tx = source.transfer_signed(&owner, "bob", 70).unwrap();
    let remote_block = source.mine_block().unwrap();
    let mut receiver = base;
    let local_tx = receiver.transfer_signed(&owner, "bob", 71).unwrap();
    assert_eq!(local_tx.id, remote_tx.id);
    let before = serde_json::to_value(&receiver).unwrap();

    assert!(receiver.apply_consensus_block(remote_block).is_err());
    assert_eq!(serde_json::to_value(&receiver).unwrap(), before);
}

#[test]
fn recipient_overflow_does_not_mutate_sender_or_pending_state() {
    let mut ledger = Ledger::new();
    let owner = Wallet::new("overflow-owner");
    ledger.import_wallet(&owner).unwrap();
    let recipient = "overflow-recipient";
    let before = serde_json::to_value(&ledger).unwrap();
    let mut value = serde_json::to_value(&ledger).unwrap();
    value["balances"][recipient] = json!(u64::MAX);
    ledger = serde_json::from_value(value).unwrap();
    let before_attempt = serde_json::to_value(&ledger).unwrap();

    assert!(ledger.transfer_signed(&owner, recipient, 1).is_err());
    assert_eq!(serde_json::to_value(&ledger).unwrap(), before_attempt);
    assert!(before.get("nonce").is_some());
}

#[test]
fn serialized_validator_state_preserves_finalized_certificate() {
    let (_, mut consensus, validator) = validator_state("persisted-finality", 100);
    let block_hash = "persistent-certificate-block";
    let vote = consensus.make_vote(&validator, 1, block_hash).unwrap();
    let certificate = consensus.submit_vote(vote).unwrap().unwrap();
    let restored: ConsensusEngine =
        serde_json::from_slice(&serde_json::to_vec(&consensus).unwrap()).unwrap();

    assert_eq!(restored.finalized_height(), 1);
    assert_eq!(restored.certificate(1), Some(&certificate));
}

#[test]
fn persisted_consensus_can_validate_next_height_vote() {
    let (_, mut consensus, validator) = validator_state("resume-vote", 100);
    let first_vote = consensus.make_vote(&validator, 1, "block-one").unwrap();
    consensus.submit_vote(first_vote).unwrap().unwrap();
    let restored: ConsensusEngine =
        serde_json::from_slice(&serde_json::to_vec(&consensus).unwrap()).unwrap();
    let next_vote = restored.make_vote(&validator, 2, "block-two");

    assert_eq!(next_vote.unwrap().height, 2);
}

#[test]
fn pending_signed_transaction_survives_restart_and_can_be_mined() {
    let owner = Wallet::new("pending-restart-owner");
    let mut ledger = Ledger::new();
    ledger.import_wallet(&owner).unwrap();
    ledger.transfer_signed(&owner, "bob", 40).unwrap();
    let mut restored: Ledger =
        serde_json::from_slice(&serde_json::to_vec(&ledger).unwrap()).unwrap();
    let initial_bob = restored.balance_of("bob");

    let block = restored.mine_block().unwrap();

    assert_eq!(block.transactions.len(), 1);
    assert_eq!(restored.balance_of("bob"), initial_bob + 40);
    assert!(restored.validate().is_ok());
}

#[test]
fn ledger_corruption_in_balance_is_detected_on_load_validation() {
    let ledger = Ledger::new();
    let mut value = serde_json::to_value(ledger).unwrap();
    value["balances"]["alice"] = json!("broken");

    assert!(serde_json::from_value::<Ledger>(value).is_err());
}

#[test]
fn zero_stake_update_removes_account_from_snapshot() {
    let mut ledger = Ledger::new();
    ledger.stake("alice", 100).unwrap();

    ledger.set_stake_amount("alice", 0).unwrap();
    let block = ledger.mine_block().unwrap();

    assert!(!block
        .stake_snapshot
        .iter()
        .any(|(address, _)| address == "alice"));
    assert!(ledger.validate().is_ok());
}

#[test]
fn no_stake_after_removing_bootstrap_stake_prevents_mining() {
    let mut ledger = Ledger::new();
    ledger.set_stake_amount("henry", 0).unwrap();

    assert!(ledger.mine_block().is_err());
    assert_eq!(ledger.height(), 0);
}

#[test]
fn consensus_node_rejects_state_with_ledger_height_ahead() {
    let (mut ledger, consensus, validator) = validator_state("height-mismatch", 100);
    ledger.mine_block().unwrap();

    assert!(ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validator,
        [],
        ledger,
        consensus
    )
    .is_err());
}

#[test]
fn consensus_node_rejects_state_with_consensus_height_ahead() {
    let (ledger, mut consensus, validator) = validator_state("consensus-ahead", 100);
    let vote = consensus.make_vote(&validator, 1, "block-one").unwrap();
    consensus.submit_vote(vote).unwrap();

    assert!(ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validator,
        [],
        ledger,
        consensus
    )
    .is_err());
}

#[test]
fn finalized_node_can_restart_and_finalize_next_empty_block() {
    let (ledger, consensus, validator) = validator_state("node-restart", 100);
    let mut original = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validator.clone(),
        [],
        ledger,
        consensus,
    )
    .unwrap();
    original.propose_block().unwrap();
    let restored_ledger: Ledger =
        serde_json::from_slice(&serde_json::to_vec(original.ledger()).unwrap()).unwrap();
    let restored_consensus: ConsensusEngine =
        serde_json::from_slice(&serde_json::to_vec(original.consensus()).unwrap()).unwrap();
    drop(original);
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
fn concurrent_authenticated_senders_are_all_served() {
    let sender_wallets: Vec<_> = (0..8)
        .map(|index| Wallet::new(&format!("concurrent-sender-{index}")))
        .collect();
    let receiver_identity = Wallet::new("concurrent-receiver");
    let receiver = network_node(
        receiver_identity.clone(),
        &sender_wallets.iter().collect::<Vec<_>>(),
    );
    let receiver_address = receiver.local_addr();
    let senders: Vec<_> = sender_wallets
        .iter()
        .cloned()
        .map(|identity| {
            let receiver_identity = receiver_identity.clone();
            thread::spawn(move || {
                let sender = network_node(identity, &[&receiver_identity]);
                sender
                    .send(receiver_address, json!({"worker": receiver_identity.label}))
                    .unwrap();
            })
        })
        .collect();
    for sender in senders {
        sender.join().unwrap();
    }

    let messages: Vec<_> = (0..8)
        .map(|_| receiver.recv_timeout(Duration::from_secs(2)).unwrap())
        .collect();
    assert_eq!(messages.len(), 8);
    assert_eq!(
        messages
            .iter()
            .map(|message| message.peer_public_key.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        8
    );
}

#[test]
fn listener_recovers_after_invalid_noise_prefix() {
    let identity = Wallet::new("noise-recovery");
    let sender_identity = Wallet::new("noise-recovery-sender");
    let node = network_node(identity.clone(), &[&sender_identity]);
    let mut invalid = TcpStream::connect(node.local_addr()).unwrap();
    invalid.write_all(&[0, 0, 0, 1, 0xff]).unwrap();
    drop(invalid);
    let sender = network_node(sender_identity, &[&identity]);

    sender
        .send(node.local_addr(), json!({"after": "invalid"}))
        .unwrap();

    assert_eq!(
        node.recv_timeout(Duration::from_secs(1)).unwrap().payload,
        json!({"after": "invalid"})
    );
}

#[test]
fn dropped_node_releases_its_listening_port() {
    let identity = Wallet::new("released-port");
    let node = network_node(identity.clone(), &[]);
    let address = node.local_addr();
    drop(node);
    let replacement = P2pNode::bind(address, identity, BTreeSet::new());

    assert!(replacement.is_ok());
}

#[test]
fn invalid_certificate_does_not_replace_persisted_finality() {
    let (ledger, mut consensus, validator) = validator_state("invalid-certificate", 100);
    let vote = consensus.make_vote(&validator, 1, "valid-block").unwrap();
    let mut certificate = consensus.submit_vote(vote).unwrap().unwrap();
    certificate.block_hash.push('x');
    let before = serde_json::to_value(&consensus).unwrap();

    assert!(consensus.submit_certificate(certificate).is_err());
    assert_eq!(serde_json::to_value(&consensus).unwrap(), before);
    assert_eq!(ledger.height(), 0);
}

#[test]
fn broadcast_with_one_unreachable_peer_still_attempts_other_peers() {
    let sender_identity = Wallet::new("partial-broadcast-sender");
    let first_identity = Wallet::new("partial-broadcast-first");
    let second_identity = Wallet::new("partial-broadcast-second");
    let sender = network_node(
        sender_identity.clone(),
        &[&first_identity, &second_identity],
    );
    let first = network_node(first_identity, &[&sender_identity]);
    let second = network_node(second_identity, &[&sender_identity]);
    sender
        .discover(&[first.local_addr(), second.local_addr()])
        .unwrap();
    drop(first);

    assert!(sender.broadcast(json!({"event": "partial"})).is_err());
    assert_eq!(
        second.recv_timeout(Duration::from_secs(1)).unwrap().payload,
        json!({"event": "partial"})
    );
}

#[test]
fn malformed_persisted_consensus_is_not_silently_accepted() {
    let value = json!({"validators": "not-a-map"});

    assert!(serde_json::from_value::<ConsensusEngine>(value).is_err());
}

#[test]
fn failed_wallet_signature_does_not_change_existing_pending_transactions() {
    let owner = Wallet::new("pending-preservation-owner");
    let mut ledger = Ledger::new();
    ledger.import_wallet(&owner).unwrap();
    ledger.transfer_signed(&owner, "bob", 2).unwrap();
    let mut invalid = owner.clone();
    invalid.secret.clear();
    let before = serde_json::to_value(&ledger).unwrap();

    assert!(ledger.transfer_signed(&invalid, "bob", 3).is_err());
    assert_eq!(serde_json::to_value(&ledger).unwrap(), before);
}

#[test]
fn signed_transaction_rejects_amount_fee_integer_overflow() {
    let owner = Wallet::new("amount-overflow-owner");
    let mut ledger = Ledger::new();
    ledger.import_wallet(&owner).unwrap();

    assert!(ledger.transfer_signed(&owner, "bob", u64::MAX).is_err());
    assert!(ledger.pending.is_empty());
}

#[test]
fn network_validator_rejects_untrusted_signed_transfer_block() {
    let (ledger, consensus, validator) = validator_state("strict-network-validator", 100);
    let attacker = Wallet::new("trusted-attacker");
    let validator_identity = validator.clone();
    let mut node = ConsensusP2pNode::bind(
        "127.0.0.1:0".parse().unwrap(),
        validator.clone(),
        [attacker.public_key.clone()],
        ledger.clone(),
        consensus,
    )
    .unwrap();
    let peer = network_node(attacker.clone(), &[&validator_identity]);
    peer.discover(&[node.local_addr()]).unwrap();
    let mut forged = ledger;
    forged.transfer("alice", "attacker", 1_000_000).unwrap();
    let forged_block = forged.mine_block().unwrap();

    send_block(&peer, &node, &forged_block);

    assert!(node.receive(Duration::from_secs(1)).is_err());
    assert_eq!(node.ledger().height(), 0);
    assert!(node.consensus().vote_for(1, &validator.address).is_none());
}

#[test]
fn node_drop_with_idle_listener_finishes_promptly() {
    let node = network_node(Wallet::new("prompt-drop"), &[]);
    let started = Instant::now();

    drop(node);

    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn transaction_signature_is_not_valid_for_another_sender_address() {
    let mut ledger = Ledger::new();
    let owner = Wallet::new("sender-binding-owner");
    ledger.import_wallet(&owner).unwrap();
    let mut transaction = ledger.transfer_signed(&owner, "bob", 3).unwrap();
    transaction.sender = "alice".to_string();

    assert!(validate_transaction_authorization(&transaction).is_err());
}
