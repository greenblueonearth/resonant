use fs2::FileExt;
use resonant::consensus::ConsensusEngine;
use resonant::ledger::{run_benchmark, Ledger};
use resonant::p2p_consensus::ConsensusP2pNode;
use resonant::wallet::Wallet;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

#[derive(serde::Serialize, serde::Deserialize)]
struct NodeStateSnapshot {
    ledger: Ledger,
    consensus: ConsensusEngine,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    if matches!(command.as_deref(), Some("help") | None) {
        print_usage();
        return Ok(());
    }
    if command.as_deref() == Some("p2p") {
        return run_p2p(args);
    }

    let _state_lock = if command.as_deref() == Some("benchmark") {
        None
    } else {
        Some(lock_state_file()?)
    };
    let mut ledger = if command.as_deref() == Some("benchmark") {
        Ledger::new()
    } else {
        load_ledger()?
    };

    match command.as_deref() {
        Some("status") => println!("{}", ledger.status()),
        Some("balance") => {
            let account = required_arg(&mut args, "account")?;
            println!("{}: {}", account, ledger.balance_of(&account));
        }
        Some("stake") => {
            let account = required_arg(&mut args, "validator account")?;
            let amount = parse_u64(&required_arg(&mut args, "stake amount")?, "stake amount")?;
            let total_stake = ledger.stake(&account, amount)?;
            save_ledger(&ledger)?;
            println!(
                "{} staked {}; total stake: {}",
                account, amount, total_stake
            );
        }
        Some("wallet") => match args.next().as_deref() {
            Some("list") => {
                for (account, balance) in ledger.accounts() {
                    println!("{}: {}", account, balance);
                }
            }
            Some("create") => {
                let label = required_arg(&mut args, "label")?;
                let wallet = ledger.create_wallet(&label);
                save_ledger(&ledger)?;
                println!("{} {}", wallet.label, wallet.address);
            }
            _ => return Err("usage: resonant wallet <list|create LABEL>".to_string()),
        },
        Some("transfer") => {
            let sender = required_arg(&mut args, "sender")?;
            let recipient = required_arg(&mut args, "recipient")?;
            let amount = parse_u64(&required_arg(&mut args, "amount")?, "amount")?;
            let tx = ledger.transfer(&sender, &recipient, amount)?;
            save_ledger(&ledger)?;
            println!("created {} (fee: {})", tx.id, tx.fee);
        }
        Some("transfer-signed") => {
            let wallet_path = PathBuf::from(required_arg(&mut args, "wallet file")?);
            let recipient = required_arg(&mut args, "recipient")?;
            let amount = parse_u64(&required_arg(&mut args, "amount")?, "amount")?;
            let wallet = read_wallet_file(&wallet_path)?;
            let tx = ledger.transfer_signed(&wallet, &recipient, amount)?;
            save_ledger(&ledger)?;
            println!("created authenticated {} (fee: {})", tx.id, tx.fee);
        }
        Some("mine") => {
            let block = ledger.mine_block()?;
            save_ledger(&ledger)?;
            println!(
                "mined block {} by {} with {} transaction(s): {}",
                block.index,
                block.validator,
                block.transactions.len(),
                block.hash
            );
        }
        Some("pending") => {
            if ledger.pending.is_empty() {
                println!("no pending transactions");
            } else {
                for tx in &ledger.pending {
                    println!(
                        "{}: {} -> {} ({})",
                        tx.id, tx.sender, tx.recipient, tx.amount
                    );
                }
            }
        }
        Some("benchmark") => {
            let tx_count = args
                .next()
                .map(|value| parse_u64(&value, "transaction count"))
                .transpose()?
                .unwrap_or(20_000);
            let batch_size = args
                .next()
                .map(|value| parse_u64(&value, "batch size"))
                .transpose()?
                .unwrap_or(2_000);
            let result = run_benchmark(tx_count, batch_size);
            println!(
                "transactions: {}\nelapsed_ms: {:.2}\ntx_per_second: {:.2}\nblocks: {}",
                result.tx_count, result.elapsed_ms, result.tx_per_second, result.block_count
            );
        }
        Some("help") | None => print_usage(),
        Some(other) => return Err(format!("unknown command: {other}\n{}", usage())),
    }

    Ok(())
}

