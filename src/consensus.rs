use crate::wallet::Wallet;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnbondingRequest {
    pub amount: u64,
    pub unlock_height: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Validator {
    pub address: String,
    pub public_key: String,
    pub stake: u64,
    pub jailed: bool,
    pub total_slashed: u64,
    pub unbonding: Vec<UnbondingRequest>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SignedVote {
    pub height: u64,
    pub block_hash: String,
    pub validator: String,
    pub signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalityCertificate {
    pub height: u64,
    pub block_hash: String,
    pub stake_snapshot: Vec<(String, u64)>,
    pub votes: Vec<SignedVote>,
    pub signed_stake: u64,
    pub total_stake: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SlashingEvidence {
    pub height: u64,
    pub validator: String,
    pub first_vote: SignedVote,
    pub conflicting_vote: SignedVote,
    pub slashed_amount: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConsensusEngine {
    validators: BTreeMap<String, Validator>,
    votes: BTreeMap<u64, Vec<SignedVote>>,
    stake_snapshots: BTreeMap<u64, Vec<(String, u64)>>,
    certificates: BTreeMap<u64, FinalityCertificate>,
    slashing_evidence: Vec<SlashingEvidence>,
    finalized_height: u64,
    unbonding_period: u64,
}

impl Default for ConsensusEngine {
    fn default() -> Self {
        Self::new(3)
    }
}

impl ConsensusEngine {
    pub fn new(unbonding_period: u64) -> Self {
        Self {
            validators: BTreeMap::new(),
            votes: BTreeMap::new(),
            stake_snapshots: BTreeMap::new(),
            certificates: BTreeMap::new(),
            slashing_evidence: Vec::new(),
            finalized_height: 0,
            unbonding_period,
        }
    }

    pub fn register_validator(&mut self, wallet: &Wallet, stake: u64) -> Result<(), String> {
        if wallet.address.trim().is_empty()
            || !Wallet::verify(
                &wallet.public_key,
                b"validator-key-check",
                &wallet.sign(b"validator-key-check")?,
            )
        {
            return Err("invalid validator signing key".to_string());
        }
        if Wallet::address_for_public_key(&wallet.public_key)? != wallet.address {
            return Err("validator address does not match public key".to_string());
        }
        if stake == 0 {
            return Err("validator stake must be greater than zero".to_string());
        }
        if self.validators.contains_key(&wallet.address) {
            return Err("validator is already registered".to_string());
        }
        self.validators
            .values()
            .try_fold(stake, |total, validator| total.checked_add(validator.stake))
            .ok_or_else(|| "total validator stake overflows".to_string())?;
        self.validators.insert(
            wallet.address.clone(),
            Validator {
                address: wallet.address.clone(),
                public_key: wallet.public_key.clone(),
                stake,
                jailed: false,
                total_slashed: 0,
                unbonding: Vec::new(),
            },
        );
        Ok(())
    }

    pub fn validator(&self, address: &str) -> Option<&Validator> {
        self.validators.get(address)
    }

    pub fn validators(&self) -> impl Iterator<Item = &Validator> {
        self.validators.values()
    }

    pub fn finalized_height(&self) -> u64 {
        self.finalized_height
    }

    pub fn certificate(&self, height: u64) -> Option<&FinalityCertificate> {
        self.certificates.get(&height)
    }

    pub fn slashing_evidence(&self) -> &[SlashingEvidence] {
        &self.slashing_evidence
    }

    pub fn make_vote(
        &self,
        wallet: &Wallet,
        height: u64,
        block_hash: &str,
    ) -> Result<SignedVote, String> {
        let validator = self
            .validators
            .get(&wallet.address)
            .ok_or_else(|| "wallet is not a registered validator".to_string())?;
        if validator.public_key != wallet.public_key {
            return Err("wallet key does not match registered validator".to_string());
        }
        if validator.jailed || validator.stake == 0 {
            return Err("validator is not eligible to vote".to_string());
        }
        if block_hash.is_empty() {
            return Err("block hash must not be empty".to_string());
        }
        let message = vote_message(height, block_hash, &wallet.address);
        Ok(SignedVote {
            height,
            block_hash: block_hash.to_string(),
            validator: wallet.address.clone(),
            signature: wallet.sign(&message)?,
        })
    }

    pub fn submit_vote(&mut self, vote: SignedVote) -> Result<Option<FinalityCertificate>, String> {
        let validator = self
            .validators
            .get(&vote.validator)
            .ok_or_else(|| "unknown validator".to_string())?;
        if !Wallet::verify(
            &validator.public_key,
            &vote_message(vote.height, &vote.block_hash, &vote.validator),
            &vote.signature,
        ) {
            return Err("invalid validator signature".to_string());
        }

        if let Some(previous_vote) = self
            .votes
            .get(&vote.height)
            .and_then(|votes| {
                votes
                    .iter()
                    .find(|existing| existing.validator == vote.validator)
            })
            .cloned()
        {
            if previous_vote.block_hash == vote.block_hash {
                return Ok(None);
            }
            self.slash_equivocator(previous_vote, vote)?;
            return Ok(None);
        }

        if vote.height != self.finalized_height.saturating_add(1) {
            return Err("vote height is not the next finality height".to_string());
        }
        let validator = self
            .validators
            .get(&vote.validator)
            .ok_or_else(|| "unknown validator".to_string())?;
        if validator.jailed || validator.stake == 0 {
            return Err("validator is not eligible to vote".to_string());
        }

        let snapshot = self
            .stake_snapshots
            .entry(vote.height)
            .or_insert_with(|| {
                self.validators
                    .values()
                    .filter(|validator| !validator.jailed && validator.stake > 0)
                    .map(|validator| (validator.address.clone(), validator.stake))
                    .collect()
            })
            .clone();
        if !snapshot
            .iter()
            .any(|(address, amount)| address == &vote.validator && *amount > 0)
        {
            return Err("validator is not in this height's stake snapshot".to_string());
        }

        self.votes
            .entry(vote.height)
            .or_default()
            .push(vote.clone());
        if let Some(certificate) = self.build_certificate(vote.height, &vote.block_hash)? {
            self.finalized_height = certificate.height;
            self.certificates
                .insert(certificate.height, certificate.clone());
            return Ok(Some(certificate));
        }
        Ok(None)
    }

    pub fn verify_certificate(&self, certificate: &FinalityCertificate) -> Result<(), String> {
        if certificate.height == 0 || certificate.block_hash.is_empty() {
            return Err("certificate height and block hash must be non-empty".to_string());
        }
        let mut previous_address: Option<&str> = None;
        let mut total_stake = 0u64;
        for (address, stake) in &certificate.stake_snapshot {
            if address.is_empty()
                || *stake == 0
                || previous_address.is_some_and(|p| p >= address.as_str())
            {
                return Err("invalid certificate stake snapshot".to_string());
            }
            previous_address = Some(address);
            total_stake = total_stake
                .checked_add(*stake)
                .ok_or_else(|| "certificate total stake overflows".to_string())?;
        }
        if total_stake != certificate.total_stake {
            return Err("certificate total stake does not match snapshot".to_string());
        }

        let mut seen = std::collections::HashSet::new();
        let mut signed_stake = 0u64;
        for vote in &certificate.votes {
            if vote.height != certificate.height
                || vote.block_hash != certificate.block_hash
                || !seen.insert(vote.validator.as_str())
            {
                return Err("certificate contains conflicting or duplicate votes".to_string());
            }
            let stake = certificate
                .stake_snapshot
                .iter()
                .find(|(address, _)| address == &vote.validator)
                .map(|(_, stake)| *stake)
                .ok_or_else(|| "certificate vote is not in stake snapshot".to_string())?;
            let validator = self
                .validators
                .get(&vote.validator)
                .ok_or_else(|| "certificate references unknown validator".to_string())?;
            if !Wallet::verify(
                &validator.public_key,
                &vote_message(vote.height, &vote.block_hash, &vote.validator),
                &vote.signature,
            ) {
                return Err("certificate contains invalid signature".to_string());
            }
            signed_stake = signed_stake
                .checked_add(stake)
                .ok_or_else(|| "certificate signed stake overflows".to_string())?;
        }
        if signed_stake != certificate.signed_stake {
            return Err("certificate signed stake does not match votes".to_string());
        }
        if !has_quorum(signed_stake, total_stake) {
            return Err("certificate does not have a two-thirds stake quorum".to_string());
        }
        Ok(())
    }

    pub fn request_unstake(&mut self, address: &str, amount: u64) -> Result<u64, String> {
        if amount == 0 {
            return Err("unstake amount must be greater than zero".to_string());
        }
        let validator = self
            .validators
            .get_mut(address)
            .ok_or_else(|| "unknown validator".to_string())?;
        let already_unbonding = validator
            .unbonding
            .iter()
            .try_fold(0u64, |sum, request| sum.checked_add(request.amount))
            .ok_or_else(|| "unbonding amount overflows".to_string())?;
        let available = validator.stake.saturating_sub(already_unbonding);
        if amount > available {
            return Err("unstake amount exceeds bonded stake".to_string());
        }
        let unlock_height = self
            .finalized_height
            .checked_add(self.unbonding_period)
            .ok_or_else(|| "unstake unlock height overflows".to_string())?;
        validator.unbonding.push(UnbondingRequest {
            amount,
            unlock_height,
        });
        Ok(unlock_height)
    }

    pub fn withdraw_unstaked(&mut self, address: &str) -> Result<u64, String> {
        let validator = self
            .validators
            .get_mut(address)
            .ok_or_else(|| "unknown validator".to_string())?;
        let amount = validator
            .unbonding
            .iter()
            .filter(|request| request.unlock_height <= self.finalized_height)
            .try_fold(0u64, |sum, request| sum.checked_add(request.amount))
            .ok_or_else(|| "withdrawal amount overflows".to_string())?;
        if amount == 0 {
            return Err("no unstaked amount is ready to withdraw".to_string());
        }
        validator.stake = validator
            .stake
            .checked_sub(amount)
            .ok_or_else(|| "validator stake is below unbonding amount".to_string())?;
        validator
            .unbonding
            .retain(|request| request.unlock_height > self.finalized_height);
        Ok(amount)
    }

    fn build_certificate(
        &self,
        height: u64,
        block_hash: &str,
    ) -> Result<Option<FinalityCertificate>, String> {
        let snapshot = self
            .stake_snapshots
            .get(&height)
            .ok_or_else(|| "missing stake snapshot".to_string())?;
        let total_stake = snapshot
            .iter()
            .try_fold(0u64, |sum, (_, stake)| sum.checked_add(*stake))
            .ok_or_else(|| "total stake overflows".to_string())?;
        let votes: Vec<_> = self
            .votes
            .get(&height)
            .into_iter()
            .flatten()
            .filter(|vote| vote.block_hash == block_hash)
            .filter(|vote| {
                self.validators
                    .get(&vote.validator)
                    .is_some_and(|v| !v.jailed)
            })
            .cloned()
            .collect();
        let signed_stake = votes
            .iter()
            .try_fold(0u64, |sum, vote| {
                let stake = snapshot
                    .iter()
                    .find(|(address, _)| address == &vote.validator)
                    .map(|(_, stake)| *stake)
                    .unwrap_or(0);
                sum.checked_add(stake)
            })
            .ok_or_else(|| "signed stake overflows".to_string())?;
        if !has_quorum(signed_stake, total_stake) {
            return Ok(None);
        }
        let certificate = FinalityCertificate {
            height,
            block_hash: block_hash.to_string(),
            stake_snapshot: snapshot.clone(),
            votes,
            signed_stake,
            total_stake,
        };
        self.verify_certificate(&certificate)?;
        Ok(Some(certificate))
    }

    fn slash_equivocator(
        &mut self,
        first_vote: SignedVote,
        conflicting_vote: SignedVote,
    ) -> Result<(), String> {
        let validator = self
            .validators
            .get_mut(&conflicting_vote.validator)
            .ok_or_else(|| "unknown equivocating validator".to_string())?;
        let slashed_amount = (validator.stake / 10 + u64::from(validator.stake % 10 != 0))
            .max(1)
            .min(validator.stake);
        let mut remaining = slashed_amount;
        for request in validator.unbonding.iter_mut().rev() {
            let reduction = request.amount.min(remaining);
            request.amount -= reduction;
            remaining -= reduction;
            if remaining == 0 {
                break;
            }
        }
        validator.unbonding.retain(|request| request.amount > 0);
        validator.stake -= slashed_amount;
        validator.total_slashed = validator
            .total_slashed
            .checked_add(slashed_amount)
            .ok_or_else(|| "total slashed amount overflows".to_string())?;
        validator.jailed = true;
        if let Some(votes) = self.votes.get_mut(&conflicting_vote.height) {
            votes.retain(|vote| vote.validator != conflicting_vote.validator);
        }
        self.slashing_evidence.push(SlashingEvidence {
            height: conflicting_vote.height,
            validator: conflicting_vote.validator.clone(),
            first_vote,
            conflicting_vote,
            slashed_amount,
        });
        Ok(())
    }
}

fn vote_message(height: u64, block_hash: &str, validator: &str) -> Vec<u8> {
    serde_json::to_vec(&("resonant/finality-vote/v1", height, block_hash, validator))
        .expect("vote message serialization is infallible")
}

fn has_quorum(signed_stake: u64, total_stake: u64) -> bool {
    total_stake > 0 && u128::from(signed_stake) * 3 > u128::from(total_stake) * 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(stakes: &[u64]) -> (ConsensusEngine, Vec<Wallet>) {
        let mut engine = ConsensusEngine::new(3);
        let wallets: Vec<_> = stakes
            .iter()
            .enumerate()
            .map(|(index, _)| Wallet::new(&format!("validator-{index}")))
            .collect();
        for (wallet, stake) in wallets.iter().zip(stakes) {
            engine.register_validator(wallet, *stake).unwrap();
        }
        (engine, wallets)
    }

    fn submit(engine: &mut ConsensusEngine, wallet: &Wallet, height: u64, hash: &str) {
        let vote = engine.make_vote(wallet, height, hash).unwrap();
        engine.submit_vote(vote).unwrap();
    }

    fn finalize_next(engine: &mut ConsensusEngine, wallet: &Wallet) {
        let height = engine.finalized_height() + 1;
        let vote = engine
            .make_vote(wallet, height, &format!("block-{height}"))
            .unwrap();
        assert!(engine.submit_vote(vote).unwrap().is_some());
    }

    mod signatures {
        use super::*;

        #[test]
        fn valid_signature_verifies() {
            let wallet = Wallet::new("signer");
            let signature = wallet.sign(b"message").unwrap();
            assert!(Wallet::verify(&wallet.public_key, b"message", &signature));
        }

        #[test]
        fn signature_rejects_modified_message() {
            let wallet = Wallet::new("signer");
            let signature = wallet.sign(b"original").unwrap();
            assert!(!Wallet::verify(&wallet.public_key, b"modified", &signature));
        }

        #[test]
        fn signature_rejects_modified_signature() {
            let wallet = Wallet::new("signer");
            let mut signature = wallet.sign(b"message").unwrap();
            signature.replace_range(0..2, "00");
            assert!(!Wallet::verify(&wallet.public_key, b"message", &signature));
        }

        #[test]
        fn signature_rejects_another_public_key() {
            let signer = Wallet::new("signer");
            let other = Wallet::new("other");
            let signature = signer.sign(b"message").unwrap();
            assert!(!Wallet::verify(&other.public_key, b"message", &signature));
        }

        #[test]
        fn signature_rejects_malformed_public_key_hex() {
            let wallet = Wallet::new("signer");
            let signature = wallet.sign(b"message").unwrap();
            assert!(!Wallet::verify("not-hex", b"message", &signature));
        }

        #[test]
        fn signature_rejects_malformed_signature_hex() {
            let wallet = Wallet::new("signer");
            assert!(!Wallet::verify(&wallet.public_key, b"message", "not-hex"));
        }

        #[test]
        fn signature_rejects_short_public_key() {
            let wallet = Wallet::new("signer");
            let signature = wallet.sign(b"message").unwrap();
            assert!(!Wallet::verify("abcd", b"message", &signature));
        }

        #[test]
        fn signature_rejects_short_signature() {
            let wallet = Wallet::new("signer");
            assert!(!Wallet::verify(&wallet.public_key, b"message", "abcd"));
        }

        #[test]
        fn signature_supports_empty_message() {
            let wallet = Wallet::new("signer");
            let signature = wallet.sign(b"").unwrap();
            assert!(Wallet::verify(&wallet.public_key, b"", &signature));
        }

        #[test]
        fn signature_supports_unicode_message() {
            let wallet = Wallet::new("signer");
            let signature = wallet.sign("finality-đồng-thuận".as_bytes()).unwrap();
            assert!(Wallet::verify(
                &wallet.public_key,
                "finality-đồng-thuận".as_bytes(),
                &signature
            ));
        }

        #[test]
        fn ed25519_signatures_are_deterministic() {
            let wallet = Wallet::new("signer");
            assert_eq!(wallet.sign(b"same").unwrap(), wallet.sign(b"same").unwrap());
        }

        #[test]
        fn different_messages_produce_different_signatures() {
            let wallet = Wallet::new("signer");
            assert_ne!(wallet.sign(b"one").unwrap(), wallet.sign(b"two").unwrap());
        }

        #[test]
        fn vote_is_bound_to_registered_validator() {
            let (engine, wallets) = setup(&[10]);
            let vote = engine.make_vote(&wallets[0], 1, "block").unwrap();
            assert_eq!(vote.validator, wallets[0].address);
        }

        #[test]
        fn vote_signature_verifies() {
            let (engine, wallets) = setup(&[10]);
            let vote = engine.make_vote(&wallets[0], 1, "block").unwrap();
            assert!(Wallet::verify(
                &wallets[0].public_key,
                &vote_message(vote.height, &vote.block_hash, &vote.validator),
                &vote.signature
            ));
        }

        #[test]
        fn vote_signature_is_bound_to_height() {
            let (engine, wallets) = setup(&[10]);
            let mut vote = engine.make_vote(&wallets[0], 1, "block").unwrap();
            vote.height = 2;
            assert!(engine.clone().submit_vote(vote).is_err());
        }

        #[test]
        fn vote_signature_is_bound_to_block_hash() {
            let (engine, wallets) = setup(&[10]);
            let mut vote = engine.make_vote(&wallets[0], 1, "block-a").unwrap();
            vote.block_hash = "block-b".to_string();
            assert!(engine.clone().submit_vote(vote).is_err());
        }

        #[test]
        fn vote_signature_is_bound_to_validator_address() {
            let (engine, wallets) = setup(&[10, 10]);
            let mut vote = engine.make_vote(&wallets[0], 1, "block").unwrap();
            vote.validator = wallets[1].address.clone();
            assert!(engine.clone().submit_vote(vote).is_err());
        }

        #[test]
        fn unregistered_wallet_cannot_create_vote() {
            let engine = ConsensusEngine::new(3);
            assert!(engine
                .make_vote(&Wallet::new("outsider"), 1, "block")
                .is_err());
        }

        #[test]
        fn mismatched_wallet_key_cannot_sign_registered_vote() {
            let (engine, wallets) = setup(&[10]);
            let mut altered = wallets[0].clone();
            altered.public_key = Wallet::new("other").public_key;
            assert!(engine.make_vote(&altered, 1, "block").is_err());

            let mut forged_address = wallets[0].clone();
            forged_address.address.push('x');
            let mut registry = ConsensusEngine::new(3);
            assert!(registry.register_validator(&forged_address, 10).is_err());
        }

        #[test]
        fn malformed_wallet_secret_cannot_sign() {
            let mut wallet = Wallet::new("signer");
            wallet.secret = "bad".to_string();
            assert!(wallet.sign(b"message").is_err());
        }
    }

    mod slashing {
        use super::*;

        fn double_vote(engine: &mut ConsensusEngine, wallet: &Wallet, height: u64) {
            submit(engine, wallet, height, "block-a");
            let conflicting = engine.make_vote(wallet, height, "block-b").unwrap();
            engine.submit_vote(conflicting).unwrap();
        }

        #[test]
        fn equivocation_slashes_ten_percent() {
            let (mut engine, wallets) = setup(&[100]);
            double_vote(&mut engine, &wallets[0], 1);
            assert_eq!(engine.validator(&wallets[0].address).unwrap().stake, 90);
        }

        #[test]
        fn equivocation_jails_validator() {
            let (mut engine, wallets) = setup(&[100]);
            double_vote(&mut engine, &wallets[0], 1);
            assert!(engine.validator(&wallets[0].address).unwrap().jailed);
        }

        #[test]
        fn equivocation_records_both_signed_votes() {
            let (mut engine, wallets) = setup(&[100]);
            double_vote(&mut engine, &wallets[0], 1);
            let evidence = &engine.slashing_evidence()[0];
            assert_ne!(
                evidence.first_vote.block_hash,
                evidence.conflicting_vote.block_hash
            );
            assert_eq!(evidence.validator, wallets[0].address);
        }

        #[test]
        fn same_vote_is_idempotent_and_not_slashable() {
            let (mut engine, wallets) = setup(&[100]);
            let vote = engine.make_vote(&wallets[0], 1, "block-a").unwrap();
            engine.submit_vote(vote.clone()).unwrap();
            engine.submit_vote(vote).unwrap();
            assert_eq!(engine.validator(&wallets[0].address).unwrap().stake, 100);
            assert!(engine.slashing_evidence().is_empty());
        }

        #[test]
        fn invalid_conflicting_signature_does_not_slash() {
            let (mut engine, wallets) = setup(&[100]);
            submit(&mut engine, &wallets[0], 1, "block-a");
            let mut vote = engine.make_vote(&wallets[0], 1, "block-b").unwrap();
            vote.signature = "00".repeat(64);
            assert!(engine.submit_vote(vote).is_err());
            assert_eq!(engine.validator(&wallets[0].address).unwrap().stake, 100);
        }

        #[test]
        fn conflicting_votes_at_different_heights_are_not_equivocation() {
            let (mut engine, wallets) = setup(&[100]);
            finalize_next(&mut engine, &wallets[0]);
            submit(&mut engine, &wallets[0], 2, "different-block");
            assert!(!engine.validator(&wallets[0].address).unwrap().jailed);
        }

        #[test]
        fn equivocation_removes_prior_vote_from_tally() {
            let (mut engine, wallets) = setup(&[33, 33, 34]);
            submit(&mut engine, &wallets[0], 1, "block-a");
            submit(&mut engine, &wallets[1], 1, "block-a");
            double_vote(&mut engine, &wallets[0], 1);
            assert!(engine.certificate(1).is_none());
        }

        #[test]
        fn slash_amount_rounds_up() {
            let (mut engine, wallets) = setup(&[11]);
            double_vote(&mut engine, &wallets[0], 1);
            assert_eq!(engine.slashing_evidence()[0].slashed_amount, 2);
        }

        #[test]
        fn smallest_stake_is_fully_slashed() {
            let (mut engine, wallets) = setup(&[1]);
            double_vote(&mut engine, &wallets[0], 1);
            assert_eq!(engine.validator(&wallets[0].address).unwrap().stake, 0);
        }

        #[test]
        fn slash_updates_total_slashed() {
            let (mut engine, wallets) = setup(&[50]);
            double_vote(&mut engine, &wallets[0], 1);
            assert_eq!(
                engine.validator(&wallets[0].address).unwrap().total_slashed,
                5
            );
        }

        #[test]
        fn evidence_contains_verifiable_signatures() {
            let (mut engine, wallets) = setup(&[100]);
            double_vote(&mut engine, &wallets[0], 1);
            for vote in [
                &engine.slashing_evidence()[0].first_vote,
                &engine.slashing_evidence()[0].conflicting_vote,
            ] {
                assert!(Wallet::verify(
                    &wallets[0].public_key,
                    &vote_message(vote.height, &vote.block_hash, &vote.validator),
                    &vote.signature
                ));
            }
        }

        #[test]
        fn vote_signed_by_another_wallet_cannot_impersonate_validator() {
            let (mut engine, wallets) = setup(&[100]);
            let other = Wallet::new("other");
            let signature = other
                .sign(&vote_message(1, "block-a", &wallets[0].address))
                .unwrap();
            let vote = SignedVote {
                height: 1,
                block_hash: "block-a".to_string(),
                validator: wallets[0].address.clone(),
                signature,
            };
            assert!(engine.submit_vote(vote).is_err());
            assert!(engine.slashing_evidence().is_empty());
        }

        #[test]
        fn jailed_validator_cannot_make_future_votes() {
            let (mut engine, wallets) = setup(&[100]);
            double_vote(&mut engine, &wallets[0], 1);
            assert!(engine.make_vote(&wallets[0], 1, "block-c").is_err());
        }

        #[test]
        fn jailed_validator_cannot_submit_new_vote() {
            let (mut engine, wallets) = setup(&[100]);
            double_vote(&mut engine, &wallets[0], 1);
            let vote = SignedVote {
                height: 2,
                block_hash: "block-2".to_string(),
                validator: wallets[0].address.clone(),
                signature: wallets[0]
                    .sign(&vote_message(2, "block-2", &wallets[0].address))
                    .unwrap(),
            };
            assert!(engine.submit_vote(vote).is_err());
        }

        #[test]
        fn evidence_serializes_and_round_trips() {
            let (mut engine, wallets) = setup(&[100]);
            double_vote(&mut engine, &wallets[0], 1);
            let json = serde_json::to_vec(&engine.slashing_evidence()).unwrap();
            let restored: Vec<SlashingEvidence> = serde_json::from_slice(&json).unwrap();
            assert_eq!(restored, engine.slashing_evidence());
        }

        #[test]
        fn slashing_reduces_pending_unbonding_amount() {
            let (mut engine, wallets) = setup(&[100]);
            engine.request_unstake(&wallets[0].address, 20).unwrap();
            double_vote(&mut engine, &wallets[0], 1);
            let validator = engine.validator(&wallets[0].address).unwrap();
            assert_eq!(validator.stake, 90);
            assert_eq!(validator.unbonding[0].amount, 10);
        }

        #[test]
        fn equivocation_after_finality_is_still_slashed() {
            let (mut engine, wallets) = setup(&[100]);
            finalize_next(&mut engine, &wallets[0]);
            let conflicting = engine
                .make_vote(&wallets[0], 1, "conflicting-finalized-hash")
                .unwrap();
            engine.submit_vote(conflicting).unwrap();
            assert_eq!(engine.validator(&wallets[0].address).unwrap().stake, 90);
        }

        #[test]
        fn slash_state_survives_engine_serialization() {
            let (mut engine, wallets) = setup(&[100]);
            double_vote(&mut engine, &wallets[0], 1);
            let json = serde_json::to_vec(&engine).unwrap();
            let restored: ConsensusEngine = serde_json::from_slice(&json).unwrap();
            assert!(restored.validator(&wallets[0].address).unwrap().jailed);
            assert_eq!(restored.slashing_evidence().len(), 1);
        }

        #[test]
        fn invalid_unregistered_equivocation_is_not_recorded() {
            let (mut engine, _) = setup(&[100]);
            let unknown = Wallet::new("unknown");
            let vote = SignedVote {
                height: 1,
                block_hash: "hash".to_string(),
                validator: unknown.address.clone(),
                signature: unknown
                    .sign(&vote_message(1, "hash", &unknown.address))
                    .unwrap(),
            };
            assert!(engine.submit_vote(vote).is_err());
            assert!(engine.slashing_evidence().is_empty());
        }

        #[test]
        fn slashed_stake_never_exceeds_original_stake() {
            let (mut engine, wallets) = setup(&[3]);
            double_vote(&mut engine, &wallets[0], 1);
            let validator = engine.validator(&wallets[0].address).unwrap();
            assert!(validator.total_slashed <= 3);
            assert_eq!(validator.stake + validator.total_slashed, 3);
        }
    }

    mod unstaking {
        use super::*;

        #[test]
        fn request_uses_finalized_height_for_unlock() {
            let (mut engine, wallets) = setup(&[100]);
            assert_eq!(engine.request_unstake(&wallets[0].address, 10).unwrap(), 3);
        }

        #[test]
        fn zero_unstake_is_rejected() {
            let (mut engine, wallets) = setup(&[100]);
            assert!(engine.request_unstake(&wallets[0].address, 0).is_err());
        }

        #[test]
        fn unknown_validator_cannot_unstake() {
            let mut engine = ConsensusEngine::new(3);
            assert!(engine.request_unstake("unknown", 1).is_err());
        }

        #[test]
        fn unstake_cannot_exceed_bonded_amount() {
            let (mut engine, wallets) = setup(&[100]);
            assert!(engine.request_unstake(&wallets[0].address, 101).is_err());
        }

        #[test]
        fn pending_unstake_requests_reserve_stake() {
            let (mut engine, wallets) = setup(&[100]);
            engine.request_unstake(&wallets[0].address, 60).unwrap();
            assert!(engine.request_unstake(&wallets[0].address, 41).is_err());
        }

        #[test]
        fn request_does_not_release_stake_immediately() {
            let (mut engine, wallets) = setup(&[100]);
            engine.request_unstake(&wallets[0].address, 40).unwrap();
            assert_eq!(engine.validator(&wallets[0].address).unwrap().stake, 100);
        }

        #[test]
        fn withdrawal_before_unbonding_period_is_rejected() {
            let (mut engine, wallets) = setup(&[100]);
            engine.request_unstake(&wallets[0].address, 40).unwrap();
            assert!(engine.withdraw_unstaked(&wallets[0].address).is_err());
        }

        #[test]
        fn withdrawal_succeeds_at_unlock_height() {
            let (mut engine, wallets) = setup(&[100]);
            engine.request_unstake(&wallets[0].address, 40).unwrap();
            for _ in 0..3 {
                finalize_next(&mut engine, &wallets[0]);
            }
            assert_eq!(engine.withdraw_unstaked(&wallets[0].address).unwrap(), 40);
        }

        #[test]
        fn withdrawal_cannot_be_repeated() {
            let (mut engine, wallets) = setup(&[100]);
            engine.request_unstake(&wallets[0].address, 40).unwrap();
            for _ in 0..3 {
                finalize_next(&mut engine, &wallets[0]);
            }
            engine.withdraw_unstaked(&wallets[0].address).unwrap();
            assert!(engine.withdraw_unstaked(&wallets[0].address).is_err());
        }

        #[test]
        fn partial_withdrawal_reduces_stake() {
            let (mut engine, wallets) = setup(&[100]);
            engine.request_unstake(&wallets[0].address, 35).unwrap();
            for _ in 0..3 {
                finalize_next(&mut engine, &wallets[0]);
            }
            engine.withdraw_unstaked(&wallets[0].address).unwrap();
            assert_eq!(engine.validator(&wallets[0].address).unwrap().stake, 65);
        }

        #[test]
        fn full_unstake_removes_validator_voting_weight() {
            let (mut engine, wallets) = setup(&[100]);
            engine.request_unstake(&wallets[0].address, 100).unwrap();
            for _ in 0..3 {
                finalize_next(&mut engine, &wallets[0]);
            }
            engine.withdraw_unstaked(&wallets[0].address).unwrap();
            assert_eq!(engine.validator(&wallets[0].address).unwrap().stake, 0);
            assert!(engine.make_vote(&wallets[0], 4, "after-unstake").is_err());
        }

        #[test]
        fn validator_remains_eligible_during_unbonding() {
            let (mut engine, wallets) = setup(&[100]);
            engine.request_unstake(&wallets[0].address, 100).unwrap();
            assert!(engine.make_vote(&wallets[0], 1, "during-unbonding").is_ok());
        }

        #[test]
        fn unbonding_is_enforced_by_finality_not_caller_input() {
            let (mut engine, wallets) = setup(&[100]);
            engine.request_unstake(&wallets[0].address, 10).unwrap();
            assert!(engine.withdraw_unstaked(&wallets[0].address).is_err());
            assert_eq!(engine.finalized_height(), 0);
        }

        #[test]
        fn requests_at_different_finalized_heights_unlock_separately() {
            let (mut engine, wallets) = setup(&[100]);
            engine.request_unstake(&wallets[0].address, 10).unwrap();
            finalize_next(&mut engine, &wallets[0]);
            finalize_next(&mut engine, &wallets[0]);
            engine.request_unstake(&wallets[0].address, 20).unwrap();
            finalize_next(&mut engine, &wallets[0]);
            assert_eq!(engine.withdraw_unstaked(&wallets[0].address).unwrap(), 10);
            assert_eq!(engine.validator(&wallets[0].address).unwrap().stake, 90);
            finalize_next(&mut engine, &wallets[0]);
            finalize_next(&mut engine, &wallets[0]);
            assert_eq!(engine.withdraw_unstaked(&wallets[0].address).unwrap(), 20);
        }

        #[test]
        fn withdrawal_only_includes_matured_requests() {
            let (mut engine, wallets) = setup(&[100]);
            engine.request_unstake(&wallets[0].address, 10).unwrap();
            finalize_next(&mut engine, &wallets[0]);
            finalize_next(&mut engine, &wallets[0]);
            engine.request_unstake(&wallets[0].address, 20).unwrap();
            finalize_next(&mut engine, &wallets[0]);
            assert_eq!(engine.withdraw_unstaked(&wallets[0].address).unwrap(), 10);
            assert_eq!(
                engine
                    .validator(&wallets[0].address)
                    .unwrap()
                    .unbonding
                    .len(),
                1
            );
        }

        #[test]
        fn multiple_matured_requests_withdraw_together() {
            let (mut engine, wallets) = setup(&[100]);
            engine.request_unstake(&wallets[0].address, 10).unwrap();
            engine.request_unstake(&wallets[0].address, 20).unwrap();
            for _ in 0..3 {
                finalize_next(&mut engine, &wallets[0]);
            }
            assert_eq!(engine.withdraw_unstaked(&wallets[0].address).unwrap(), 30);
        }

        #[test]
        fn unlock_height_overflow_is_rejected() {
            let (mut engine, wallets) = setup(&[100]);
            engine.finalized_height = u64::MAX - 1;
            assert!(engine.request_unstake(&wallets[0].address, 1).is_err());
        }

        #[test]
        fn zero_period_allows_withdrawal_at_current_finalized_height() {
            let (mut engine, wallets) = setup(&[100]);
            engine.unbonding_period = 0;
            engine.request_unstake(&wallets[0].address, 10).unwrap();
            assert_eq!(engine.withdraw_unstaked(&wallets[0].address).unwrap(), 10);
        }

        #[test]
        fn unknown_validator_cannot_withdraw() {
            let mut engine = ConsensusEngine::new(3);
            assert!(engine.withdraw_unstaked("unknown").is_err());
        }

        #[test]
        fn unbonding_requests_survive_serialization() {
            let (mut engine, wallets) = setup(&[100]);
            engine.request_unstake(&wallets[0].address, 25).unwrap();
            let serialized = serde_json::to_vec(&engine).unwrap();
            let mut restored: ConsensusEngine = serde_json::from_slice(&serialized).unwrap();
            assert_eq!(
                restored.validator(&wallets[0].address).unwrap().unbonding[0].amount,
                25
            );
            assert!(restored.withdraw_unstaked(&wallets[0].address).is_err());
        }
    }

    mod finality {
        use super::*;

        fn vote(wallet: &Wallet, height: u64, hash: &str) -> SignedVote {
            SignedVote {
                height,
                block_hash: hash.to_string(),
                validator: wallet.address.clone(),
                signature: wallet
                    .sign(&vote_message(height, hash, &wallet.address))
                    .unwrap(),
            }
        }

        fn certificate(stakes: &[u64]) -> (ConsensusEngine, Vec<Wallet>, FinalityCertificate) {
            let (mut engine, wallets) = setup(stakes);
            let mut result = None;
            for wallet in &wallets {
                result = engine
                    .submit_vote(vote(wallet, 1, "block-a"))
                    .unwrap()
                    .or(result);
                if result.is_some() {
                    break;
                }
            }
            (
                engine,
                wallets,
                result.expect("test stakes must reach quorum"),
            )
        }

        #[test]
        fn sub_quorum_does_not_finalize() {
            let (mut engine, wallets) = setup(&[40, 30, 30]);
            assert!(engine
                .submit_vote(vote(&wallets[0], 1, "a"))
                .unwrap()
                .is_none());
            assert_eq!(engine.finalized_height(), 0);
        }

        #[test]
        fn exactly_two_thirds_does_not_finalize() {
            let (mut engine, wallets) = setup(&[2, 2, 2]);
            engine.submit_vote(vote(&wallets[0], 1, "a")).unwrap();
            let result = engine.submit_vote(vote(&wallets[1], 1, "a")).unwrap();
            assert!(result.is_none());
            assert_eq!(engine.finalized_height(), 0);
        }

        #[test]
        fn more_than_two_thirds_finalizes() {
            let (mut engine, wallets) = setup(&[50, 30, 20]);
            engine.submit_vote(vote(&wallets[0], 1, "a")).unwrap();
            let result = engine.submit_vote(vote(&wallets[1], 1, "a")).unwrap();
            assert!(result.is_some());
            assert_eq!(engine.finalized_height(), 1);
        }

        #[test]
        fn certificate_records_signed_and_total_stake() {
            let (_, _, certificate) = certificate(&[50, 30, 20]);
            assert_eq!(certificate.signed_stake, 80);
            assert_eq!(certificate.total_stake, 100);
        }

        #[test]
        fn valid_certificate_verifies() {
            let (engine, _, certificate) = certificate(&[50, 30, 20]);
            assert!(engine.verify_certificate(&certificate).is_ok());
        }

        #[test]
        fn certificate_rejects_invalid_signature() {
            let (engine, _, mut certificate) = certificate(&[100]);
            certificate.votes[0].signature = "00".repeat(64);
            assert!(engine.verify_certificate(&certificate).is_err());
        }

        #[test]
        fn certificate_rejects_duplicate_validator_vote() {
            let (engine, _, mut certificate) = certificate(&[100]);
            certificate.votes.push(certificate.votes[0].clone());
            assert!(engine.verify_certificate(&certificate).is_err());
        }

        #[test]
        fn certificate_rejects_mixed_heights() {
            let (engine, _, mut certificate) = certificate(&[100]);
            certificate.votes[0].height += 1;
            assert!(engine.verify_certificate(&certificate).is_err());

            let mut zero_height = certificate.clone();
            zero_height.votes[0].height = 1;
            zero_height.height = 0;
            assert!(engine.verify_certificate(&zero_height).is_err());
            zero_height.height = 1;
            zero_height.block_hash.clear();
            assert!(engine.verify_certificate(&zero_height).is_err());
        }

        #[test]
        fn certificate_rejects_mixed_block_hashes() {
            let (engine, _, mut certificate) = certificate(&[100]);
            certificate.votes[0].block_hash.push('x');
            assert!(engine.verify_certificate(&certificate).is_err());
        }

        #[test]
        fn certificate_rejects_incorrect_signed_weight() {
            let (engine, _, mut certificate) = certificate(&[100]);
            certificate.signed_stake = 1;
            assert!(engine.verify_certificate(&certificate).is_err());
        }

        #[test]
        fn certificate_rejects_incorrect_total_weight() {
            let (engine, _, mut certificate) = certificate(&[100]);
            certificate.total_stake += 1;
            assert!(engine.verify_certificate(&certificate).is_err());
        }

        #[test]
        fn certificate_rejects_unsorted_snapshot() {
            let (engine, _, mut certificate) = certificate(&[50, 50]);
            certificate.stake_snapshot.reverse();
            assert!(engine.verify_certificate(&certificate).is_err());
        }

        #[test]
        fn certificate_rejects_snapshot_total_overflow() {
            let (engine, _, mut certificate) = certificate(&[100]);
            certificate.stake_snapshot = vec![("a".to_string(), u64::MAX), ("b".to_string(), 1)];
            assert!(engine.verify_certificate(&certificate).is_err());

            let mut registry = ConsensusEngine::new(3);
            let first = Wallet::new("max-stake");
            let second = Wallet::new("overflow-stake");
            registry.register_validator(&first, u64::MAX).unwrap();
            assert!(registry.register_validator(&second, 1).is_err());
            assert_eq!(registry.validators().count(), 1);
        }

        #[test]
        fn certificate_rejects_duplicate_snapshot_validator() {
            let (engine, _, mut certificate) = certificate(&[100]);
            let entry = certificate.stake_snapshot[0].clone();
            certificate.stake_snapshot.push(entry);
            certificate
                .stake_snapshot
                .sort_by(|left, right| left.0.cmp(&right.0));
            assert!(engine.verify_certificate(&certificate).is_err());
        }

        #[test]
        fn future_height_vote_is_rejected() {
            let (mut engine, wallets) = setup(&[100]);
            let future_vote = vote(&wallets[0], 2, "future");
            assert!(engine.submit_vote(future_vote).is_err());
        }

        #[test]
        fn unregistered_vote_is_rejected() {
            let (mut engine, _) = setup(&[100]);
            let outsider = Wallet::new("outsider");
            let message = vote_message(1, "block", &outsider.address);
            let unknown_vote = SignedVote {
                height: 1,
                block_hash: "block".to_string(),
                validator: outsider.address.clone(),
                signature: outsider.sign(&message).unwrap(),
            };
            assert!(engine.submit_vote(unknown_vote).is_err());
        }

        #[test]
        fn invalid_signature_does_not_count_toward_quorum() {
            let (mut engine, wallets) = setup(&[70, 30]);
            let mut invalid = vote(&wallets[0], 1, "block");
            invalid.signature = "00".repeat(64);
            assert!(engine.submit_vote(invalid).is_err());
            assert_eq!(engine.finalized_height(), 0);
        }

        #[test]
        fn duplicate_vote_cannot_inflate_quorum() {
            let (mut engine, wallets) = setup(&[50, 30, 20]);
            let first_vote = vote(&wallets[0], 1, "block");
            engine.submit_vote(first_vote.clone()).unwrap();
            engine.submit_vote(first_vote).unwrap();
            assert_eq!(engine.finalized_height(), 0);
            assert!(engine
                .submit_vote(vote(&wallets[1], 1, "block"))
                .unwrap()
                .is_some());
        }

        #[test]
        fn stake_snapshot_is_frozen_when_round_starts() {
            let (mut engine, wallets) = setup(&[40, 30, 30]);
            engine.submit_vote(vote(&wallets[0], 1, "block")).unwrap();
            let late_validator = Wallet::new("late-validator");
            engine.register_validator(&late_validator, 100).unwrap();
            let certificate = engine
                .submit_vote(vote(&wallets[1], 1, "block"))
                .unwrap()
                .unwrap();
            assert_eq!(certificate.total_stake, 100);
            assert!(!certificate
                .stake_snapshot
                .iter()
                .any(|(address, _)| address == &late_validator.address));
        }

        #[test]
        fn finality_advances_one_height_at_a_time() {
            let (mut engine, wallets) = setup(&[100]);
            assert!(engine.submit_vote(vote(&wallets[0], 2, "two")).is_err());
            let first = vote(&wallets[0], 1, "one");
            assert!(engine.submit_vote(first).unwrap().is_some());
            let second = vote(&wallets[0], 2, "two");
            assert!(engine.submit_vote(second).unwrap().is_some());
            assert_eq!(engine.finalized_height(), 2);
        }
    }
}
