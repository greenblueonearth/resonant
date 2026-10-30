use fs2::FileExt;
use resonant::ledger::{run_benchmark, Ledger};
use std::fs::{self, File, OpenOptions};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::ExitCode;

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
    "usage: resonant <status|balance ACCOUNT|stake ACCOUNT AMOUNT|wallet list|wallet create LABEL|transfer SENDER RECIPIENT AMOUNT|mine|pending|benchmark [TX_COUNT] [BATCH_SIZE]>"
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
