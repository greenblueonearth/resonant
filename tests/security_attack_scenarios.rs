use resonant::consensus::{ConsensusEngine, FinalityCertificate};
use resonant::ledger::{Block, Ledger, Transaction};
use resonant::wallet::Wallet;

fn signed_pending_ledger() -> Ledger {
    let mut ledger = Ledger::new();
    let owner = Wallet::new("attack-matrix-transaction-owner");
    ledger.import_wallet(&owner).unwrap();
    ledger.transfer_signed(&owner, "attack-recipient", 10).unwrap();
    ledger
}

macro_rules! pending_state_tampering_cases {
    ($($name:ident => $mutation:expr,)*) => {
        $(
            #[test]
            fn $name() {
                let mut ledger = signed_pending_ledger();
                let mutate: fn(&mut Transaction) = $mutation;
                mutate(&mut ledger.pending[0]);
                assert!(ledger.validate().is_err());
            }
        )*
    };
}

pending_state_tampering_cases! {
    attack_pending_rejects_zero_amount => |tx| tx.amount = 0,
    attack_pending_rejects_maximum_amount => |tx| tx.amount = u64::MAX,
    attack_pending_rejects_empty_sender => |tx| tx.sender.clear(),
    attack_pending_rejects_whitespace_sender => |tx| tx.sender = " \t".to_string(),
    attack_pending_rejects_empty_recipient => |tx| tx.recipient.clear(),
    attack_pending_rejects_whitespace_recipient => |tx| tx.recipient = "\n ".to_string(),
    attack_pending_rejects_self_recipient => |tx| tx.recipient = tx.sender.clone(),
    attack_pending_rejects_zero_nonce => |tx| tx.nonce = 0,
    attack_pending_rejects_future_nonce => |tx| tx.nonce = u64::MAX,
    attack_pending_rejects_forged_identifier => |tx| tx.id = "tx-999".to_string(),
    attack_pending_rejects_zero_fee => |tx| tx.fee = 0,
    attack_pending_rejects_maximum_fee => |tx| tx.fee = u64::MAX,
    attack_pending_rejects_negative_timestamp => |tx| tx.timestamp = i64::MIN,
    attack_pending_rejects_future_timestamp_mutation => |tx| tx.timestamp = i64::MAX,
    attack_pending_rejects_missing_commitment => |tx| tx.stealth_commitment.clear(),
    attack_pending_rejects_malformed_commitment => |tx| tx.stealth_commitment = "00".to_string(),
    attack_pending_rejects_missing_nullifier => |tx| tx.nullifier.clear(),
    attack_pending_rejects_malformed_nullifier => |tx| tx.nullifier = "not-a-nullifier".to_string(),
    attack_pending_rejects_missing_signature => |tx| tx.signature.clear(),
    attack_pending_rejects_removed_signing_key => |tx| tx.public_key.clear(),
}

fn valid_remote_block() -> (Ledger, Block) {
    let owner = Wallet::new("attack-matrix-block-owner");
    let mut proposer = Ledger::new();
    proposer.import_wallet(&owner).unwrap();
    proposer
        .transfer_signed(&owner, "attack-block-recipient", 10)
        .unwrap();
    let block = proposer.mine_block().unwrap();

    let mut receiver = Ledger::new();
    receiver.import_wallet(&owner).unwrap();
    (receiver, block)
}

macro_rules! remote_block_tampering_cases {
    ($($name:ident => $mutation:expr,)*) => {
        $(
            #[test]
            fn $name() {
                let (mut receiver, mut block) = valid_remote_block();
                let before = serde_json::to_value(&receiver).unwrap();
                let mutate: fn(&mut Block) = $mutation;
                mutate(&mut block);

                assert!(receiver.apply_consensus_block(block).is_err());
                assert_eq!(serde_json::to_value(&receiver).unwrap(), before);
            }
        )*
    };
}

remote_block_tampering_cases! {
    attack_block_rejects_zero_height => |block| block.index = 0,
    attack_block_rejects_maximum_height => |block| block.index = u64::MAX,
    attack_block_rejects_negative_timestamp => |block| block.timestamp = -1,
    attack_block_rejects_maximum_timestamp => |block| block.timestamp = i64::MAX,
    attack_block_rejects_missing_parent_hash => |block| block.prev_hash.clear(),
    attack_block_rejects_forged_parent_hash => |block| block.prev_hash = "attacker-parent".to_string(),
    attack_block_rejects_missing_merkle_root => |block| block.merkle_root.clear(),
    attack_block_rejects_forged_merkle_root => |block| block.merkle_root = "00".repeat(32),
    attack_block_rejects_missing_block_hash => |block| block.hash.clear(),
    attack_block_rejects_noncanonical_block_hash => |block| block.hash = "not-hex".to_string(),
    attack_block_rejects_missing_proposer => |block| block.validator.clear(),
    attack_block_rejects_forged_proposer => |block| block.validator = "attacker".to_string(),
    attack_block_rejects_missing_stake_snapshot => |block| block.stake_snapshot.clear(),
    attack_block_rejects_zero_value_transaction => |block| block.transactions[0].amount = 0,
    attack_block_rejects_forged_transaction_signature => |block| block.transactions[0].signature = "00".repeat(64),
}

fn single_validator() -> (ConsensusEngine, Wallet) {
    let validator = Wallet::new("attack-matrix-validator");
    let mut consensus = ConsensusEngine::new(3);
    consensus.register_validator(&validator, 100).unwrap();
    (consensus, validator)
}

