# Resonant

Resonant is a minimal, fast, Sui-inspired Rust blockchain demo focused on:
- low-overhead transaction flow
- stealth-style masking for transaction privacy
- post-quantum-inspired hash-based security design
- easy local testing and experimentation

## Features
- block mining using BLAKE3
- basic mempool and transaction validation
- stealth commitment + nullifier pattern for privacy-oriented transfers
- quantum-resistant style design via hash-based commitments and signatures
- JSON output for easy inspection

## Run

```bash
cargo run -- status
cargo run -- balance alice
cargo run -- transfer alice bob 250
cargo run -- mine
cargo run -- pending
```

## Notes
This is a learning/demo project. It is intentionally simple and not a production-ready blockchain or cryptographic implementation.