fn run_p2p(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    match args.next().as_deref() {
        Some("identity-create") => {
            let key_path = PathBuf::from(required_arg(&mut args, "identity file path")?);
            let label = required_arg(&mut args, "identity label")?;
            let _state_lock = lock_state_file()?;
            let mut ledger = load_ledger()?;
            let wallet = Wallet::new(&label);
            ledger.import_wallet(&wallet)?;
            write_wallet_file(&key_path, &wallet)?;
            if let Err(error) = save_ledger(&ledger) {
                let _ = fs::remove_file(&key_path);
                return Err(error);
            }
            println!(
                "address: {}\npublic_key: {}",
                wallet.address, wallet.public_key
            );
            Ok(())
        }
        Some("validator") => match args.next().as_deref() {
            Some("register") => {
                let address = required_arg(&mut args, "validator address")?;
                let public_key = required_arg(&mut args, "validator public key")?;
                let stake = parse_u64(&required_arg(&mut args, "stake amount")?, "stake amount")?;
                let _state_lock = lock_state_file()?;
                let ledger = load_ledger()?;
                let actual_stake = ledger.stake_of(&address);
                if actual_stake != stake {
                    return Err(format!(
                        "wallet stake is {actual_stake}, but registration requested {stake}"
                    ));
                }
                let mut consensus = load_consensus()?;
                consensus.register_public_validator(&address, &public_key, stake)?;
                save_consensus(&consensus)?;
                println!("registered validator {address} with stake {stake}");
                Ok(())
            }
            _ => Err(format!("usage: {}\n", p2p_usage())),
        },
        Some("node") => run_p2p_node(args),
        _ => Err(format!("usage: {}\n", p2p_usage())),
    }
}

fn run_p2p_node(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let address = parse_socket_addr(&required_arg(&mut args, "listen address")?)?;
    let identity_path = PathBuf::from(required_arg(&mut args, "identity file")?);
    let mut advertised_address = address;
    let mut trusted_peers = BTreeSet::new();
    let mut seeds = Vec::new();
    let mut propose_on_start = false;
    while let Some(option) = args.next() {
        match option.as_str() {
            "--advertise" => {
                advertised_address =
                    parse_socket_addr(&required_arg(&mut args, "advertised address")?)?;
            }
            "--trust" => {
                let public_key = required_arg(&mut args, "trusted peer public key")?;
                trusted_peers.insert(public_key);
            }
            "--seed" => {
                let seed = parse_socket_addr(&required_arg(&mut args, "seed address")?)?;
                seeds.push(seed);
            }
            "--propose-on-start" => propose_on_start = true,
            _ => return Err(format!("unknown P2P option: {option}\n{}", p2p_usage())),
        }
    }

    let _state_lock = lock_state_file()?;
    let ledger = load_ledger()?;
    let consensus = load_consensus()?;
    let identity = read_wallet_file(&identity_path)?;
    let mut node = ConsensusP2pNode::bind_with_advertise(
        address,
        advertised_address,
        identity,
        trusted_peers,
        ledger,
        consensus,
    )?;
    println!(
        "listening on {}; advertising {}; identity: {}",
        node.local_addr(),
        node.advertised_addr(),
        node.public_key()
    );
    if !seeds.is_empty() {
        let peers = node.discover(&seeds)?;
        println!("discovered {} peer(s):", peers.len());
        for peer in peers {
            println!("{peer}");
        }
    }
    if propose_on_start {
        let event = node.propose_block()?;
        save_node_state(&node)?;
        println!("{event:?}");
    }

    loop {
        match node.receive(Duration::from_secs(1)) {
            Ok(Some(event)) => {
                save_node_state(&node)?;
                println!("{event:?}");
            }
            Ok(None) => {}
            Err(error) => {
                save_node_state(&node)?;
                eprintln!("rejected P2P message: {error}");
            }
        }
    }
}

fn save_node_state(node: &ConsensusP2pNode) -> Result<(), String> {
    let snapshot = NodeStateSnapshot {
        ledger: node.ledger().clone(),
        consensus: node.consensus().clone(),
    };
    write_node_state_snapshot(
        &state_path(),
        &consensus_state_path(),
        &node_state_journal_path(),
        &snapshot,
    )
}

