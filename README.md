# Resonant

Resonant is a compact Rust blockchain demo inspired by the speed and simplicity goals of Sui-style architectures, while focusing on a local, testable implementation with lightweight P2P networking.

## What is included
- wallet generation and account management
- transaction mempool and block mining
- hash-based ledger structure using BLAKE3
- stealth commitment and nullifier patterns for privacy-inspired transaction flow
- local peer-to-peer node communication over TCP sockets
- CLI for demoing wallet and network behavior

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

## P2P model

This version includes a lightweight P2P layer for local testing:
- nodes bind to localhost ports
- peers are configured as socket endpoints
- transactions and mined blocks are broadcast between peers as JSON messages
- the demo creates three local nodes (`alpha`, `beta`, and `gamma`) for a small network simulation

## Notes
This is an educational and demo project. It is not a production blockchain, not a formal cryptographic implementation, and not a fully audited consensus system.

## Recommended next upgrades
- validator or stake simulation
- node synchronization rules and conflict handling
- wallet signing / verification
- more realistic state replication across peers
- zero-knowledge style shielding for transactions
