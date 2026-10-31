use resonant::consensus::{ConsensusEngine, FinalityCertificate, SignedVote};
use resonant::ledger::{validate_transaction_authorization, Block, Ledger, Transaction};
use resonant::p2p::P2pNode;
use resonant::wallet::Wallet;
use std::collections::BTreeSet;

fn signed_tx() -> Transaction {
    let mut ledger = Ledger::new();
    let owner = Wallet::new("matrix-owner");
    ledger.import_wallet(&owner).unwrap();
    ledger.transfer_signed(&owner, "bob", 10).unwrap()
}

fn flip_hex_character(value: &mut String, index: usize) {
    let replacement = if value.as_bytes()[index] == b'0' {
        "1"
    } else {
        "0"
    };
    value.replace_range(index..index + 1, replacement);
}

macro_rules! transaction_authorization_cases {
    ($($name:ident => $mutation:expr,)*) => {
        $(
            #[test]
            fn $name() {
                let mut transaction = signed_tx();
                let mutate: fn(&mut Transaction) = $mutation;
                mutate(&mut transaction);
                assert!(validate_transaction_authorization(&transaction).is_err());
            }
        )*
    };
}

transaction_authorization_cases! {
    matrix_tx_rejects_sender_substitution => |tx| tx.sender = "alice".to_string(),
    matrix_tx_rejects_recipient_substitution => |tx| tx.recipient = "mallory".to_string(),
    matrix_tx_rejects_amount_increment => |tx| tx.amount += 1,
    matrix_tx_rejects_fee_change => |tx| tx.fee += 1,
    matrix_tx_rejects_nonce_increment => |tx| tx.nonce += 1,
    matrix_tx_rejects_timestamp_change => |tx| tx.timestamp -= 1,
    matrix_tx_rejects_transaction_id_change => |tx| tx.id.push('x'),
    matrix_tx_rejects_stealth_commitment_change => |tx| tx.stealth_commitment.push('x'),
    matrix_tx_rejects_nullifier_change => |tx| tx.nullifier.push('x'),
    matrix_tx_rejects_empty_public_key => |tx| tx.public_key.clear(),
    matrix_tx_rejects_another_valid_public_key => |tx| tx.public_key = Wallet::new("other-1").public_key,
    matrix_tx_rejects_uppercase_public_key => |tx| tx.public_key.make_ascii_uppercase(),
    matrix_tx_rejects_short_public_key => |tx| tx.public_key.truncate(20),
    matrix_tx_rejects_non_hex_public_key => |tx| tx.public_key.replace_range(0..2, "zz"),
    matrix_tx_rejects_first_signature_byte_change => |tx| flip_hex_character(&mut tx.signature, 0),
    matrix_tx_rejects_middle_signature_byte_change => |tx| flip_hex_character(&mut tx.signature, 30),
    matrix_tx_rejects_last_signature_byte_change => |tx| flip_hex_character(&mut tx.signature, 62),
    matrix_tx_rejects_truncated_signature => |tx| tx.signature.truncate(80),
    matrix_tx_rejects_extended_signature => |tx| tx.signature.push_str("00"),
    matrix_tx_rejects_non_hex_signature => |tx| tx.signature.replace_range(0..2, "gg"),
}

fn valid_certificate() -> (ConsensusEngine, FinalityCertificate) {
    let validator = Wallet::new("matrix-certificate-validator");
    let other = Wallet::new("matrix-certificate-other");
    let mut engine = ConsensusEngine::new(3);
    engine.register_validator(&validator, 100).unwrap();
    engine.register_validator(&other, 1).unwrap();
    let vote = engine.make_vote(&validator, 1, "matrix-block").unwrap();
    let certificate = engine.submit_vote(vote).unwrap().unwrap();
    (engine, certificate)
}

macro_rules! certificate_rejection_cases {
    ($($name:ident => $mutation:expr,)*) => {
        $(
            #[test]
            fn $name() {
                let (engine, mut certificate) = valid_certificate();
                let mutate: fn(&mut FinalityCertificate) = $mutation;
                mutate(&mut certificate);
                assert!(engine.verify_certificate(&certificate).is_err());
            }
        )*
    };
}