fn node_state_journal_path() -> PathBuf {
    state_path().with_extension("node-state.json")
}

fn write_node_state_snapshot(
    ledger_path: &std::path::Path,
    consensus_path: &std::path::Path,
    journal_path: &std::path::Path,
    snapshot: &NodeStateSnapshot,
) -> Result<(), String> {
    validate_node_state_snapshot(snapshot)?;
    let contents = serde_json::to_vec(snapshot)
        .map_err(|error| format!("could not serialize node state snapshot: {error}"))?;
    atomic_write(journal_path, &contents)?;
    recover_node_state_snapshot(ledger_path, consensus_path, journal_path)
}

fn recover_node_state_snapshot(
    ledger_path: &std::path::Path,
    consensus_path: &std::path::Path,
    journal_path: &std::path::Path,
) -> Result<(), String> {
    let contents = match fs::read(journal_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "could not read node state journal {}: {error}",
                journal_path.display()
            ))
        }
    };
    let snapshot: NodeStateSnapshot = serde_json::from_slice(&contents)
        .map_err(|error| format!("could not parse node state journal: {error}"))?;
    validate_node_state_snapshot(&snapshot)?;
    let ledger_contents = serde_json::to_vec(&snapshot.ledger)
        .map_err(|error| format!("could not serialize ledger state: {error}"))?;
    let consensus_contents = serde_json::to_vec(&snapshot.consensus)
        .map_err(|error| format!("could not serialize consensus state: {error}"))?;
    atomic_write(ledger_path, &ledger_contents)?;
    atomic_write(consensus_path, &consensus_contents)?;
    fs::remove_file(journal_path).map_err(|error| {
        format!(
            "could not remove recovered node state journal {}: {error}",
            journal_path.display()
        )
    })?;
    sync_parent_directory(journal_path)?;
    Ok(())
}

fn validate_node_state_snapshot(snapshot: &NodeStateSnapshot) -> Result<(), String> {
    snapshot
        .ledger
        .validate()
        .map_err(|error| format!("invalid ledger in node state snapshot: {error}"))?;
    if snapshot.ledger.height() != snapshot.consensus.finalized_height() {
        return Err(format!(
            "node state snapshot has ledger height {} but consensus finalized height {}",
            snapshot.ledger.height(),
            snapshot.consensus.finalized_height()
        ));
    }
    Ok(())
}

fn atomic_write(path: &std::path::Path, contents: &[u8]) -> Result<(), String> {
    let temporary_path = path.with_extension("json.tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary_path)
        .map_err(|error| format!("could not open {}: {error}", temporary_path.display()))?;
    file.write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("could not write {}: {error}", temporary_path.display()))?;
    fs::rename(&temporary_path, path)
        .map_err(|error| format!("could not save {}: {error}", path.display()))?;
    sync_parent_directory(path)
}

fn sync_parent_directory(path: &std::path::Path) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("could not sync directory {}: {error}", parent.display()))
}

