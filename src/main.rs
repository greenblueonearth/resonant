use crate::wallet::Wallet;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

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
    pub signature: String,
}

impl Transaction {
    pub fn hash(&self) -> String {
        let payload = format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.id,
            self.sender,
            self.recipient,
            self.amount,
            self.fee,
            self.nonce,
            self.timestamp,
            self.stealth_commitment,
            self.nullifier,
            self.signature,
        );

        format!("{:x}", blake3::hash(payload.as_bytes()))
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
}

impl Block {
    pub fn genesis() -> Self {
        let timestamp = unix_now();
        let prev_hash = "GENESIS".to_string();
        let hash = format!(
            "{:x}",
            blake3::hash(format!("{}:{}:genesis", timestamp, prev_hash).as_bytes())
        );

        Self {
            index: 0,
            timestamp,
            prev_hash,
            transactions: Vec::new(),
            merkle_root: "0x0".to_string(),
            hash,
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

#[derive(Clone, Debug)]
pub struct Ledger {
    balances: HashMap<String, u64>,
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
            pending: Vec::new(),
            chain: vec![Block::genesis()],
            nonce: 0,
        }
    }

    pub fn create_wallet(&mut self, label: &str) -> Wallet {
        let wallet = Wallet::new(label);
        self.balances.insert(wallet.address.clone(), 10_000u64);
        wallet
    }

    pub fn transfer(&mut self, sender: &str, recipient: &str, amount: u64) -> Result<Transaction, String> {
        if sender == recipient {
            return Err("sender and recipient must be different".to_string());
        }
        if amount == 0 {
            return Err("amount must be greater than zero".to_string());
        }

        let sender_balance = self.balances.get(sender).copied().unwrap_or(0);
        if sender_balance < amount {
            return Err(format!(
                "insufficient funds: {} has {} but tries to send {}",
                sender, sender_balance, amount
            ));
        }

        self.nonce += 1;
        let timestamp = unix_now();
        let fee = 1;
        let stealth_commitment = compute_stealth_commitment(sender, recipient, self.nonce, amount, timestamp);
        let nullifier = compute_nullifier(sender, self.nonce);
        let signature = compute_signature(sender, recipient, amount, self.nonce, &stealth_commitment);

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
            signature,
        };

        self.pending.push(tx.clone());
        Ok(tx)
    }

    pub fn mine_block(&mut self) -> Block {
        let mut txs = std::mem::take(&mut self.pending);
        let prev_hash = self.chain.last().unwrap().hash.clone();
        let timestamp = unix_now();

        let mut tx_hashes = Vec::with_capacity(txs.len());
        for tx in &txs {
            tx_hashes.push(tx.hash());
            let sender_balance = self.balances.get_mut(&tx.sender).unwrap();
            *sender_balance = sender_balance.saturating_sub(tx.amount + tx.fee);
            let recipient_balance = self.balances.entry(tx.recipient.clone()).or_insert(0);
            *recipient_balance = recipient_balance.saturating_add(tx.amount);
        }

        let merkle_root = compute_merkle_root(&tx_hashes);
        let block_hash_input = format!(
            "{}|{}|{}|{}|{}",
            self.chain.len(),
            prev_hash,
            timestamp,
            merkle_root,
            tx_hashes.join("|")
        );
        let hash = format!("{:x}", blake3::hash(block_hash_input.as_bytes()));

        let block = Block {
            index: self.chain.len() as u64,
            timestamp,
            prev_hash,
            transactions: txs,
            merkle_root,
            hash,
        };

        self.chain.push(block.clone());
        block
    }

    pub fn balance_of(&self, account: &str) -> u64 {
        *self.balances.get(account).unwrap_or(&0)
    }

    pub fn status(&self) -> String {
        let last = self.chain.last().unwrap();
        let mut lines = vec![
            "Resonant v2 status".to_string(),
            format!("blocks: {}", self.chain.len()),
            format!("last_hash: {}", last.hash),
            format!("pending_tx: {}", self.pending.len()),
        ];

        let mut accounts: Vec<_> = self.balances.keys().cloned().collect();
        accounts.sort();

        for account in accounts {
            lines.push(format!("  - {}: {}", account, self.balances.get(&account).unwrap()));
        }

        lines.join("\n")
    }
}

pub fn run_benchmark(tx_count: u64, batch_size: u64) -> BenchmarkResult {
    let mut ledger = Ledger::new();
    let tx_target = tx_count.max(1);
    let batch_target = batch_size.max(1);

    let start = std::time::Instant::now();
    let mut generated = 0u64;
    let mut blocks = 0u64;

    while generated < tx_target {
        let remaining = tx_target - generated;
        let current_batch = remaining.min(batch_target);

        for i in 0..current_batch {
            let sender = match (generated + i) % 8 {
                0 => "alice",
                1 => "bob",
                2 => "charlie",
                3 => "dana",
                4 => "erin",
                5 => "frank",
                6 => "grace",
                _ => "henry",
            };

            let recipient = match ((generated + i + 3) % 8) {
                0 => "alice",
                1 => "bob",
                2 => "charlie",
                3 => "dana",
                4 => "erin",
                5 => "frank",
                6 => "grace",
                _ => "henry",
            };

            let _ = ledger.transfer(sender, recipient, 1 + ((generated + i) % 50));
            generated += 1;
        }

        let _ = ledger.mine_block();
        blocks += 1;
    }

    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_secs_f64() * 1000.0;
    let tx_per_second = (tx_target as f64 / elapsed.as_secs_f64()).max(0.0);

    BenchmarkResult {
        tx_count: tx_target,
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

fn compute_stealth_commitment(sender: &str, recipient: &str, nonce: u64, amount: u64, timestamp: i64) -> String {
    let payload = format!("stealth:{}|{}|{}|{}|{}", sender, recipient, nonce, amount, timestamp);
    format!("{:x}", blake3::hash(payload.as_bytes()))
}

fn compute_nullifier(sender: &str, nonce: u64) -> String {
    let payload = format!("nullifier:{}|{}", sender, nonce);
    format!("{:x}", blake3::hash(payload.as_bytes()))
}

fn compute_signature(sender: &str, recipient: &str, amount: u64, nonce: u64, stealth_commitment: &str) -> String {
    let payload = format!("sig:{}|{}|{}|{}|{}", sender, recipient, amount, nonce, stealth_commitment);
    format!("{:x}", blake3::hash(payload.as_bytes()))
}

fn compute_merkle_root(leaves: &[String]) -> String {
    if leaves.is_empty() {
        return "0x0".to_string();
    }

    let mut current = leaves.to_vec();
    while current.len() > 1 {
        let mut next = Vec::with_capacity((current.len() + 1) / 2);
        for chunk in current.chunks(2) {
            let left = &chunk[0];
            let right = if chunk.len() == 2 { &chunk[1] } else { &chunk[0] };
            let merged = format!("{}{}", left, right);
            next.push(format!("{:x}", blake3::hash(merged.as_bytes())));
        }
        current = next;
    }

    current[0].clone()
}