certificate_rejection_cases! {
    matrix_certificate_rejects_zero_height => |cert| cert.height = 0,
    matrix_certificate_rejects_empty_hash => |cert| cert.block_hash.clear(),
    matrix_certificate_rejects_changed_hash => |cert| cert.block_hash.push('x'),
    matrix_certificate_rejects_changed_snapshot_address => |cert| cert.stake_snapshot[0].0.push('x'),
    matrix_certificate_rejects_changed_snapshot_weight => |cert| cert.stake_snapshot[0].1 += 1,
    matrix_certificate_rejects_empty_snapshot => |cert| cert.stake_snapshot.clear(),
    matrix_certificate_rejects_zero_snapshot_weight => |cert| cert.stake_snapshot[0].1 = 0,
    matrix_certificate_rejects_duplicate_snapshot_validator => |cert| cert.stake_snapshot.push(cert.stake_snapshot[0].clone()),
    matrix_certificate_rejects_total_stake_mismatch => |cert| cert.total_stake += 1,
    matrix_certificate_rejects_signed_stake_mismatch => |cert| cert.signed_stake += 1,
    matrix_certificate_rejects_no_votes => |cert| cert.votes.clear(),
    matrix_certificate_rejects_wrong_vote_height => |cert| cert.votes[0].height += 1,
    matrix_certificate_rejects_wrong_vote_hash => |cert| cert.votes[0].block_hash.push('x'),
    matrix_certificate_rejects_unknown_voter => |cert| cert.votes[0].validator = "unknown".to_string(),
    matrix_certificate_rejects_invalid_vote_signature => |cert| cert.votes[0].signature = "00".repeat(64),
    matrix_certificate_rejects_duplicate_vote => |cert| { let duplicate = cert.votes[0].clone(); cert.votes.push(duplicate); },
    matrix_certificate_rejects_vote_from_another_key => |cert| cert.votes[0].signature = Wallet::new("certificate-forger").sign(b"forged").unwrap(),
    matrix_certificate_rejects_vote_removal_from_quorum => |cert| { cert.votes.remove(0); },
    matrix_certificate_rejects_snapshot_order_violation => |cert| cert.stake_snapshot.reverse(),
    matrix_certificate_rejects_signed_stake_overflow => |cert| cert.signed_stake = u64::MAX,
}

fn vote_fixture() -> (ConsensusEngine, Wallet, SignedVote) {
    let validator = Wallet::new("matrix-vote-validator");
    let mut engine = ConsensusEngine::new(3);
    engine.register_validator(&validator, 100).unwrap();
    let vote = engine
        .make_vote(&validator, 1, "matrix-vote-block")
        .unwrap();
    (engine, validator, vote)
}

macro_rules! vote_rejection_cases {
    ($($name:ident => $mutation:expr,)*) => {
        $(
            #[test]
            fn $name() {
                let (mut engine, _, mut vote) = vote_fixture();
                let mutate: fn(&mut SignedVote) = $mutation;
                mutate(&mut vote);
                assert!(engine.submit_vote(vote).is_err());
                assert_eq!(engine.finalized_height(), 0);
            }
        )*
    };
}

