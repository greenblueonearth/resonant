# Resonant v2

Resonant v2 is a cleaner and more modular Rust blockchain demo designed to be easy to understand, extend, and benchmark locally.

## Highlights
- modular layout: `wallet`, `ledger`, `main`
- faster in-memory transaction processing
- batch-style benchmark tooling
- privacy-inspired transaction model via stealth commitment and nullifier
- hash-based block construction with BLAKE3
- deterministic stake-weighted block proposer selection
- Ed25519-signed validator votes and stake-weighted finality certificates
- equivocation slashing and delayed unstaking

## Quick start

```bash
cargo run -- status
cargo run -- balance alice
cargo run -- stake alice 10000
cargo run -- wallet list
cargo run -- wallet create tester
cargo run -- transfer alice bob 250
cargo run -- mine
cargo run -- pending
cargo run -- benchmark 20000 2000
```

## Notes
This project remains a learning/demo implementation, not a production blockchain or complete cryptographic system.
The genesis state starts with Henry as a bootstrap proposer. The `ConsensusEngine` library API verifies Ed25519 validator votes, finalizes a block after more than two-thirds of the snapshot stake signs it, slashes and jails validators that sign conflicting hashes at one height, and releases unstaked funds after three finalized heights. `SignedVote` and `FinalityCertificate` are serializable so an external transport can exchange them between nodes.

This repository does not include peer discovery or P2P transport, validator key custody, slashing governance, or production network safety. The CLI's `mine` command creates a local block; network finality is provided through the library consensus API, not by opening network connections. State files created before PoS need to be removed or migrated because their blocks do not contain proposer and stake-snapshot metadata.
CLI state is stored in `.resonant-ledger.json` in the current directory. Set `RESONANT_LEDGER_PATH` to use another file; remove the state file to reset the demo ledger.

## Why v2
The first version proved the concept. v2 focuses on cleaner structure, better separation of concerns, and a more benchmark-friendly code layout.
