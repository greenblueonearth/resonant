# Resonant v2

Resonant v2 is a cleaner and more modular Rust blockchain demo designed to be easy to understand, extend, and benchmark locally.

## Highlights
- modular layout: `wallet`, `ledger`, `main`
- faster in-memory transaction processing
- batch-style benchmark tooling
- privacy-inspired transaction model via stealth commitment and nullifier
- hash-based block construction with BLAKE3

## Quick start

```bash
cargo run -- status
cargo run -- balance alice
cargo run -- wallet list
cargo run -- wallet create tester
cargo run -- transfer alice bob 250
cargo run -- mine
cargo run -- pending
cargo run -- benchmark 20000 2000
```

## Notes
This project remains a learning/demo implementation, not a production blockchain or complete cryptographic system.

## Why v2
The first version proved the concept. v2 focuses on cleaner structure, better separation of concerns, and a more benchmark-friendly code layout.