fn consensus_state_path() -> PathBuf {
    std::env::var_os("RESONANT_CONSENSUS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".resonant-consensus.json"))
}

fn load_consensus() -> Result<ConsensusEngine, String> {
    let path = consensus_state_path();
    match fs::read(&path) {
        Ok(contents) => serde_json::from_slice(&contents)
            .map_err(|error| format!("could not parse {}: {error}", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(ConsensusEngine::default()),
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

fn save_consensus(consensus: &ConsensusEngine) -> Result<(), String> {
    let path = consensus_state_path();
    let temporary_path = path.with_extension("json.tmp");
    let contents = serde_json::to_vec(consensus)
        .map_err(|error| format!("could not serialize consensus state: {error}"))?;
    fs::write(&temporary_path, contents)
        .map_err(|error| format!("could not write {}: {error}", temporary_path.display()))?;
    fs::rename(&temporary_path, &path)
        .map_err(|error| format!("could not save {}: {error}", path.display()))
}

fn read_wallet_file(path: &std::path::Path) -> Result<Wallet, String> {
    let contents =
        fs::read(path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let wallet: Wallet = serde_json::from_slice(&contents)
        .map_err(|error| format!("could not parse wallet file {}: {error}", path.display()))?;
    if Wallet::address_for_public_key(&wallet.public_key)? != wallet.address
        || !Wallet::verify(
            &wallet.public_key,
            b"resonant/p2p-identity-check/v1",
            &wallet.sign(b"resonant/p2p-identity-check/v1")?,
        )
    {
        return Err(format!("invalid wallet identity in {}", path.display()));
    }
    Ok(wallet)
}

fn write_wallet_file(path: &std::path::Path, wallet: &Wallet) -> Result<(), String> {
    let contents = serde_json::to_vec_pretty(wallet)
        .map_err(|error| format!("could not serialize wallet identity: {error}"))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    file.write_all(&contents)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("could not securely write {}: {error}", path.display()))
}

fn parse_socket_addr(value: &str) -> Result<SocketAddr, String> {
    value
        .parse()
        .map_err(|error| format!("invalid peer address {value}: {error}"))
}

fn state_path() -> PathBuf {
    std::env::var_os("RESONANT_LEDGER_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".resonant-ledger.json"))
}

fn lock_state_file() -> Result<File, String> {
    let path = state_path().with_extension("lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("could not open {}: {error}", path.display()))?;
    FileExt::lock_exclusive(&lock)
        .map_err(|error| format!("could not lock {}: {error}", path.display()))?;
    recover_node_state_snapshot(
        &state_path(),
        &consensus_state_path(),
        &node_state_journal_path(),
    )?;
    Ok(lock)
}

fn load_ledger() -> Result<Ledger, String> {
    let path = state_path();
    match fs::read(&path) {
        Ok(contents) => {
            let ledger: Ledger = serde_json::from_slice(&contents)
                .map_err(|error| format!("could not parse {}: {error}", path.display()))?;
            ledger
                .validate()
                .map_err(|error| format!("invalid ledger state in {}: {error}", path.display()))?;
            Ok(ledger)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Ledger::new()),
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

fn save_ledger(ledger: &Ledger) -> Result<(), String> {
    let path = state_path();
    let temporary_path = path.with_extension("json.tmp");
    let contents = serde_json::to_vec(ledger)
        .map_err(|error| format!("could not serialize ledger: {error}"))?;
    fs::write(&temporary_path, contents)
        .map_err(|error| format!("could not write {}: {error}", temporary_path.display()))?;
    fs::rename(&temporary_path, &path)
        .map_err(|error| format!("could not save {}: {error}", path.display()))
}

fn required_arg(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String, String> {
    args.next()
        .ok_or_else(|| format!("missing {name}\n{}", usage()))
}

fn parse_u64(value: &str, name: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{name} must be a non-negative integer: {value}"))
}

fn print_usage() {
    println!("{}", usage());
}

fn usage() -> &'static str {
    "usage: resonant <status|balance ACCOUNT|stake ACCOUNT AMOUNT|wallet list|wallet create LABEL|transfer SENDER RECIPIENT AMOUNT|transfer-signed WALLET_FILE RECIPIENT AMOUNT|mine|pending|benchmark [TX_COUNT] [BATCH_SIZE]|p2p identity-create KEY_FILE LABEL|p2p validator register ADDRESS PUBLIC_KEY STAKE|p2p node LISTEN_ADDR IDENTITY_FILE [--advertise ADDR] [--trust PUBLIC_KEY]... [--seed ADDR]... [--propose-on-start]>"
}

fn p2p_usage() -> &'static str {
    "resonant p2p identity-create KEY_FILE LABEL | p2p validator register ADDRESS PUBLIC_KEY STAKE | p2p node LISTEN_ADDR IDENTITY_FILE [--advertise ADDR] [--trust PUBLIC_KEY]... [--seed ADDR]... [--propose-on-start]"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_directory(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("resonant-{label}-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).unwrap();
        path
    }

    fn finalized_snapshot() -> NodeStateSnapshot {
        let validator = Wallet::new("journal-validator");
        let mut ledger = Ledger::new();
        ledger.import_wallet(&validator).unwrap();
        ledger.stake(&validator.address, 100).unwrap();
        let block = ledger.mine_block().unwrap();

        let mut consensus = ConsensusEngine::new(3);
        consensus.register_validator(&validator, 100).unwrap();
        let vote = consensus
            .make_vote(&validator, block.index, &block.hash)
            .unwrap();
        consensus.submit_vote(vote).unwrap().unwrap();

        NodeStateSnapshot { ledger, consensus }
    }

    #[test]
    fn ledger_state_round_trips_through_state_file() {
        let path = std::env::temp_dir().join(format!(
            "resonant-ledger-test-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let previous_path = std::env::var_os("RESONANT_LEDGER_PATH");
        std::env::set_var("RESONANT_LEDGER_PATH", &path);

        let result = (|| {
            let mut ledger = Ledger::new();
            ledger.stake("alice", 25)?;
            ledger.transfer("alice", "bob", 25)?;
            save_ledger(&ledger)?;

            let mut restored = load_ledger()?;
            assert_eq!(restored.pending.len(), 1);
            assert_eq!(restored.stake_of("alice"), 25);
            let block = restored.mine_block()?;
            assert_eq!(block.transactions.len(), 1);
            assert_eq!(restored.balance_of("alice"), 1_000_000_000 - 26);

            fs::write(
                &path,
                br#"{"balances":{},"pending":[],"chain":[],"nonce":0}"#,
            )
            .map_err(|error| error.to_string())?;
            assert!(load_ledger()
                .unwrap_err()
                .contains("ledger chain must contain a genesis block"));
            Ok::<(), String>(())
        })();

        match previous_path {
            Some(value) => std::env::set_var("RESONANT_LEDGER_PATH", value),
            None => std::env::remove_var("RESONANT_LEDGER_PATH"),
        }
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("json.tmp"));
        result.unwrap();
    }

    #[test]
    fn node_state_journal_recovers_after_consensus_write_failure() {
        let directory = test_directory("journal-recovery");
        let ledger_path = directory.join("ledger.json");
        let consensus_path = directory.join("consensus.json");
        let journal_path = directory.join("node-state.json");
        let snapshot = finalized_snapshot();
        fs::write(&ledger_path, serde_json::to_vec(&Ledger::new()).unwrap()).unwrap();
        fs::create_dir(&consensus_path).unwrap();

        assert!(
            write_node_state_snapshot(&ledger_path, &consensus_path, &journal_path, &snapshot)
                .is_err()
        );
        assert!(journal_path.exists());
        assert_eq!(
            serde_json::from_slice::<Ledger>(&fs::read(&ledger_path).unwrap())
                .unwrap()
                .height(),
            snapshot.ledger.height()
        );

        fs::remove_dir(&consensus_path).unwrap();
        let _ = fs::remove_file(consensus_path.with_extension("json.tmp"));
        recover_node_state_snapshot(&ledger_path, &consensus_path, &journal_path).unwrap();

        let recovered_ledger: Ledger =
            serde_json::from_slice(&fs::read(&ledger_path).unwrap()).unwrap();
        let recovered_consensus: ConsensusEngine =
            serde_json::from_slice(&fs::read(&consensus_path).unwrap()).unwrap();
        assert_eq!(
            recovered_ledger.height(),
            recovered_consensus.finalized_height()
        );
        assert!(!journal_path.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn mismatched_node_snapshot_is_rejected_before_journaling() {
        let directory = test_directory("journal-mismatch");
        let ledger_path = directory.join("ledger.json");
        let consensus_path = directory.join("consensus.json");
        let journal_path = directory.join("node-state.json");
        let snapshot = NodeStateSnapshot {
            ledger: Ledger::new(),
            consensus: finalized_snapshot().consensus,
        };

        assert!(
            write_node_state_snapshot(&ledger_path, &consensus_path, &journal_path, &snapshot)
                .is_err()
        );
        assert!(!journal_path.exists());
        assert!(!ledger_path.exists());
        assert!(!consensus_path.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn corrupt_state_journal_is_not_silently_discarded() {
        let directory = test_directory("journal-corrupt");
        let ledger_path = directory.join("ledger.json");
        let consensus_path = directory.join("consensus.json");
        let journal_path = directory.join("node-state.json");
        fs::write(&journal_path, b"not-json").unwrap();

        let error =
            recover_node_state_snapshot(&ledger_path, &consensus_path, &journal_path).unwrap_err();

        assert!(error.contains("could not parse node state journal"));
        assert!(journal_path.exists());
        assert!(!ledger_path.exists());
        assert!(!consensus_path.exists());
        fs::remove_dir_all(directory).unwrap();
    }
}
