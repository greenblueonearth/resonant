use crate::consensus::{ConsensusEngine, FinalityCertificate, SignedVote};
use crate::ledger::{Block, Ledger};
use crate::p2p::{P2pNode, ReceivedMessage};
use crate::wallet::Wallet;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

const MAX_PENDING_BLOCKS: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ConsensusMessage {
    Block(Block),
    Vote(SignedVote),
    Certificate(FinalityCertificate),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsensusEvent {
    BlockPending { height: u64, hash: String },
    VoteAccepted { height: u64 },
    BlockFinalized { height: u64, hash: String },
}

pub struct ConsensusP2pNode {
    network: P2pNode,
    identity: Wallet,
    ledger: Ledger,
    consensus: ConsensusEngine,
    pending_blocks: BTreeMap<(u64, String), Block>,
}

impl ConsensusP2pNode {
    pub fn bind(
        address: SocketAddr,
        identity: Wallet,
        trusted_peers: impl IntoIterator<Item = String>,
        ledger: Ledger,
        consensus: ConsensusEngine,
    ) -> Result<Self, String> {
        Self::bind_with_advertise(address, address, identity, trusted_peers, ledger, consensus)
    }

    pub fn bind_with_advertise(
        address: SocketAddr,
        advertised_address: SocketAddr,
        identity: Wallet,
        trusted_peers: impl IntoIterator<Item = String>,
        ledger: Ledger,
        consensus: ConsensusEngine,
    ) -> Result<Self, String> {
        ledger.validate()?;
        if ledger.height() != consensus.finalized_height() {
            return Err(format!(
                "ledger height {} does not match finalized consensus height {}",
                ledger.height(),
                consensus.finalized_height()
            ));
        }
        let network = P2pNode::bind_with_advertise(
            address,
            advertised_address,
            identity.clone(),
            trusted_peers.into_iter().collect(),
        )?;
        Ok(Self {
            network,
            identity,
            ledger,
            consensus,
            pending_blocks: BTreeMap::new(),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.network.local_addr()
    }

    pub fn advertised_addr(&self) -> SocketAddr {
        self.network.advertised_addr()
    }

    pub fn public_key(&self) -> &str {
        self.network.public_key()
    }

    pub fn peers(&self) -> Result<Vec<SocketAddr>, String> {
        self.network.peers()
    }

    pub fn discover(&self, seeds: &[SocketAddr]) -> Result<Vec<SocketAddr>, String> {
        self.network.discover(seeds)
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub fn consensus(&self) -> &ConsensusEngine {
        &self.consensus
    }

    pub fn propose_block(&mut self) -> Result<ConsensusEvent, String> {
        let mut candidate = self.ledger.clone();
        let block = candidate.mine_block()?;
        let message = serde_json::to_value(ConsensusMessage::Block(block.clone()))
            .map_err(|error| format!("could not encode block message: {error}"))?;
        let announce_result = self.network.broadcast(message);
        let event = self.accept_block(block)?;
        announce_result?;
        Ok(event)
    }

    pub fn receive(&mut self, timeout: Duration) -> Result<Option<ConsensusEvent>, String> {
        let message = match self.network.recv_timeout(timeout) {
            Ok(message) => message,
            Err(error) if error == "timed out waiting for a P2P message" => return Ok(None),
            Err(error) => return Err(error),
        };
        self.process_received(message).map(Some)
    }

    fn process_received(&mut self, received: ReceivedMessage) -> Result<ConsensusEvent, String> {
        let message: ConsensusMessage =
            serde_json::from_value(received.payload).map_err(|error| {
                format!(
                    "peer {} sent an invalid consensus message: {error}",
                    received.peer_public_key
                )
            })?;
        match message {
            ConsensusMessage::Block(block) => self.accept_block(block),
            ConsensusMessage::Vote(vote) => self.accept_vote(vote),
            ConsensusMessage::Certificate(certificate) => self.accept_certificate(certificate),
        }
    }

    fn accept_block(&mut self, block: Block) -> Result<ConsensusEvent, String> {
        let expected_height = self.consensus.finalized_height().saturating_add(1);
        if block.index != expected_height || block.index != self.ledger.height().saturating_add(1) {
            if block.index <= self.ledger.height()
                && self.ledger.block_hash_at(block.index) == Some(block.hash.as_str())
            {
                return Ok(ConsensusEvent::BlockFinalized {
                    height: block.index,
                    hash: block.hash,
                });
            }
            return Err("received block is not the next unfinalized block".to_string());
        }
        let mut candidate_ledger = self.ledger.clone();
        candidate_ledger.apply_consensus_block(block.clone())?;
        let key = (block.index, block.hash.clone());
        if !self.pending_blocks.contains_key(&key)
            && self.pending_blocks.len() >= MAX_PENDING_BLOCKS
        {
            return Err(format!(
                "pending block cache reached its {}-block limit",
                MAX_PENDING_BLOCKS
            ));
        }
        self.pending_blocks.insert(key, block.clone());
        if let Some(vote) = self.local_vote(&block)? {
            return self.accept_local_vote(vote);
        }
        Ok(ConsensusEvent::BlockPending {
            height: block.index,
            hash: block.hash,
        })
    }

    fn local_vote(&self, block: &Block) -> Result<Option<SignedVote>, String> {
        let Some(validator) = self.consensus.validator(&self.identity.address) else {
            return Ok(None);
        };
        if validator.jailed || validator.stake == 0 {
            return Ok(None);
        }
        if self
            .consensus
            .vote_for(block.index, &self.identity.address)
            .is_some()
        {
            return Ok(None);
        }
        self.consensus
            .make_vote(&self.identity, block.index, &block.hash)
            .map(Some)
    }

    fn accept_local_vote(&mut self, vote: SignedVote) -> Result<ConsensusEvent, String> {
        let height = vote.height;
        let mut candidate = self.consensus.clone();
        let certificate = candidate.submit_vote(vote.clone())?;
        let vote_message = serde_json::to_value(ConsensusMessage::Vote(vote))
            .map_err(|error| format!("could not encode vote message: {error}"))?;
        let broadcast_vote = self.network.broadcast(vote_message);

        if let Some(certificate) = certificate {
            let event = self.apply_finalized(candidate, certificate.clone())?;
            let certificate_message =
                serde_json::to_value(ConsensusMessage::Certificate(certificate))
                    .map_err(|error| format!("could not encode certificate message: {error}"))?;
            let broadcast_certificate = self.network.broadcast(certificate_message);
            broadcast_vote?;
            broadcast_certificate?;
            return Ok(event);
        }
        self.consensus = candidate;
        broadcast_vote?;
        Ok(ConsensusEvent::VoteAccepted { height })
    }

    fn accept_vote(&mut self, vote: SignedVote) -> Result<ConsensusEvent, String> {
        let key = (vote.height, vote.block_hash.clone());
        if !self.pending_blocks.contains_key(&key) {
            if let Some(certificate) = self.consensus.certificate(vote.height) {
                if certificate.block_hash == vote.block_hash {
                    self.consensus.verify_vote(&vote)?;
                    return Ok(ConsensusEvent::VoteAccepted {
                        height: vote.height,
                    });
                }
            }
            return Err("vote arrived without its matching pending block".to_string());
        }
        let mut candidate = self.consensus.clone();
        if let Some(certificate) = candidate.submit_vote(vote.clone())? {
            let event = self.apply_finalized(candidate, certificate.clone())?;
            let message = serde_json::to_value(ConsensusMessage::Certificate(certificate))
                .map_err(|error| format!("could not encode certificate message: {error}"))?;
            self.network.broadcast(message)?;
            Ok(event)
        } else {
            self.consensus = candidate;
            Ok(ConsensusEvent::VoteAccepted {
                height: vote.height,
            })
        }
    }

    fn accept_certificate(
        &mut self,
        certificate: FinalityCertificate,
    ) -> Result<ConsensusEvent, String> {
        let key = (certificate.height, certificate.block_hash.clone());
        if !self.pending_blocks.contains_key(&key) {
            if self.ledger.height() >= certificate.height
                && self.ledger.block_hash_at(certificate.height)
                    == Some(certificate.block_hash.as_str())
            {
                return Ok(ConsensusEvent::BlockFinalized {
                    height: certificate.height,
                    hash: certificate.block_hash,
                });
            }
            return Err("finality certificate arrived without its matching block".to_string());
        }
        let mut candidate = self.consensus.clone();
        candidate.submit_certificate(certificate.clone())?;
        self.apply_finalized(candidate, certificate)
    }

    fn apply_finalized(
        &mut self,
        consensus: ConsensusEngine,
        certificate: FinalityCertificate,
    ) -> Result<ConsensusEvent, String> {
        let key = (certificate.height, certificate.block_hash.clone());
        let block = self
            .pending_blocks
            .get(&key)
            .cloned()
            .ok_or_else(|| "finality certificate has no matching pending block".to_string())?;
        let mut ledger = self.ledger.clone();
        ledger.apply_consensus_block(block)?;
        for validator in consensus.validators() {
            ledger.set_stake_amount(&validator.address, validator.stake)?;
        }
        ledger.validate()?;
        self.ledger = ledger;
        self.consensus = consensus;
        self.pending_blocks
            .retain(|(height, _), _| *height > certificate.height);
        Ok(ConsensusEvent::BlockFinalized {
            height: certificate.height,
            hash: certificate.block_hash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(wallets: &[Wallet]) -> ConsensusEngine {
        let mut engine = ConsensusEngine::new(3);
        for (index, wallet) in wallets.iter().enumerate() {
            engine
                .register_validator(wallet, if index == 0 { 70 } else { 30 })
                .unwrap();
        }
        engine
    }

    fn fixture(stakes: &[u64]) -> (ConsensusP2pNode, Vec<Wallet>) {
        let validators: Vec<_> = stakes
            .iter()
            .enumerate()
            .map(|(index, _)| Wallet::new(&format!("fixture-validator-{index}")))
            .collect();
        let mut ledger = Ledger::new();
        for (validator, stake) in validators.iter().zip(stakes) {
            ledger.import_wallet(validator).unwrap();
            ledger.stake(&validator.address, *stake).unwrap();
        }
        let identity = Wallet::new("non-validator-node");
        let node = ConsensusP2pNode::bind(
            "127.0.0.1:0".parse().unwrap(),
            identity,
            [],
            ledger,
            engine(&validators),
        )
        .unwrap();
        (node, validators)
    }

    fn candidate_block(ledger: &Ledger) -> Block {
        let mut candidate = ledger.clone();
        candidate.mine_block().unwrap()
    }

    #[test]
    fn block_is_applied_only_after_a_valid_finality_certificate() {
        let validator_a = Wallet::new("validator-a");
        let validator_b = Wallet::new("validator-b");
        let validators = vec![validator_a.clone(), validator_b.clone()];
        let mut ledger = Ledger::new();
        ledger.import_wallet(&validator_a).unwrap();
        ledger.import_wallet(&validator_b).unwrap();
        ledger.stake(&validator_a.address, 7000).unwrap();
        ledger.stake(&validator_b.address, 3000).unwrap();
        let mut node_a = ConsensusP2pNode::bind(
            "127.0.0.1:0".parse().unwrap(),
            validator_a.clone(),
            [validator_b.public_key.clone()],
            ledger.clone(),
            engine(&validators),
        )
        .unwrap();
        let mut node_b = ConsensusP2pNode::bind(
            "127.0.0.1:0".parse().unwrap(),
            validator_b.clone(),
            [validator_a.public_key.clone()],
            ledger.clone(),
            engine(&validators),
        )
        .unwrap();
        node_a.discover(&[node_b.local_addr()]).unwrap();

        assert!(matches!(
            node_a.propose_block().unwrap(),
            ConsensusEvent::BlockFinalized { height: 1, .. }
        ));
        assert_eq!(node_a.ledger().height(), 1);
        assert_eq!(node_b.ledger().height(), 0);

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while node_b.ledger().height() == 0 && std::time::Instant::now() < deadline {
            node_b.receive(Duration::from_millis(100)).unwrap();
        }
        assert_eq!(node_b.ledger().height(), 1);
    }

    #[test]
    fn refuses_to_bind_when_ledger_and_consensus_heights_differ() {
        let validator = Wallet::new("validator");
        let mut consensus = ConsensusEngine::new(3);
        consensus.register_validator(&validator, 100).unwrap();
        let vote = consensus.make_vote(&validator, 1, "block-1").unwrap();
        consensus.submit_vote(vote).unwrap();

        assert!(ConsensusP2pNode::bind(
            "127.0.0.1:0".parse().unwrap(),
            Wallet::new("node"),
            [],
            Ledger::new(),
            consensus
        )
        .is_err());
    }

    #[test]
    fn valid_block_stays_pending_until_finality() {
        let (mut node, _) = fixture(&[70, 30]);
        let block = candidate_block(node.ledger());
        let block_hash = block.hash.clone();

        assert_eq!(
            node.accept_block(block).unwrap(),
            ConsensusEvent::BlockPending {
                height: 1,
                hash: block_hash
            }
        );
        assert_eq!(node.ledger().height(), 0);
        assert_eq!(node.consensus().finalized_height(), 0);
    }

    #[test]
    fn invalid_block_is_rejected_without_mutating_node_state() {
        let (mut node, _) = fixture(&[70, 30]);
        let mut block = candidate_block(node.ledger());
        block.hash.push('x');

        assert!(node.accept_block(block).is_err());
        assert_eq!(node.ledger().height(), 0);
        assert!(node.pending_blocks.is_empty());
    }

    #[test]
    fn rejects_block_that_skips_the_next_height() {
        let (mut node, _) = fixture(&[70, 30]);
        let mut block = candidate_block(node.ledger());
        block.index = 2;

        assert!(node.accept_block(block).is_err());
        assert_eq!(node.ledger().height(), 0);
        assert!(node.pending_blocks.is_empty());
    }

    #[test]
    fn rejects_vote_without_matching_pending_block() {
        let (mut node, validators) = fixture(&[70, 30]);
        let vote = node
            .consensus()
            .make_vote(&validators[0], 1, "unknown-block")
            .unwrap();

        assert!(node.accept_vote(vote).is_err());
        assert_eq!(node.consensus().finalized_height(), 0);
    }

    #[test]
    fn rejects_forged_vote_for_pending_block() {
        let (mut node, validators) = fixture(&[70, 30]);
        let block = candidate_block(node.ledger());
        node.accept_block(block.clone()).unwrap();
        let mut vote = node
            .consensus()
            .make_vote(&validators[0], 1, "different-block")
            .unwrap();
        vote.block_hash = block.hash;

        assert!(node.accept_vote(vote).is_err());
        assert_eq!(node.ledger().height(), 0);
        assert_eq!(node.consensus().finalized_height(), 0);
    }

    #[test]
    fn valid_quorum_vote_finalizes_pending_block_and_syncs_stake() {
        let (mut node, validators) = fixture(&[7000, 3000]);
        let block = candidate_block(node.ledger());
        let block_hash = block.hash.clone();
        node.accept_block(block).unwrap();
        let vote = node
            .consensus()
            .make_vote(&validators[0], 1, &block_hash)
            .unwrap();

        assert_eq!(
            node.accept_vote(vote).unwrap(),
            ConsensusEvent::BlockFinalized {
                height: 1,
                hash: block_hash
            }
        );
        assert_eq!(node.ledger().height(), 1);
        assert_eq!(node.ledger().stake_of(&validators[0].address), 70);
        assert_eq!(node.ledger().stake_of(&validators[1].address), 30);
    }

    #[test]
    fn invalid_finality_certificate_does_not_apply_pending_block() {
        let (mut node, validators) = fixture(&[7000, 3000]);
        let block = candidate_block(node.ledger());
        let block_hash = block.hash.clone();
        node.accept_block(block).unwrap();
        let mut candidate = node.consensus().clone();
        let vote = candidate.make_vote(&validators[0], 1, &block_hash).unwrap();
        let mut certificate = candidate.submit_vote(vote).unwrap().unwrap();
        certificate.signed_stake -= 1;

        assert!(node.accept_certificate(certificate).is_err());
        assert_eq!(node.ledger().height(), 0);
        assert_eq!(node.consensus().finalized_height(), 0);
        assert_eq!(node.pending_blocks.len(), 1);
    }
}
