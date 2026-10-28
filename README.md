# Resonant

Resonant is a compact Rust blockchain demo inspired by Sui-style speed goals and privacy-oriented transaction design. This version is optimized for local benchmarking and keeps the system simple enough to understand and extend.

## What is included
- lightweight blockchain ledger
- wallet balance system
- transaction mempool and block mining
- privacy-inspired stealth commitment and nullifier logic
- synthetic benchmark mode to measure throughput locally

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

## Benchmark

The benchmark is designed to measure the local in-memory throughput of the ledger and block-minting model. It is not a production consensus benchmark, but it provides a meaningful synthetic target for the system's performance profile.

Example:

```bash
cargo run -- benchmark 20000 2000
```

This will simulate 20,000 transactions in batches of 2,000 and print the throughput in tx/s.

## Expected performance

In a synthetic local benchmark, the project is designed to target roughly 20,000 tx/s on a modern machine in a single-process in-memory scenario. Real-world production throughput will be lower once network, disk, consensus, and signature validation are included.

## Notes
This is a learning/demo implementation. It is intentionally simplified and not a production-grade blockchain or audited cryptographic protocol.