fn valid_certificate() -> (ConsensusEngine, FinalityCertificate) {
    let (mut consensus, validator) = single_validator();
    let vote = consensus
        .make_vote(&validator, 1, "attack-matrix-block")
        .unwrap();
    let certificate = consensus.submit_vote(vote).unwrap().unwrap();
    (consensus, certificate)
}

#[test]
fn attack_vote_replay_after_finality_does_not_change_state() {
    let (mut consensus, validator) = single_validator();
    let vote = consensus
        .make_vote(&validator, 1, "replay-block")
        .unwrap();
    consensus.submit_vote(vote.clone()).unwrap().unwrap();
    let before = serde_json::to_value(&consensus).unwrap();

    assert!(consensus.submit_vote(vote).unwrap().is_none());
    assert_eq!(serde_json::to_value(&consensus).unwrap(), before);
}

#[test]
fn attack_equivocating_vote_is_recorded_and_validator_is_jailed() {
    let (mut consensus, validator) = single_validator();
    let first = consensus
        .make_vote(&validator, 1, "first-block")
        .unwrap();
    let conflicting = consensus
        .make_vote(&validator, 1, "conflicting-block")
        .unwrap();
    consensus.submit_vote(first).unwrap();

    assert!(consensus.submit_vote(conflicting).unwrap().is_none());
    assert_eq!(consensus.slashing_evidence().len(), 1);
    assert!(consensus.validator(&validator.address).unwrap().jailed);
    assert_eq!(consensus.finalized_height(), 1);
}

#[test]
fn attack_invalid_vote_signature_does_not_change_state() {
    let (mut consensus, validator) = single_validator();
    let mut vote = consensus.make_vote(&validator, 1, "signed-block").unwrap();
    vote.signature = "00".repeat(64);
    let before = serde_json::to_value(&consensus).unwrap();

    assert!(consensus.submit_vote(vote).is_err());
    assert_eq!(serde_json::to_value(&consensus).unwrap(), before);
}

#[test]
fn attack_future_vote_does_not_change_finality() {
    let (mut consensus, validator) = single_validator();
    let vote = consensus
        .make_vote(&validator, 2, "future-block")
        .unwrap();

    assert!(consensus.submit_vote(vote).is_err());
    assert_eq!(consensus.finalized_height(), 0);
    assert!(consensus.slashing_evidence().is_empty());
}

#[test]
fn attack_zero_height_vote_does_not_change_finality() {
    let (mut consensus, validator) = single_validator();
    let vote = consensus.make_vote(&validator, 0, "genesis").unwrap();

    assert!(consensus.submit_vote(vote).is_err());
    assert_eq!(consensus.finalized_height(), 0);
}

#[test]
fn attack_empty_block_hash_cannot_be_signed_as_a_vote() {
    let (consensus, validator) = single_validator();

    assert!(consensus.make_vote(&validator, 1, "").is_err());
}

#[test]
fn attack_unregistered_wallet_cannot_create_validator_vote() {
    let (consensus, _) = single_validator();
    let attacker = Wallet::new("attack-matrix-outsider");

    assert!(consensus.make_vote(&attacker, 1, "block").is_err());
}

#[test]
fn attack_replacement_wallet_cannot_vote_for_registered_address() {
    let (consensus, validator) = single_validator();
    let mut replacement = Wallet::new("attack-matrix-replacement");
    replacement.address = validator.address.clone();

    assert!(consensus.make_vote(&replacement, 1, "block").is_err());
}

#[test]
fn attack_replayed_certificate_cannot_advance_finality_twice() {
    let (mut consensus, certificate) = valid_certificate();

    assert!(consensus.submit_certificate(certificate.clone()).is_err());
    assert_eq!(consensus.finalized_height(), 1);
    assert_eq!(consensus.certificate(1), Some(&certificate));
}

#[test]
fn attack_certificate_with_forged_signed_stake_is_atomic() {
    let (mut consensus, mut certificate) = valid_certificate();
    certificate.signed_stake += 1;
    let before = serde_json::to_value(&consensus).unwrap();

    assert!(consensus.submit_certificate(certificate).is_err());
    assert_eq!(serde_json::to_value(&consensus).unwrap(), before);
}

#[test]
fn attack_certificate_with_altered_snapshot_is_rejected() {
    let (consensus, mut certificate) = valid_certificate();
    certificate.stake_snapshot[0].1 += 1;

    assert!(consensus.verify_certificate(&certificate).is_err());
}

#[test]
fn attack_zero_stake_unbond_request_is_rejected() {
    let (mut consensus, validator) = single_validator();

    assert!(consensus.request_unstake(&validator.address, 0).is_err());
    assert!(consensus.validator(&validator.address).unwrap().unbonding.is_empty());
}

#[test]
fn attack_unbond_request_cannot_exceed_stake() {
    let (mut consensus, validator) = single_validator();

    assert!(consensus
        .request_unstake(&validator.address, 101)
        .is_err());
    assert!(consensus.validator(&validator.address).unwrap().unbonding.is_empty());
}

#[test]
fn attack_multiple_unbond_requests_cannot_reserve_stake_twice() {
    let (mut consensus, validator) = single_validator();
    consensus.request_unstake(&validator.address, 60).unwrap();

    assert!(consensus.request_unstake(&validator.address, 41).is_err());
    let requests = &consensus.validator(&validator.address).unwrap().unbonding;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].amount, 60);
}

#[test]
fn attack_unbonded_stake_cannot_be_withdrawn_before_unlock_height() {
    let (mut consensus, validator) = single_validator();
    consensus.request_unstake(&validator.address, 25).unwrap();

    assert!(consensus.withdraw_unstaked(&validator.address).is_err());
    assert_eq!(consensus.validator(&validator.address).unwrap().stake, 100);
}
