use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Wallet {
    pub label: String,
    pub address: String,
    pub secret: String,
    pub public_key: String,
}

impl Wallet {
    pub fn new(label: &str) -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        let secret = hex::encode(signing_key.to_bytes());
        let public_key = hex::encode(signing_key.verifying_key().as_bytes());
        let address_hash = blake3::hash(signing_key.verifying_key().as_bytes())
            .to_hex()
            .to_string();
        let address = format!("raddr_{}", &address_hash[..20]);

        Self {
            label: label.to_string(),
            address,
            secret,
            public_key,
        }
    }

    pub fn sign(&self, message: &[u8]) -> Result<String, String> {
        let secret: [u8; 32] = hex::decode(&self.secret)
            .map_err(|error| format!("invalid wallet secret: {error}"))?
            .try_into()
            .map_err(|_| "wallet secret must be 32 bytes".to_string())?;
        let signing_key = SigningKey::from_bytes(&secret);
        let public_key = hex::encode(signing_key.verifying_key().as_bytes());
        if public_key != self.public_key {
            return Err("wallet public key does not match secret".to_string());
        }
        Ok(hex::encode(signing_key.sign(message).to_bytes()))
    }

    pub fn address_for_public_key(public_key: &str) -> Result<String, String> {
        let public_key: [u8; 32] = hex::decode(public_key)
            .map_err(|error| format!("invalid public key: {error}"))?
            .try_into()
            .map_err(|_| "public key must be 32 bytes".to_string())?;
        VerifyingKey::from_bytes(&public_key)
            .map_err(|error| format!("invalid public key: {error}"))?;
        let hash = blake3::hash(&public_key).to_hex().to_string();
        Ok(format!("raddr_{}", &hash[..20]))
    }

    pub fn verify(public_key: &str, message: &[u8], signature: &str) -> bool {
        let Ok(public_key) = hex::decode(public_key) else {
            return false;
        };
        let Ok(public_key): Result<[u8; 32], _> = public_key.try_into() else {
            return false;
        };
        let Ok(verifying_key) = VerifyingKey::from_bytes(&public_key) else {
            return false;
        };
        let Ok(signature) = hex::decode(signature) else {
            return false;
        };
        let Ok(signature): Result<[u8; 64], _> = signature.try_into() else {
            return false;
        };
        verifying_key
            .verify(message, &Signature::from_bytes(&signature))
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn repeated_wallet_labels_get_distinct_addresses() {
        let first = Wallet::new("same-label");
        let second = Wallet::new("same-label");

        assert_ne!(first.address, second.address);
        assert_ne!(first.secret, second.secret);
    }

    #[test]
    fn wallet_preserves_its_label() {
        let wallet = Wallet::new("test label");
        assert_eq!(wallet.label, "test label");
    }

    #[test]
    fn wallet_address_has_prefix_and_hex_payload() {
        let wallet = Wallet::new("format-test");
        let (prefix, payload) = wallet.address.split_at(6);

        assert_eq!(prefix, "raddr_");
        assert_eq!(payload.len(), 20);
        assert!(payload
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase()));
    }

    #[test]
    fn wallet_secret_is_a_64_character_hex_digest() {
        let wallet = Wallet::new("secret-format-test");

        assert_eq!(wallet.secret.len(), 64);
        assert!(wallet
            .secret
            .chars()
            .all(|character| character.is_ascii_hexdigit()));
    }

    #[test]
    fn different_labels_produce_different_wallets() {
        let first = Wallet::new("first-label");
        let second = Wallet::new("second-label");

        assert_ne!(first.address, second.address);
        assert_ne!(first.secret, second.secret);
    }

    #[test]
    fn concurrent_wallet_creation_produces_unique_addresses() {
        let addresses: HashSet<_> = (0..32)
            .map(|_| std::thread::spawn(|| Wallet::new("parallel").address))
            .map(|thread| thread.join().unwrap())
            .collect();

        assert_eq!(addresses.len(), 32);
    }
}
