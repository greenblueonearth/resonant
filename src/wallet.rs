use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Wallet {
    pub label: String,
    pub address: String,
    pub secret: String,
}

impl Wallet {
    pub fn new(label: &str) -> Self {
        let timestamp = unix_now();
        let secret = format!("{:x}", blake3::hash(format!("resonant:{}:{}", label, timestamp).as_bytes()));
        let address = format!("raddr_{}", &secret[..20]);

        Self {
            label: label.to_string(),
            address,
            secret,
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
