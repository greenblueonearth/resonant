use crate::wallet::Wallet;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Transaction {
    pub id: String,
    pub sender: String,
    pub recipient: String,
    pub amount: u64,
    pub fee: u64,
    pub nonce: u64,
    pub timestamp: i64,
    pub stealth_commitment: String,
    pub nullifier: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub public_key: String,
    pub signature: String,
}

impl Transaction {
    pub fn hash(&self) -> String {
        let payload = serde_json::to_vec(self).expect("transaction serialization is infallible");
        blake3::hash(&payload).to_hex().to_string()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    pub index: u64,
    pub timestamp: i64,
    pub prev_hash: String,
    pub transactions: Vec<Transaction>,
    pub merkle_root: String,
    pub hash: String,
    #[serde(default = "genesis_validator")]
    pub validator: String,
    #[serde(default)]
    pub stake_snapshot: Vec<(String, u64)>,
}

fn genesis_validator() -> String {
    "GENESIS".to_string()
}

const GENESIS_TIMESTAMP: i64 = 0;

impl Block {
    pub fn genesis() -> Self {
        let timestamp = GENESIS_TIMESTAMP;
        let prev_hash = "GENESIS".to_string();
        let hash = blake3::hash(format!("{}:{}:genesis", timestamp, prev_hash).as_bytes())
            .to_hex()
            .to_string();
        Self {
            index: 0,
            timestamp,
            prev_hash,
            transactions: Vec::new(),
            merkle_root: "0x0".to_string(),
            hash,
            validator: genesis_validator(),
            stake_snapshot: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BenchmarkResult {
    pub tx_count: u64,
    pub elapsed_ms: f64,
    pub tx_per_second: f64,
    pub block_count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Ledger {
    balances: HashMap<String, u64>,
    #[serde(default = "genesis_stakes")]
    stakes: HashMap<String, u64>,
    pub pending: Vec<Transaction>,
    chain: Vec<Block>,
    nonce: u64,
}

impl Ledger {
    pub fn new() -> Self {
        let mut balances = HashMap::new();
        for label in [
            "alice", "bob", "charlie", "dana", "erin", "frank", "grace", "henry",
        ] {
            balances.insert(label.to_string(), 1_000_000_000u64);
        }

        Self {
            balances,
            stakes: genesis_stakes(),
            pending: Vec::new(),
            chain: vec![Block::genesis()],
            nonce: 0,
        }
    }

    pub fn create_wallet(&mut self, label: &str) -> Wallet {
        let wallet = Wallet::new(label);
        self.balances
            .entry(wallet.address.clone())
            .or_insert(10_000u64);
        wallet
    }

    pub fn import_wallet(&mut self, wallet: &Wallet) -> Result<(), String> {
        if Wallet::address_for_public_key(&wallet.public_key)? != wallet.address
            || !Wallet::verify(
                &wallet.public_key,
                b"validator-key-check",
                &wallet.sign(b"validator-key-check")?,
            )
        {
            return Err("invalid wallet signing key".to_string());
        }
        if self.balances.contains_key(&wallet.address) {
            return Err("wallet already exists in ledger".to_string());
        }
        self.balances.insert(wallet.address.clone(), 10_000);
        Ok(())
    }

    pub fn validate_block_for_consensus(&self, block: &Block) -> Result<(), String> {
        self.validate_block(block)?;
        for transaction in &block.transactions {
            validate_transaction_authorization(transaction)?;
        }
        Ok(())
    }

    pub fn apply_consensus_block(&mut self, block: Block) -> Result<(), String> {
        self.validate_block_for_consensus(&block)?;
        self.apply_block(block)
    }

    pub fn stake(&mut self, account: &str, amount: u64) -> Result<u64, String> {
        if account.trim().is_empty() {
            return Err("validator account must not be empty".to_string());
        }
        if amount == 0 {
            return Err("stake amount must be greater than zero".to_string());
        }
        let balance = self
            .balances
            .get(account)
            .copied()
            .ok_or_else(|| format!("unknown validator account: {account}"))?;
        let current_stake = self.stakes.get(account).copied().unwrap_or(0);
        let pending_cost = self
            .pending
            .iter()
            .filter(|tx| tx.sender == account)
            .try_fold(0u64, |reserved, tx| {
                reserved.checked_add(tx.amount)?.checked_add(tx.fee)
            })
            .unwrap_or(u64::MAX);
        let available = balance
            .saturating_sub(current_stake)
            .saturating_sub(pending_cost);
        if amount > available {
            return Err(format!(
                "insufficient unstaked balance: {account} has {available} available"
            ));
        }
        let updated_stake = current_stake
            .checked_add(amount)
            .ok_or_else(|| "validator stake overflows".to_string())?;
        self.stakes.insert(account.to_string(), updated_stake);
        Ok(updated_stake)
    }

    pub fn stake_of(&self, account: &str) -> u64 {
        self.stakes.get(account).copied().unwrap_or(0)
    }

    pub fn set_stake_amount(&mut self, account: &str, amount: u64) -> Result<(), String> {
        if account.trim().is_empty() {
            return Err("validator account must not be empty".to_string());
        }
        if amount > self.balance_of(account) {
            return Err(format!("stake exceeds balance for {account}"));
        }
        let mut candidate = self.clone();
        if amount == 0 {
            candidate.stakes.remove(account);
        } else {
            candidate.stakes.insert(account.to_string(), amount);
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn height(&self) -> u64 {
        u64::try_from(self.chain.len().saturating_sub(1)).unwrap_or(u64::MAX)
    }

    pub fn block_hash_at(&self, height: u64) -> Option<&str> {
        self.chain
            .get(usize::try_from(height).ok()?)
            .map(|block| block.hash.as_str())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.chain.is_empty() {
            return Err("ledger chain must contain a genesis block".to_string());
        }

        let genesis = &self.chain[0];
        let expected_genesis_hash =
            blake3::hash(format!("{}:{}:genesis", genesis.timestamp, genesis.prev_hash).as_bytes())
                .to_hex()
                .to_string();
        if genesis.validator != "GENESIS"
            || genesis.timestamp != GENESIS_TIMESTAMP
            || genesis.prev_hash != "GENESIS"
            || !genesis.stake_snapshot.is_empty()
            || !genesis.transactions.is_empty()
            || genesis.merkle_root != "0x0"
            || genesis.hash != expected_genesis_hash
        {
            return Err("invalid genesis block".to_string());
        }

        for (index, block) in self.chain.iter().enumerate() {
            let expected_index =
                u64::try_from(index).map_err(|_| "block index exceeds u64".to_string())?;
            if block.index != expected_index {
                return Err(format!("unexpected block index at position {index}"));
            }
            if index > 0 && block.prev_hash != self.chain[index - 1].hash {
                return Err(format!("broken previous-hash link at block {index}"));
            }
            if index > 0 {
                let expected_validator =
                    select_pos_validator(&block.prev_hash, block.index, &block.stake_snapshot)?;
                if block.validator != expected_validator {
                    return Err(format!("incorrect proposer for block {index}"));
                }
                let tx_hashes: Vec<_> = block.transactions.iter().map(Transaction::hash).collect();
                if compute_merkle_root(&tx_hashes) != block.merkle_root
                    || compute_pos_block_hash(block) != block.hash
                {
                    return Err(format!("invalid block commitment at index {index}"));
                }
            }
        }

        for (account, amount) in &self.stakes {
            if account.trim().is_empty() || *amount == 0 || self.balance_of(account) < *amount {
                return Err(format!("invalid stake for {account}"));
            }
        }

        let mut pending_costs = HashMap::<&str, u64>::new();
        let mut pending_income = HashMap::<&str, u64>::new();
        let mut nonces = HashSet::new();
        let mut transaction_ids = HashSet::new();
        for block in &self.chain {
            for tx in &block.transactions {
                if tx.nonce == 0
                    || tx.nonce > self.nonce
                    || !nonces.insert(tx.nonce)
                    || !transaction_ids.insert(tx.id.as_str())
                {
                    return Err(format!(
                        "invalid transaction identity in block {}",
                        block.index
                    ));
                }
                validate_transaction_integrity(tx)?;
            }
        }

        for tx in &self.pending {
            if tx.amount == 0
                || tx.sender.trim().is_empty()
                || tx.recipient.trim().is_empty()
                || tx.sender == tx.recipient
            {
                return Err(format!("invalid pending transaction {}", tx.id));
            }
            if tx.nonce == 0
                || tx.nonce > self.nonce
                || !nonces.insert(tx.nonce)
                || !transaction_ids.insert(tx.id.as_str())
            {
                return Err(format!("invalid or duplicate nonce in {}", tx.id));
            }
            validate_transaction_integrity(tx)?;
            if !self.balances.contains_key(&tx.sender) {
                return Err(format!("unknown sender in pending transaction {}", tx.id));
            }

            let cost = tx
                .amount
                .checked_add(tx.fee)
                .ok_or_else(|| format!("amount plus fee overflows in {}", tx.id))?;
            let reserved = pending_costs.entry(&tx.sender).or_default();
            *reserved = reserved
                .checked_add(cost)
                .ok_or_else(|| format!("pending costs overflow for {}", tx.sender))?;

            let incoming = pending_income.entry(&tx.recipient).or_default();
            *incoming = incoming
                .checked_add(tx.amount)
                .ok_or_else(|| format!("pending income overflows for {}", tx.recipient))?;
        }

        for (sender, cost) in pending_costs {
            if self
                .balance_of(sender)
                .saturating_sub(self.stake_of(sender))
                < cost
            {
                return Err(format!("pending transactions overspend {sender}"));
            }
        }
        for (recipient, amount) in pending_income {
            self.balance_of(recipient)
                .checked_add(amount)
                .ok_or_else(|| format!("pending transactions overflow {recipient}"))?;
        }

        Ok(())
    }

    pub fn transfer(
        &mut self,
        sender: &str,
        recipient: &str,
        amount: u64,
    ) -> Result<Transaction, String> {
        if sender.trim().is_empty() || recipient.trim().is_empty() {
            return Err("sender and recipient must not be empty".to_string());
        }
        if sender == recipient {
            return Err("sender and recipient must be different".to_string());
        }
        if amount == 0 {
            return Err("amount must be greater than zero".to_string());
        }

        let fee = 1;
        let total_cost = amount
            .checked_add(fee)
            .ok_or_else(|| "amount plus fee overflows the balance limit".to_string())?;
        let sender_balance = self.balances.get(sender).copied().unwrap_or(0);
        let pending_cost = self
            .pending
            .iter()
            .filter(|tx| tx.sender == sender)
            .try_fold(0u64, |reserved, tx| {
                reserved.checked_add(tx.amount)?.checked_add(tx.fee)
            })
            .unwrap_or(u64::MAX);
        let available_balance = sender_balance
            .saturating_sub(self.stake_of(sender))
            .saturating_sub(pending_cost);
        if available_balance < total_cost {
            return Err(format!(
                "insufficient funds: {} has {} available but needs {} including fees",
                sender, available_balance, total_cost
            ));
        }

        let recipient_balance = self.balances.get(recipient).copied().unwrap_or(0);
        let pending_income = self
            .pending
            .iter()
            .filter(|tx| tx.recipient == recipient)
            .try_fold(0u64, |total, tx| total.checked_add(tx.amount))
            .unwrap_or(u64::MAX);
        recipient_balance
            .checked_add(pending_income)
            .and_then(|balance| balance.checked_add(amount))
            .ok_or_else(|| "recipient balance would overflow".to_string())?;

        self.nonce = self
            .nonce
            .checked_add(1)
            .ok_or_else(|| "transaction nonce overflow".to_string())?;
        let timestamp = unix_now();
        let stealth_commitment =
            compute_stealth_commitment(sender, recipient, self.nonce, amount, timestamp);
        let nullifier = compute_nullifier(sender, self.nonce);
        let signature =
            compute_signature(sender, recipient, amount, self.nonce, &stealth_commitment);

        let tx = Transaction {
            id: format!("tx-{}", self.nonce),
            sender: sender.to_string(),
            recipient: recipient.to_string(),
            amount,
            fee,
            nonce: self.nonce,
            timestamp,
            stealth_commitment,
            nullifier,
            public_key: String::new(),
            signature,
        };

        self.pending.push(tx.clone());
        Ok(tx)
    }

    pub fn transfer_signed(
        &mut self,
        wallet: &Wallet,
        recipient: &str,
        amount: u64,
    ) -> Result<Transaction, String> {
        if Wallet::address_for_public_key(&wallet.public_key)? != wallet.address {
            return Err("wallet address does not match public key".to_string());
        }
        let mut candidate = self.clone();
        let mut transaction = candidate.transfer(&wallet.address, recipient, amount)?;
        transaction.public_key = wallet.public_key.clone();
        transaction.signature = wallet.sign(&transaction_signing_message(&transaction))?;
        let pending = candidate
            .pending
            .last_mut()
            .ok_or_else(|| "created transaction is missing from pending pool".to_string())?;
        *pending = transaction.clone();
        candidate.validate()?;
        *self = candidate;
        Ok(transaction)
    }

    pub fn mine_block(&mut self) -> Result<Block, String> {
        self.validate()?;
        let txs = self.pending.clone();
        let prev_hash = self
            .chain
            .last()
            .ok_or_else(|| "ledger chain must contain a genesis block".to_string())?
            .hash
            .clone();
        let index =
            u64::try_from(self.chain.len()).map_err(|_| "block index exceeds u64".to_string())?;
        let timestamp = unix_now();
        let mut stake_snapshot: Vec<_> = self
            .stakes
            .iter()
            .map(|(account, amount)| (account.clone(), *amount))
            .collect();
        stake_snapshot.sort_by(|left, right| left.0.cmp(&right.0));
        let validator = select_pos_validator(&prev_hash, index, &stake_snapshot)?;

        let mut tx_hashes = Vec::with_capacity(txs.len());
        for tx in &txs {
            tx_hashes.push(tx.hash());
        }

        let merkle_root = compute_merkle_root(&tx_hashes);
        let mut block = Block {
            index,
            timestamp,
            prev_hash,
            transactions: txs,
            merkle_root,
            hash: String::new(),
            validator,
            stake_snapshot,
        };
        block.hash = compute_pos_block_hash(&block);

        self.apply_block(block.clone())?;
        Ok(block)
    }

    pub fn validate_block(&self, block: &Block) -> Result<(), String> {
        self.validate()?;
        let expected_index =
            u64::try_from(self.chain.len()).map_err(|_| "block index exceeds u64".to_string())?;
        let previous = self
            .chain
            .last()
            .ok_or_else(|| "ledger chain must contain a genesis block".to_string())?;
        if block.index != expected_index {
            return Err(format!(
                "block index {} is not the next ledger index {expected_index}",
                block.index
            ));
        }
        if block.prev_hash != previous.hash {
            return Err("block does not extend the current ledger tip".to_string());
        }
        let mut expected_snapshot: Vec<_> = self
            .stakes
            .iter()
            .map(|(address, amount)| (address.clone(), *amount))
            .collect();
        expected_snapshot.sort_by(|left, right| left.0.cmp(&right.0));
        if block.stake_snapshot != expected_snapshot {
            return Err("block stake snapshot does not match the current ledger state".to_string());
        }
        let expected_validator =
            select_pos_validator(&block.prev_hash, block.index, &block.stake_snapshot)?;
        if block.validator != expected_validator {
            return Err("block proposer is invalid".to_string());
        }
        let tx_hashes: Vec<_> = block.transactions.iter().map(Transaction::hash).collect();
        if compute_merkle_root(&tx_hashes) != block.merkle_root
            || compute_pos_block_hash(block) != block.hash
        {
            return Err("block commitment is invalid".to_string());
        }
        Ok(())
    }

    pub fn apply_block(&mut self, block: Block) -> Result<(), String> {
        self.validate_block(&block)?;
        let mut candidate = self.clone();
        let mut next_balances = candidate.balances.clone();
        let mut chain_nonce = candidate
            .chain
            .iter()
            .flat_map(|existing| existing.transactions.iter().map(|tx| tx.nonce))
            .max()
            .unwrap_or(0);
        let mut next_nonce = candidate.nonce;
        let mut known_ids: HashSet<_> = candidate
            .chain
            .iter()
            .flat_map(|existing| existing.transactions.iter().map(|tx| tx.id.as_str()))
            .collect();

        for tx in &block.transactions {
            if tx.amount == 0
                || tx.sender.trim().is_empty()
                || tx.recipient.trim().is_empty()
                || tx.sender == tx.recipient
            {
                return Err(format!("invalid transaction {}", tx.id));
            }
            if tx.nonce <= chain_nonce || tx.id != format!("tx-{}", tx.nonce) {
                return Err(format!("invalid transaction nonce or id in {}", tx.id));
            }
            validate_transaction_integrity(tx)?;
            if let Some(pending) = candidate.pending.iter().find(|pending| pending.id == tx.id) {
                if pending.hash() != tx.hash() {
                    return Err(format!(
                        "block transaction {} conflicts with local pending transaction",
                        tx.id
                    ));
                }
            }
            if !known_ids.insert(tx.id.as_str()) {
                return Err(format!("duplicate transaction {}", tx.id));
            }
            let cost = tx
                .amount
                .checked_add(tx.fee)
                .ok_or_else(|| format!("amount plus fee overflows in {}", tx.id))?;
            let sender_balance = next_balances
                .get_mut(&tx.sender)
                .ok_or_else(|| format!("unknown sender in transaction {}", tx.id))?;
            if sender_balance.saturating_sub(candidate.stake_of(&tx.sender)) < cost {
                return Err(format!("transaction {} overspends its sender", tx.id));
            }
            *sender_balance = sender_balance
                .checked_sub(cost)
                .ok_or_else(|| format!("transaction {} overspends its sender", tx.id))?;
            let recipient_balance = next_balances.entry(tx.recipient.clone()).or_insert(0);
            *recipient_balance = recipient_balance
                .checked_add(tx.amount)
                .ok_or_else(|| format!("recipient balance overflows in {}", tx.id))?;
            chain_nonce = tx.nonce;
            next_nonce = next_nonce.max(tx.nonce);
        }

        let included_ids: HashSet<_> = block.transactions.iter().map(|tx| tx.id.clone()).collect();
        candidate.balances = next_balances;
        candidate.nonce = next_nonce;
        candidate.chain.push(block);
        let previous_pending = std::mem::take(&mut candidate.pending);
        for tx in previous_pending {
            if included_ids.contains(&tx.id) {
                continue;
            }
            candidate.pending.push(tx);
            if candidate.validate().is_err() {
                candidate.pending.pop();
            }
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn balance_of(&self, account: &str) -> u64 {
        *self.balances.get(account).unwrap_or(&0)
    }

    pub fn available_balance(&self, account: &str) -> u64 {
        self.balance_of(account)
            .saturating_sub(self.stake_of(account))
    }

    pub fn status(&self) -> String {
        let last_hash = self
            .chain
            .last()
            .map(|block| block.hash.as_str())
            .unwrap_or("invalid-empty-chain");
        let mut lines = vec![
            "Resonant v2 status".to_string(),
            format!("blocks: {}", self.chain.len()),
            format!("last_hash: {last_hash}"),
            format!("pending_tx: {}", self.pending.len()),
        ];

        for (account, balance) in self.accounts() {
            lines.push(format!("  - {}: {}", account, balance));
        }

        lines.join("\n")
    }

    pub fn accounts(&self) -> Vec<(String, u64)> {
        let mut accounts: Vec<_> = self
            .balances
            .iter()
            .map(|(account, balance)| (account.clone(), *balance))
            .collect();
        accounts.sort_by(|left, right| left.0.cmp(&right.0));
        accounts
    }
}

impl Default for Ledger {
    fn default() -> Self {
        Self::new()
    }
}

pub fn run_benchmark(tx_count: u64, batch_size: u64) -> BenchmarkResult {
    run_benchmark_with_ledger(Ledger::new(), tx_count, batch_size)
}

fn run_benchmark_with_ledger(
    mut ledger: Ledger,
    tx_count: u64,
    batch_size: u64,
) -> BenchmarkResult {
    let tx_target = tx_count.max(1);
    let batch_target = batch_size.max(1);

    let start = Instant::now();
    let mut generated = 0u64;
    let mut mined_transactions = 0u64;
    let mut blocks = 0u64;

    while generated < tx_target {
        let remaining = tx_target - generated;
        let current_batch = remaining.min(batch_target);

        for offset in 0..current_batch {
            let transaction_index = generated + offset;
            let sender = match transaction_index % 8 {
                0 => "alice",
                1 => "bob",
                2 => "charlie",
                3 => "dana",
                4 => "erin",
                5 => "frank",
                6 => "grace",
                _ => "henry",
            };

            let recipient = match (transaction_index % 8 + 3) % 8 {
                0 => "alice",
                1 => "bob",
                2 => "charlie",
                3 => "dana",
                4 => "erin",
                5 => "frank",
                6 => "grace",
                _ => "henry",
            };

            let _ = ledger.transfer(sender, recipient, 1 + (transaction_index % 50));
        }
        generated += current_batch;

        match ledger.mine_block() {
            Ok(block) => {
                mined_transactions += block.transactions.len() as u64;
                blocks += 1;
            }
            Err(_) => break,
        }
    }

    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
    let tx_per_second = if elapsed.is_zero() {
        0.0
    } else {
        mined_transactions as f64 / elapsed.as_secs_f64()
    };

    BenchmarkResult {
        tx_count: mined_transactions,
        elapsed_ms,
        tx_per_second,
        block_count: blocks,
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn genesis_stakes() -> HashMap<String, u64> {
    HashMap::from([("henry".to_string(), 1)])
}

fn select_pos_validator(
    previous_hash: &str,
    index: u64,
    stake_snapshot: &[(String, u64)],
) -> Result<String, String> {
    if stake_snapshot.is_empty() {
        return Err("no validators have staked".to_string());
    }

    let mut total_stake = 0u64;
    let mut previous_account: Option<&str> = None;
    for (account, amount) in stake_snapshot {
        if account.trim().is_empty() || *amount == 0 {
            return Err("invalid validator stake snapshot".to_string());
        }
        if previous_account.is_some_and(|previous| previous >= account.as_str()) {
            return Err("validator stake snapshot must be uniquely sorted".to_string());
        }
        previous_account = Some(account);
        total_stake = total_stake
            .checked_add(*amount)
            .ok_or_else(|| "total validator stake overflows".to_string())?;
    }

    let seed = serde_json::to_vec(&(previous_hash, index))
        .expect("PoS selection seed serialization is infallible");
    let digest = blake3::hash(&seed);
    let mut ticket_bytes = [0u8; 8];
    ticket_bytes.copy_from_slice(&digest.as_bytes()[..8]);
    let mut ticket = u64::from_le_bytes(ticket_bytes) % total_stake;

    for (account, amount) in stake_snapshot {
        if ticket < *amount {
            return Ok(account.clone());
        }
        ticket -= amount;
    }

    Err("could not select a validator".to_string())
}

fn compute_pos_block_hash(block: &Block) -> String {
    let tx_hashes: Vec<_> = block.transactions.iter().map(Transaction::hash).collect();
    let payload = serde_json::to_vec(&(
        block.index,
        block.timestamp,
        &block.prev_hash,
        &block.merkle_root,
        tx_hashes,
        &block.validator,
        &block.stake_snapshot,
    ))
    .expect("PoS block serialization is infallible");
    blake3::hash(&payload).to_hex().to_string()
}

fn compute_stealth_commitment(
    sender: &str,
    recipient: &str,
    nonce: u64,
    amount: u64,
    timestamp: i64,
) -> String {
    let payload = format!(
        "stealth:{}|{}|{}|{}|{}",
        sender, recipient, nonce, amount, timestamp
    );
    blake3::hash(payload.as_bytes()).to_hex().to_string()
}

fn compute_nullifier(sender: &str, nonce: u64) -> String {
    let payload = format!("nullifier:{}|{}", sender, nonce);
    blake3::hash(payload.as_bytes()).to_hex().to_string()
}

fn compute_signature(
    sender: &str,
    recipient: &str,
    amount: u64,
    nonce: u64,
    stealth_commitment: &str,
) -> String {
    let payload = format!(
        "sig:{}|{}|{}|{}|{}",
        sender, recipient, amount, nonce, stealth_commitment
    );
    blake3::hash(payload.as_bytes()).to_hex().to_string()
}

fn transaction_signing_message(tx: &Transaction) -> Vec<u8> {
    serde_json::to_vec(&(
        "resonant/transaction-signature/v1",
        &tx.id,
        &tx.sender,
        &tx.recipient,
        tx.amount,
        tx.fee,
        tx.nonce,
        tx.timestamp,
        &tx.stealth_commitment,
        &tx.nullifier,
        &tx.public_key,
    ))
    .expect("transaction signing message serialization is infallible")
}

pub fn validate_transaction_authorization(tx: &Transaction) -> Result<(), String> {
    if tx.public_key.is_empty()
        || Wallet::address_for_public_key(&tx.public_key)
            .map_err(|error| format!("invalid transaction public key: {error}"))?
            != tx.sender
        || !Wallet::verify(
            &tx.public_key,
            &transaction_signing_message(tx),
            &tx.signature,
        )
    {
        return Err(format!("invalid transaction authorization in {}", tx.id));
    }
    Ok(())
}

fn validate_transaction_integrity(tx: &Transaction) -> Result<(), String> {
    if tx.id != format!("tx-{}", tx.nonce)
        || tx.fee != 1
        || tx.stealth_commitment
            != compute_stealth_commitment(
                &tx.sender,
                &tx.recipient,
                tx.nonce,
                tx.amount,
                tx.timestamp,
            )
        || tx.nullifier != compute_nullifier(&tx.sender, tx.nonce)
    {
        return Err(format!("invalid transaction commitment in {}", tx.id));
    }
    if tx.public_key.is_empty() {
        if tx.signature
            != compute_signature(
                &tx.sender,
                &tx.recipient,
                tx.amount,
                tx.nonce,
                &tx.stealth_commitment,
            )
        {
            return Err(format!("invalid transaction commitment in {}", tx.id));
        }
    } else {
        validate_transaction_authorization(tx)?;
    }
    Ok(())
}

fn compute_merkle_root(leaves: &[String]) -> String {
    if leaves.is_empty() {
        return "0x0".to_string();
    }

    let mut current = leaves.to_vec();
    while current.len() > 1 {
        let mut next = Vec::with_capacity(current.len().div_ceil(2));
        for chunk in current.chunks(2) {
            let left = &chunk[0];
            let right = if chunk.len() == 2 {
                &chunk[1]
            } else {
                &chunk[0]
            };
            let merged = format!("{}{}", left, right);
            next.push(blake3::hash(merged.as_bytes()).to_hex().to_string());
        }
        current = next;
    }

    current[0].clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_and_mine_work() {
        let mut ledger = Ledger::new();
        let alice_initial = ledger.balance_of("alice");
        let bob_initial = ledger.balance_of("bob");
        let tx = ledger.transfer("alice", "bob", 250).unwrap();
        assert_eq!(tx.sender, "alice");
        assert_eq!(tx.recipient, "bob");
        assert_eq!(ledger.balance_of("alice"), alice_initial);
        assert_eq!(ledger.balance_of("bob"), bob_initial);

        let block = ledger.mine_block().unwrap();
        assert_eq!(block.index, 1);
        assert!(!block.hash.is_empty());
        assert!(ledger.pending.is_empty());
        assert_eq!(ledger.balance_of("alice"), alice_initial - 251);
        assert_eq!(ledger.balance_of("bob"), bob_initial + 250);
    }

    #[test]
    fn applying_a_valid_remote_block_updates_state() {
        let mut proposer = Ledger::new();
        proposer.transfer("alice", "bob", 250).unwrap();
        let block = proposer.mine_block().unwrap();
        let mut receiver = Ledger::new();

        receiver.apply_block(block).unwrap();

        assert_eq!(receiver.height(), 1);
        assert_eq!(receiver.balance_of("alice"), 1_000_000_000 - 251);
        assert_eq!(receiver.balance_of("bob"), 1_000_000_250);
        assert!(receiver.pending.is_empty());
        assert!(receiver.validate().is_ok());
    }

    #[test]
    fn invalid_remote_block_does_not_mutate_ledger() {
        let mut proposer = Ledger::new();
        proposer.transfer("alice", "bob", 250).unwrap();
        let mut block = proposer.mine_block().unwrap();
        block.transactions[0].amount += 1;
        let mut receiver = Ledger::new();
        let initial_alice = receiver.balance_of("alice");
        let initial_bob = receiver.balance_of("bob");

        assert!(receiver.apply_block(block).is_err());
        assert_eq!(receiver.height(), 0);
        assert_eq!(receiver.balance_of("alice"), initial_alice);
        assert_eq!(receiver.balance_of("bob"), initial_bob);
        assert!(receiver.validate().is_ok());
    }

    #[test]
    fn stake_updates_preserve_historical_block_snapshots() {
        let mut ledger = Ledger::new();
        ledger.stake("alice", 100).unwrap();
        let first = ledger.mine_block().unwrap();

        ledger.set_stake_amount("alice", 50).unwrap();

        assert_eq!(ledger.stake_of("alice"), 50);
        assert_eq!(
            first.stake_snapshot,
            vec![("alice".to_string(), 100), ("henry".to_string(), 1)]
        );
        assert!(ledger.validate().is_ok());
        assert_eq!(
            ledger.mine_block().unwrap().stake_snapshot,
            vec![("alice".to_string(), 50), ("henry".to_string(), 1)]
        );
    }

    #[test]
    fn failed_stake_update_does_not_mutate_ledger() {
        let mut ledger = Ledger::new();
        let original = ledger.stake_of("alice");

        assert!(ledger.set_stake_amount("alice", u64::MAX).is_err());
        assert_eq!(ledger.stake_of("alice"), original);
    }

    #[test]
    fn invalid_transfers_do_not_consume_nonce_or_enqueue_transactions() {
        let mut ledger = Ledger::new();

        assert!(ledger.transfer("alice", "alice", 1).is_err());
        assert!(ledger.transfer("alice", "bob", 0).is_err());
        assert!(ledger.transfer("unknown", "bob", 1).is_err());
        assert!(ledger.pending.is_empty());

        let tx = ledger.transfer("alice", "bob", 1).unwrap();
        assert_eq!(tx.id, "tx-1");
    }

    #[test]
    fn transfer_rejects_recipient_balance_overflow() {
        let mut ledger = Ledger::new();
        ledger.balances.insert("full".to_string(), u64::MAX);

        assert!(ledger.transfer("alice", "full", 1).is_err());
        assert!(ledger.pending.is_empty());
        assert_eq!(ledger.balance_of("alice"), 1_000_000_000);
    }

    #[test]
    fn creating_wallet_does_not_reset_an_existing_balance() {
        let mut ledger = Ledger::new();
        let first = ledger.create_wallet("repeat-label");
        ledger.balances.insert(first.address.clone(), 25);

        let second = ledger.create_wallet("repeat-label");

        assert_ne!(first.address, second.address);
        assert_eq!(ledger.balance_of(&first.address), 25);
        assert_eq!(ledger.balance_of(&second.address), 10_000);
    }

    #[test]
    fn serialized_ledger_preserves_pending_transactions() {
        let mut ledger = Ledger::new();
        ledger.transfer("alice", "bob", 25).unwrap();

        let serialized = serde_json::to_vec(&ledger).unwrap();
        let mut restored: Ledger = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(restored.pending.len(), 1);
        assert_eq!(restored.balance_of("alice"), 1_000_000_000);

        let block = restored.mine_block().unwrap();
        assert_eq!(block.transactions.len(), 1);
        assert_eq!(restored.balance_of("alice"), 1_000_000_000 - 26);
        assert_eq!(restored.balance_of("bob"), 1_000_000_025);
    }

    #[test]
    fn invalid_pending_state_is_rejected_without_partial_mining() {
        let mut ledger = Ledger::new();
        let mut tx = ledger.transfer("alice", "bob", 25).unwrap();
        tx.sender = "missing-account".to_string();
        ledger.pending[0] = tx;

        assert!(ledger.validate().is_err());
        assert!(ledger.mine_block().is_err());
        assert_eq!(ledger.pending.len(), 1);
        assert_eq!(ledger.balance_of("alice"), 1_000_000_000);
        assert_eq!(ledger.balance_of("bob"), 1_000_000_000);
    }

    #[test]
    fn benchmark_respects_requested_batch_count() {
        let result = run_benchmark(7, 3);

        assert_eq!(result.tx_count, 7);
        assert_eq!(result.block_count, 3);
        assert!(result.elapsed_ms.is_finite());
        assert!(result.tx_per_second.is_finite());
    }

    #[test]
    fn benchmark_counts_only_mined_transactions() {
        let mut ledger = Ledger::new();
        for balance in ledger.balances.values_mut() {
            *balance = 0;
        }
        ledger.balances.insert("henry".to_string(), 1);

        let result = run_benchmark_with_ledger(ledger, 5, 2);

        assert_eq!(result.tx_count, 0);
        assert_eq!(result.block_count, 3);
        assert_eq!(result.tx_per_second, 0.0);
    }

    #[test]
    fn pending_transfers_reserve_amount_and_fee() {
        let mut ledger = Ledger::new();
        let initial_balance = ledger.balance_of("alice");
        let recipient_initial_balance = ledger.balance_of("bob");

        ledger
            .transfer("alice", "bob", initial_balance - 1)
            .unwrap();
        assert!(ledger.transfer("alice", "charlie", 1).is_err());

        let block = ledger.mine_block().unwrap();
        assert_eq!(ledger.balance_of("alice"), 0);
        assert_eq!(
            ledger.balance_of("bob"),
            recipient_initial_balance + initial_balance - 1
        );
        assert_eq!(block.transactions.len(), 1);
    }

    #[test]
    fn transfer_rejects_amount_fee_overflow() {
        let mut ledger = Ledger::new();
        assert!(ledger.transfer("alice", "bob", u64::MAX).is_err());
    }

    #[test]
    fn new_ledger_has_genesis_and_seed_balances() {
        let ledger = Ledger::new();
        let status = ledger.status();

        assert!(status.contains("blocks: 1"));
        for account in [
            "alice", "bob", "charlie", "dana", "erin", "frank", "grace", "henry",
        ] {
            assert_eq!(ledger.balance_of(account), 1_000_000_000);
        }
    }

    #[test]
    fn default_ledger_contains_initial_accounts() {
        let ledger = Ledger::default();
        assert_eq!(ledger.balance_of("alice"), 1_000_000_000);
        assert_eq!(ledger.balance_of("henry"), 1_000_000_000);
        assert!(ledger.pending.is_empty());
    }

    #[test]
    fn mining_empty_block_increments_index() {
        let mut ledger = Ledger::new();
        let genesis_hash = ledger.chain[0].hash.clone();
        let block = ledger.mine_block().unwrap();

        assert_eq!(block.index, 1);
        assert!(block.transactions.is_empty());
        assert_eq!(block.prev_hash, genesis_hash);
    }

    #[test]
    fn mined_blocks_link_to_previous_hash() {
        let mut ledger = Ledger::new();
        let first = ledger.mine_block().unwrap();
        let second = ledger.mine_block().unwrap();

        assert_eq!(second.index, first.index + 1);
        assert_eq!(second.prev_hash, first.hash);
    }

    #[test]
    fn transfer_fee_is_removed_from_total_supply() {
        let mut ledger = Ledger::new();
        let total_before: u64 = ledger.accounts().iter().map(|(_, balance)| balance).sum();
        ledger.transfer("alice", "bob", 50).unwrap();
        ledger.mine_block().unwrap();
        let total_after: u64 = ledger.accounts().iter().map(|(_, balance)| balance).sum();

        assert_eq!(total_after, total_before - 1);
    }

    #[test]
    fn transfer_equal_to_balance_is_rejected_due_to_fee() {
        let mut ledger = Ledger::new();
        let balance = ledger.balance_of("alice");

        assert!(ledger.transfer("alice", "bob", balance).is_err());
        assert!(ledger.pending.is_empty());
    }

    #[test]
    fn transfer_rejects_empty_sender() {
        let mut ledger = Ledger::new();
        assert!(ledger.transfer("", "bob", 1).is_err());
    }

    #[test]
    fn transfer_rejects_empty_recipient() {
        let mut ledger = Ledger::new();
        assert!(ledger.transfer("alice", "", 1).is_err());
    }

    #[test]
    fn transfer_rejects_whitespace_account_identifiers() {
        let mut ledger = Ledger::new();
        assert!(ledger.transfer("   ", "bob", 1).is_err());
        assert!(ledger.transfer("alice", "\t", 1).is_err());
    }

    #[test]
    fn validation_rejects_empty_chain_without_panicking_in_status() {
        let mut ledger = Ledger::new();
        ledger.chain.clear();

        assert!(ledger.validate().is_err());
        assert!(ledger.status().contains("invalid-empty-chain"));
    }

    #[test]
    fn validation_rejects_incorrect_block_index() {
        let mut ledger = Ledger::new();
        ledger.mine_block().unwrap();
        ledger.chain[1].index = 7;

        assert!(ledger.validate().is_err());
    }

    #[test]
    fn validation_rejects_broken_previous_hash_link() {
        let mut ledger = Ledger::new();
        ledger.mine_block().unwrap();
        ledger.chain[1].prev_hash = "broken".to_string();

        assert!(ledger.validate().is_err());
    }

    #[test]
    fn validation_rejects_duplicate_pending_nonce() {
        let mut ledger = Ledger::new();
        ledger.transfer("alice", "bob", 1).unwrap();
        ledger.transfer("alice", "charlie", 1).unwrap();
        ledger.pending[1].nonce = ledger.pending[0].nonce;

        assert!(ledger.validate().is_err());
    }

    #[test]
    fn validation_rejects_duplicate_transaction_id() {
        let mut ledger = Ledger::new();
        ledger.transfer("alice", "bob", 1).unwrap();
        ledger.transfer("alice", "charlie", 1).unwrap();
        ledger.pending[1].id = ledger.pending[0].id.clone();

        assert!(ledger.validate().is_err());
    }

    #[test]
    fn validation_rejects_pending_fee_overflow() {
        let mut ledger = Ledger::new();
        ledger.transfer("alice", "bob", 1).unwrap();
        ledger.pending[0].fee = u64::MAX;

        assert!(ledger.validate().is_err());
    }

    #[test]
    fn transaction_hash_distinguishes_delimiter_containing_fields() {
        let mut ledger = Ledger::new();
        let tx = ledger.transfer("alice", "bob", 1).unwrap();
        let mut left = tx.clone();
        let mut right = tx;
        left.id = "a|b".to_string();
        left.sender = "c".to_string();
        right.id = "a".to_string();
        right.sender = "b|c".to_string();

        assert_ne!(left.hash(), right.hash());
    }

    #[test]
    fn merkle_root_handles_empty_single_and_odd_leaf_inputs() {
        assert_eq!(compute_merkle_root(&[]), "0x0");
        assert_eq!(compute_merkle_root(&["leaf".to_string()]), "leaf");

        let leaves = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let hash = |value: &str| blake3::hash(value.as_bytes()).to_hex().to_string();
        let left = hash("ab");
        let right = hash("cc");
        assert_eq!(
            compute_merkle_root(&leaves),
            hash(&format!("{left}{right}"))
        );
    }
}

#[cfg(test)]
mod pos_tests {
    use super::*;

    #[test]
    fn genesis_has_bootstrap_validator_stake() {
        let ledger = Ledger::new();
        assert_eq!(ledger.stake_of("henry"), 1);
    }

    #[test]
    fn genesis_anchor_is_fixed_and_validated() {
        let mut ledger = Ledger::new();
        assert_eq!(ledger.chain[0].validator, "GENESIS");
        assert_eq!(ledger.chain[0].prev_hash, "GENESIS");
        assert!(ledger.chain[0].stake_snapshot.is_empty());
        assert!(ledger.validate().is_ok());

        ledger.chain[0].prev_hash = "tampered".to_string();
        assert!(ledger.validate().is_err());
    }

    #[test]
    fn unknown_account_has_no_stake() {
        assert_eq!(Ledger::new().stake_of("unknown"), 0);
    }

    #[test]
    fn staking_increases_validator_stake() {
        let mut ledger = Ledger::new();
        assert_eq!(ledger.stake("alice", 100).unwrap(), 100);
        assert_eq!(ledger.stake_of("alice"), 100);
    }

    #[test]
    fn repeated_staking_accumulates() {
        let mut ledger = Ledger::new();
        ledger.stake("alice", 100).unwrap();
        assert_eq!(ledger.stake("alice", 250).unwrap(), 350);
    }

    #[test]
    fn staking_preserves_gross_balance() {
        let mut ledger = Ledger::new();
        let balance = ledger.balance_of("alice");
        ledger.stake("alice", 100).unwrap();
        assert_eq!(ledger.balance_of("alice"), balance);
    }

    #[test]
    fn available_balance_excludes_stake() {
        let mut ledger = Ledger::new();
        ledger.stake("alice", 1_000).unwrap();
        assert_eq!(ledger.available_balance("alice"), 999_999_000);
    }

    #[test]
    fn transfer_cannot_spend_staked_balance() {
        let mut ledger = Ledger::new();
        ledger.stake("alice", 1_000).unwrap();
        let available = ledger.available_balance("alice");

        assert!(ledger.transfer("alice", "bob", available).is_err());
        assert!(ledger.transfer("alice", "bob", available - 1).is_ok());
    }

    #[test]
    fn staking_rejects_zero_amount() {
        assert!(Ledger::new().stake("alice", 0).is_err());
    }

    #[test]
    fn staking_rejects_unknown_account() {
        assert!(Ledger::new().stake("unknown", 1).is_err());
    }

    #[test]
    fn staking_rejects_empty_account() {
        assert!(Ledger::new().stake("", 1).is_err());
    }

    #[test]
    fn staking_cannot_exceed_unstaked_balance() {
        let mut ledger = Ledger::new();
        assert!(ledger.stake("alice", 1_000_000_001).is_err());
        assert_eq!(ledger.stake_of("alice"), 0);
    }

    #[test]
    fn staking_respects_pending_transfer_reservations() {
        let mut ledger = Ledger::new();
        ledger.transfer("alice", "bob", 100).unwrap();
        let available = ledger.balance_of("alice") - 101;

        assert_eq!(ledger.stake("alice", available).unwrap(), available);
        assert!(ledger.validate().is_ok());
    }

    #[test]
    fn staking_all_balance_locks_all_transfer_availability() {
        let mut ledger = Ledger::new();
        ledger.stake("alice", 1_000_000_000).unwrap();

        assert_eq!(ledger.available_balance("alice"), 0);
        assert!(ledger.transfer("alice", "bob", 1).is_err());
    }

    #[test]
    fn proposer_selection_rejects_empty_validator_set() {
        assert!(select_pos_validator("seed", 1, &[]).is_err());
    }

    #[test]
    fn proposer_selection_rejects_zero_stake() {
        let validators = vec![("alice".to_string(), 0)];
        assert!(select_pos_validator("seed", 1, &validators).is_err());
    }

    #[test]
    fn proposer_selection_rejects_empty_validator_name() {
        let validators = vec![(" ".to_string(), 1)];
        assert!(select_pos_validator("seed", 1, &validators).is_err());
    }

    #[test]
    fn proposer_selection_rejects_unsorted_validators() {
        let validators = vec![("bob".to_string(), 1), ("alice".to_string(), 1)];
        assert!(select_pos_validator("seed", 1, &validators).is_err());
    }

    #[test]
    fn proposer_selection_rejects_duplicate_validators() {
        let validators = vec![("alice".to_string(), 1), ("alice".to_string(), 2)];
        assert!(select_pos_validator("seed", 1, &validators).is_err());
    }

    #[test]
    fn proposer_selection_rejects_total_stake_overflow() {
        let validators = vec![("alice".to_string(), u64::MAX), ("bob".to_string(), 1)];
        assert!(select_pos_validator("seed", 1, &validators).is_err());
    }

    #[test]
    fn proposer_selection_is_deterministic() {
        let validators = vec![("alice".to_string(), 3), ("bob".to_string(), 7)];
        let first = select_pos_validator("previous-hash", 42, &validators).unwrap();
        let second = select_pos_validator("previous-hash", 42, &validators).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn weighted_selection_tracks_stake_proportion() {
        let validators = vec![("alice".to_string(), 1), ("bob".to_string(), 9)];
        let alice_wins = (0..10_000)
            .filter(|index| select_pos_validator("seed", *index, &validators).unwrap() == "alice")
            .count();

        assert!(
            (800..1_200).contains(&alice_wins),
            "alice selected {alice_wins} times"
        );
    }

    #[test]
    fn sole_staker_is_selected_as_proposer() {
        let mut ledger = Ledger::new();
        ledger.stakes.clear();
        ledger.stake("alice", 100).unwrap();

        assert_eq!(ledger.mine_block().unwrap().validator, "alice");
    }

    #[test]
    fn block_stake_snapshot_is_sorted() {
        let mut ledger = Ledger::new();
        ledger.stake("alice", 10).unwrap();
        let snapshot = ledger.mine_block().unwrap().stake_snapshot;

        assert_eq!(snapshot[0].0, "alice");
        assert_eq!(snapshot[1].0, "henry");
    }

    #[test]
    fn mining_without_any_validator_fails_without_mutation() {
        let mut ledger = Ledger::new();
        ledger.stakes.clear();
        let initial_chain_len = ledger.chain.len();

        assert!(ledger.mine_block().is_err());
        assert_eq!(ledger.chain.len(), initial_chain_len);
    }

    #[test]
    fn validation_rejects_tampered_block_proposer() {
        let mut ledger = Ledger::new();
        ledger.mine_block().unwrap();
        ledger.chain[1].validator = "alice".to_string();

        assert!(ledger.validate().is_err());
    }

    #[test]
    fn validation_rejects_tampered_block_hash() {
        let mut ledger = Ledger::new();
        ledger.mine_block().unwrap();
        ledger.chain[1].hash.push('0');

        assert!(ledger.validate().is_err());
    }

    #[test]
    fn validation_rejects_tampered_merkle_root() {
        let mut ledger = Ledger::new();
        ledger.transfer("alice", "bob", 10).unwrap();
        ledger.mine_block().unwrap();
        ledger.chain[1].merkle_root = "tampered".to_string();

        assert!(ledger.validate().is_err());
    }

    #[test]
    fn stake_state_survives_serialization() {
        let mut ledger = Ledger::new();
        ledger.stake("alice", 321).unwrap();
        let serialized = serde_json::to_vec(&ledger).unwrap();
        let restored: Ledger = serde_json::from_slice(&serialized).unwrap();

        assert_eq!(restored.stake_of("alice"), 321);
        assert_eq!(restored.available_balance("alice"), 999_999_679);
        assert!(restored.validate().is_ok());
    }

    #[test]
    fn later_block_snapshot_reflects_new_stake() {
        let mut ledger = Ledger::new();
        let first = ledger.mine_block().unwrap();
        ledger.stake("alice", 50).unwrap();
        let second = ledger.mine_block().unwrap();

        assert_eq!(first.stake_snapshot, vec![("henry".to_string(), 1)]);
        assert_eq!(
            second.stake_snapshot,
            vec![("alice".to_string(), 50), ("henry".to_string(), 1)]
        );
        assert!(ledger.validate().is_ok());
    }
}
