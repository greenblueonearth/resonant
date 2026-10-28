# Resonant

Resonant is a compact Rust blockchain demo inspired by the speed and simplicity goals of Sui-style architectures, while focusing on a small, local, and testable implementation.

## What it includes
- lightweight blockchain ledger
- wallet generation and account management
- in-memory local node simulation
- transaction mempool and block mining
- privacy-oriented transaction design via stealth commitment and nullifier
- hash-based security model inspired by post-quantum direction without being a full cryptographic suite

## Quick start

```bash
cargo run -- status
cargo run -- balance alice
cargo run -- wallet list
cargo run -- wallet create tester
cargo run -- transfer alice bob 250
cargo run -- mine
cargo run -- pending
cargo run -- node demo
cargo run -- node run alpha
```

## Local network demo

The node system exposes a lightweight network model with three sample nodes (`alpha`, `beta`, and `gamma`) to test the flow of transactions and mining locally.

## Notes
This is a learning and demo project designed for experimentation and local development. It is intentionally not a production-grade blockchain or a complete cryptographic protocol.

## Future upgrades
- real P2P TCP/UDP node communication
- wallet signing and verification
- stake or validator simulation
- zero-knowledge style transaction shielding
- more realistic consensus model