vote_rejection_cases! {
    matrix_vote_rejects_height_zero => |vote| vote.height = 0,
    matrix_vote_rejects_future_height => |vote| vote.height = 2,
    matrix_vote_rejects_empty_hash => |vote| vote.block_hash.clear(),
    matrix_vote_rejects_changed_hash => |vote| vote.block_hash.push('x'),
    matrix_vote_rejects_unknown_validator => |vote| vote.validator = "unknown".to_string(),
    matrix_vote_rejects_changed_signature_first_byte => |vote| flip_hex_character(&mut vote.signature, 0),
    matrix_vote_rejects_changed_signature_middle_byte => |vote| flip_hex_character(&mut vote.signature, 64),
    matrix_vote_rejects_changed_signature_last_byte => |vote| flip_hex_character(&mut vote.signature, 126),
    matrix_vote_rejects_truncated_signature => |vote| vote.signature.truncate(64),
    matrix_vote_rejects_non_hex_signature => |vote| vote.signature.replace_range(0..2, "zz"),
    matrix_vote_rejects_empty_signature => |vote| vote.signature.clear(),
    matrix_vote_rejects_signature_from_another_wallet => |vote| vote.signature = Wallet::new("vote-forger-1").sign(b"forged").unwrap(),
    matrix_vote_rejects_signature_replay_at_another_height => |vote| vote.height += 1,
    matrix_vote_rejects_signature_replay_for_another_hash => |vote| vote.block_hash = "other-block".to_string(),
    matrix_vote_rejects_signature_replay_for_another_address => |vote| vote.validator.push('x'),
    matrix_vote_rejects_extra_signature_bytes => |vote| vote.signature.push_str("00"),
    matrix_vote_rejects_all_zero_signature => |vote| vote.signature = "00".repeat(64),
    matrix_vote_rejects_malformed_validator_address => |vote| vote.validator.clear(),
    matrix_vote_rejects_wrong_signer_identity => |vote| vote.validator = Wallet::new("wrong-voter").address,
    matrix_vote_rejects_multiple_vote_payload_mutations => |vote| { vote.height = 9; vote.block_hash.push('x'); },
}

fn empty_candidate_block(ledger: &Ledger) -> Block {
    let mut candidate = ledger.clone();
    let owner = Wallet::new("matrix-block-signer");
    candidate.import_wallet(&owner).unwrap();
    candidate.stake(&owner.address, 1).unwrap();
    candidate.transfer_signed(&owner, "bob", 1).unwrap();
    candidate.mine_block().unwrap()
}

macro_rules! block_rejection_cases {
    ($($name:ident => $mutation:expr,)*) => {
        $(
            #[test]
            fn $name() {
                let ledger = Ledger::new();
                let mut block = empty_candidate_block(&ledger);
                let mutate: fn(&mut Block) = $mutation;
                mutate(&mut block);
                let mut receiver = ledger.clone();
                let before = serde_json::to_value(&receiver).unwrap();
                assert!(receiver.apply_consensus_block(block).is_err());
                assert_eq!(serde_json::to_value(&receiver).unwrap(), before);
            }
        )*
    };
}

block_rejection_cases! {
    matrix_block_rejects_zero_index => |block| block.index = 0,
    matrix_block_rejects_skipped_index => |block| block.index += 1,
    matrix_block_rejects_negative_timestamp => |block| block.timestamp = -1,
    matrix_block_rejects_changed_previous_hash => |block| block.prev_hash.push('x'),
    matrix_block_rejects_changed_merkle_root => |block| block.merkle_root.push('x'),
    matrix_block_rejects_changed_block_hash => |block| block.hash.push('x'),
    matrix_block_rejects_changed_validator => |block| block.validator.push('x'),
    matrix_block_rejects_empty_stake_snapshot => |block| block.stake_snapshot.clear(),
    matrix_block_rejects_zero_stake_snapshot_entry => |block| block.stake_snapshot[0].1 = 0,
    matrix_block_rejects_repeated_snapshot_entry => |block| { let duplicate = block.stake_snapshot[0].clone(); block.stake_snapshot.push(duplicate); },
    matrix_block_rejects_reordered_snapshot => |block| block.stake_snapshot.reverse(),
    matrix_block_rejects_extra_transaction => |block| block.transactions.push(block.transactions[0].clone()),
    matrix_block_rejects_transaction_amount_change => |block| block.transactions[0].amount += 1,
    matrix_block_rejects_transaction_recipient_change => |block| block.transactions[0].recipient.push('x'),
    matrix_block_rejects_transaction_signature_change => |block| block.transactions[0].signature.push('x'),
    matrix_block_rejects_transaction_key_change => |block| block.transactions[0].public_key.push('x'),
    matrix_block_rejects_transaction_nonce_change => |block| block.transactions[0].nonce += 1,
    matrix_block_rejects_transaction_fee_change => |block| block.transactions[0].fee += 1,
    matrix_block_rejects_transaction_nullifier_change => |block| block.transactions[0].nullifier.push('x'),
    matrix_block_rejects_transaction_commitment_change => |block| block.transactions[0].stealth_commitment.push('x'),
}

