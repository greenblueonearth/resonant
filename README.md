# Resonant

Resonant is a compact Rust blockchain demo inspired by the speed and simplicity goals of Sui-style architectures, while staying small, local, and easy to compile.

## Features
- wallet generation and balance tracking
- mempool and block mining
- stealth commitment and nullifier patterns for privacy-inspired transactions
- local multi-node simulation for a lightweight blockchain demo
- hash-based security model using BLAKE3

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
```

## Notes
This project is intentionally a learning/demo implementation. It is not a production-grade blockchain or a complete cryptographic protocol.

## Next improvements
- validator/stake simulation
- wallet signing and verification
- state synchronization between nodes
- stronger privacy primitives
