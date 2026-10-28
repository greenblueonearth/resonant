use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Transaction {
    id: String,
    sender: String,
    recipient: String,
    amount: u64,
    fee: u64,
    nonce: u64,
    timestamp: i64,
    stealth_commitment: String,
    nullifier: String,
    signature: String,
}

impl Transaction {
    fn hash(&self) -> String {
        let mut input = String::new();
        input.push_str(&self.id);
        input.push('|');
        input.push_str(&self.sender);
        input.push('|');
        input.push_str(&self.recipient);
        input.push('|');
        input.push_str(&self.amount.to_string());
        input.push('|');
        input.push_str(&self.fee.to_string());
        input.push('|');
        input.push_str(&self.nonce.to_string());
        input.push('|');
        input.push_str(&self.timestamp.to_string());
        input.push('|');
        input.push_str(&self.stealth_commitment);
        input.push('|');
        input.push_str(&self.nullifier);
        input.push('|');
        input.push_str(&self.signature);

        let digest = blake3::hash(input.as_bytes());
        format!("{:x}", digest)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Block {
    index: u64,
    timestamp: i64,
    prev_hash: String,
    transactions: Vec<Transaction>,
    merkle_root: String,
    hash: String,
}

impl Block {
    fn genesis() -> Self {
        let timestamp = unix_now();
        let prev_hash = "GENESIS".to_string();
        let hash = blake3::hash(format!("{}:{}:genesis", timestamp, prev_hash).as_bytes());
        Self {
            index: 0,
            timestamp,
            prev_hash: prev_hash.clone(),
            transactions: Vec::new(),
            merkle_root: "0x0".to_string(),
            hash: format!("{:x}", hash),
        }
    }
}

#[derive(Debug, Clone)]
struct Blockchain {
    chain: Vec<Block>,
    pending: Vec<Transaction>,
    balances: HashMap<String, u64>,
    nonce: u64,
}

impl Blockchain {
    fn new() -> Self {
        let mut balances = HashMap::new();
        balances.insert("alice".to_string(), 1_000_000);
        balances.insert("bob".to_string(), 750_000);
        balances.insert("charlie".to_string(), 500_000);

        Self {
            chain: vec![Block::genesis()],
            pending: Vec::new(),
            balances,
            nonce: 0,
        }
    }

    fn transfer(&mut self, sender: &str, recipient: &str, amount: u64) -> Result<Transaction, String> {
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

    fn mine_block(&mut self) -> Block {
        let mut txs = std::mem::take(&mut self.pending);
        let prev_hash = self.chain.last().unwrap().hash.clone();
        let timestamp = unix_now();

        let mut tx_hashes: Vec<String> = Vec::new();
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

    fn pending_transactions(&self) -> &[Transaction] {
        &self.pending
    }

    fn balance_of(&self, account: &str) -> u64 {
        *self.balances.get(account).unwrap_or(&0)
    }

    fn status(&self) -> String {
        let last = self.chain.last().unwrap();
        let mut output = String::new();
        output.push_str("Resonant blockchain status\n");
        output.push_str(&format!("blocks: {}\n", self.chain.len()));
        output.push_str(&format!("last_hash: {}\n", last.hash));
        output.push_str(&format!("pending_tx: {}\n", self.pending.len()));
        output.push_str("balances:\n");

        let mut accounts: Vec<String> = self.balances.keys().cloned().collect();
        accounts.sort();
        for account in accounts {
            output.push_str(&format!("  - {}: {}\n", account, self.balances.get(&account).unwrap()));
        }

        output
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
        let mut next = Vec::new();
        for chunk in current.chunks(2) {
            let left = &chunk[0];
            let right = if chunk.len() == 2 {
                &chunk[1]
            } else {
                &chunk[0]
            };
            let merged = format!("{}{}", left, right);
            next.push(format!("{:x}", blake3::hash(merged.as_bytes())));
        }
        current = next;
    }

    current[0].clone()
}

fn print_json<T: Serialize>(value: &T) {
    println!("{}", serde_json::to_string_pretty(value).unwrap());
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut chain = Blockchain::new();

    if args.is_empty() {
        println!("Resonant CLI\n\nCommands:\n  status\n  balance <account>\n  transfer <sender> <recipient> <amount>\n  mine\n  pending\n");
        return;
    }

    match args[0].as_str() {
        "status" => println!("{}", chain.status()),
        "balance" => {
            if args.len() < 2 {
                eprintln!("usage: balance <account>");
                std::process::exit(1);
            }
            println!("{}", chain.balance_of(&args[1]));
        }
        "transfer" => {
            if args.len() < 4 {
                eprintln!("usage: transfer <sender> <recipient> <amount>");
                std::process::exit(1);
            }

            let amount = args[3].parse::<u64>().unwrap_or_else(|_| {
                eprintln!("amount must be a number");
                std::process::exit(1);
            });

            match chain.transfer(&args[1], &args[2], amount) {
                Ok(tx) => print_json(&tx),
                Err(err) => {
                    eprintln!("{}", err);
                    std::process::exit(1);
                }
            }
        }
        "mine" => {
            let block = chain.mine_block();
            print_json(&block);
        }
        "pending" => {
            print_json(&chain.pending_transactions());
        }
        "list" => {
            for block in &chain.chain {
                println!("Block {} -> {}", block.index, block.hash);
            }
        }
        _ => {
            eprintln!("unknown command: {}", args[0]);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_and_mine() {
        let mut chain = Blockchain::new();
        let tx = chain.transfer("alice", "bob", 250).unwrap();
        assert_eq!(tx.sender, "alice");
        assert_eq!(tx.recipient, "bob");
        assert!(!tx.stealth_commitment.is_empty());
        let block = chain.mine_block();
        assert_eq!(block.index, 1);
        assert!(!block.hash.is_empty());
    }
}