fn malformed_peer_key_cases() -> Vec<(String, String)> {
    let valid = Wallet::new("valid-key-shape").public_key;
    let upper = valid.to_ascii_uppercase();
    let letter = valid
        .bytes()
        .position(|byte| (b'a'..=b'f').contains(&byte))
        .unwrap();
    let mut mixed_case = valid.clone();
    mixed_case.replace_range(
        letter..letter + 1,
        &valid[letter..letter + 1].to_ascii_uppercase(),
    );
    vec![
        ("empty".into(), String::new()),
        ("nonhex-word".into(), "not-a-key".into()),
        ("short-hex".into(), "ab".into()),
        ("odd-hex".into(), "a".repeat(63)),
        ("31-byte".into(), "ab".repeat(31)),
        ("33-byte".into(), "ab".repeat(33)),
        ("uppercase".into(), upper),
        ("all-zero".into(), "00".repeat(32)),
        ("bad-hex-length".into(), "ff".repeat(31)),
        ("prefix".into(), format!("0x{valid}")),
        ("whitespace".into(), format!(" {valid}")),
        ("newline".into(), format!("{valid}\n")),
        (
            "embedded-space".into(),
            format!("{} {}", &valid[..32], &valid[32..]),
        ),
        ("unicode".into(), format!("{}é", &valid[..62])),
        ("trailing-hex".into(), format!("{valid}00")),
        ("leading-hex".into(), format!("00{valid}")),
        ("punctuation".into(), format!("{}!", &valid[..63])),
        ("null-byte".into(), format!("{}\0", &valid[..63])),
        ("mixed-case".into(), mixed_case),
        ("invalid-hex".into(), format!("{}zz", &valid[..60])),
    ]
}

macro_rules! peer_key_rejection_cases {
    ($($name:ident => $index:expr,)*) => {
        $(
            #[test]
            fn $name() {
                let key = malformed_peer_key_cases()[$index].1.clone();
                let result = P2pNode::bind(
                    "127.0.0.1:0".parse().unwrap(),
                    Wallet::new(stringify!($name)),
                    BTreeSet::from([key]),
                );
                assert!(result.is_err());
            }
        )*
    };
}

peer_key_rejection_cases! {
    matrix_peer_key_rejects_empty => 0,
    matrix_peer_key_rejects_nonhex_word => 1,
    matrix_peer_key_rejects_short_hex => 2,
    matrix_peer_key_rejects_odd_hex => 3,
    matrix_peer_key_rejects_31_bytes => 4,
    matrix_peer_key_rejects_33_bytes => 5,
    matrix_peer_key_rejects_uppercase_hex => 6,
    matrix_peer_key_rejects_zero_key => 7,
    matrix_peer_key_rejects_noncanonical_curve_bytes => 8,
    matrix_peer_key_rejects_prefixed_key => 9,
    matrix_peer_key_rejects_leading_whitespace => 10,
    matrix_peer_key_rejects_trailing_newline => 11,
    matrix_peer_key_rejects_embedded_space => 12,
    matrix_peer_key_rejects_unicode_suffix => 13,
    matrix_peer_key_rejects_extra_byte => 14,
    matrix_peer_key_rejects_leading_byte => 15,
    matrix_peer_key_rejects_punctuation => 16,
    matrix_peer_key_rejects_null_byte => 17,
    matrix_peer_key_rejects_mixed_case => 18,
    matrix_peer_key_rejects_invalid_hex_suffix => 19,
}
