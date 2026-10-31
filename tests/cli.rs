use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

struct StateFile {
    path: PathBuf,
    consensus_path: PathBuf,
}

impl StateFile {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "resonant-cli-test-{}-{nonce}.json",
            std::process::id()
        ));
        let consensus_path = path.with_extension("consensus.json");
        Self {
            path,
            consensus_path,
        }
    }

    fn command(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_resonant"))
            .args(arguments)
            .env("RESONANT_LEDGER_PATH", &self.path)
            .env("RESONANT_CONSENSUS_PATH", &self.consensus_path)
            .output()
            .unwrap()
    }

    fn spawn(&self, arguments: &[&str]) -> std::process::Child {
        Command::new(env!("CARGO_BIN_EXE_resonant"))
            .args(arguments)
            .env("RESONANT_LEDGER_PATH", &self.path)
            .env("RESONANT_CONSENSUS_PATH", &self.consensus_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
}

impl Drop for StateFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(self.path.with_extension("json.tmp"));
        let _ = fs::remove_file(self.path.with_extension("lock"));
        let _ = fs::remove_file(&self.consensus_path);
        let _ = fs::remove_file(self.consensus_path.with_extension("json.tmp"));
        let _ = fs::remove_file(self.path.with_extension("wallet.json"));
    }
}

#[test]
fn concurrent_transfers_are_preserved() {
    let state = StateFile::new();
    let first = state.spawn(&["transfer", "alice", "bob", "100"]);
    let second = state.spawn(&["transfer", "alice", "charlie", "200"]);

    assert!(first.wait_with_output().unwrap().status.success());
    assert!(second.wait_with_output().unwrap().status.success());

    let pending = state.command(&["pending"]);
    assert!(pending.status.success());
    assert_eq!(String::from_utf8_lossy(&pending.stdout).lines().count(), 2);

    let mined = state.command(&["mine"]);
    assert!(mined.status.success());
    assert_eq!(
        String::from_utf8_lossy(&state.command(&["balance", "alice"]).stdout).trim(),
        "alice: 999999698"
    );
    assert_eq!(
        String::from_utf8_lossy(&state.command(&["balance", "bob"]).stdout).trim(),
        "bob: 1000000100"
    );
    assert_eq!(
        String::from_utf8_lossy(&state.command(&["balance", "charlie"]).stdout).trim(),
        "charlie: 1000000200"
    );
}

#[test]
fn wallet_list_shows_accounts_without_status_summary() {
    let state = StateFile::new();
    let stake = state.command(&["stake", "alice", "100"]);
    assert!(stake.status.success());
    assert!(String::from_utf8_lossy(&stake.stdout).contains("total stake: 100"));

    let output = state.command(&["wallet", "list"]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.lines().any(|line| line.starts_with("alice: ")));
    assert!(!stdout.contains("Resonant v2 status"));

    let mined = state.command(&["mine"]);
    let mined_stdout = String::from_utf8_lossy(&mined.stdout);
    assert!(mined.status.success());
    assert!(mined_stdout.contains("mined block 1 by "));
}

#[test]
fn invalid_state_is_reported_without_panicking() {
    let state = StateFile::new();
    fs::write(
        &state.path,
        br#"{"balances":{},"pending":[],"chain":[],"nonce":0}"#,
    )
    .unwrap();

    let output = state.command(&["status"]);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stderr.contains("invalid ledger state"));
    assert!(!stderr.contains("panicked"));
}

#[test]
fn help_works_even_when_state_is_invalid() {
    let state = StateFile::new();
    fs::write(&state.path, b"not json").unwrap();

    let output = state.command(&["help"]);

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("usage:"));
}

#[test]
fn no_arguments_prints_usage() {
    let output = StateFile::new().command(&[]);

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("usage:"));
}

#[test]
fn unknown_command_returns_an_error() {
    let output = StateFile::new().command(&["not-a-command"]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown command"));
}

#[test]
fn balance_command_requires_an_account() {
    let output = StateFile::new().command(&["balance"]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing account"));
}

#[test]
fn transfer_rejects_non_integer_amounts() {
    let output = StateFile::new().command(&["transfer", "alice", "bob", "1.5"]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("non-negative integer"));
}

#[test]
fn benchmark_does_not_read_or_modify_persisted_state() {
    let state = StateFile::new();
    let invalid_state = b"not json";
    fs::write(&state.path, invalid_state).unwrap();

    let output = state.command(&["benchmark", "1", "1"]);

    assert!(output.status.success());
    assert_eq!(fs::read(&state.path).unwrap(), invalid_state);
}

#[test]
fn created_wallet_appears_in_wallet_list() {
    let state = StateFile::new();
    let created = state.command(&["wallet", "create", "tester"]);
    assert!(created.status.success());
    let created_stdout = String::from_utf8_lossy(&created.stdout);
    let address = created_stdout.split_whitespace().nth(1).unwrap();

    let listed = state.command(&["wallet", "list"]);

    assert!(listed.status.success());
    assert!(String::from_utf8_lossy(&listed.stdout).contains(address));
}

#[test]
fn pending_command_reports_empty_pool() {
    let output = StateFile::new().command(&["pending"]);

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "no pending transactions"
    );
}

#[test]
fn p2p_identity_and_validator_registration_use_private_key_files() {
    let state = StateFile::new();
    let key_path = state.path.with_extension("wallet.json");
    let created = state.command(&[
        "p2p",
        "identity-create",
        key_path.to_str().unwrap(),
        "validator-a",
    ]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let wallet: resonant::wallet::Wallet =
        serde_json::from_slice(&fs::read(&key_path).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&key_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let staked = state.command(&["stake", &wallet.address, "1000"]);
    assert!(staked.status.success());
    let registered = state.command(&[
        "p2p",
        "validator",
        "register",
        &wallet.address,
        &wallet.public_key,
        "1000",
    ]);
    assert!(
        registered.status.success(),
        "{}",
        String::from_utf8_lossy(&registered.stderr)
    );

    let consensus: resonant::consensus::ConsensusEngine =
        serde_json::from_slice(&fs::read(&state.consensus_path).unwrap()).unwrap();
    assert_eq!(consensus.validator(&wallet.address).unwrap().stake, 1000);
}

#[test]
fn signed_transfer_cli_creates_owner_authenticated_transaction() {
    let state = StateFile::new();
    let key_path = state.path.with_extension("wallet.json");
    let created = state.command(&[
        "p2p",
        "identity-create",
        key_path.to_str().unwrap(),
        "transaction-owner",
    ]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let wallet: resonant::wallet::Wallet =
        serde_json::from_slice(&fs::read(&key_path).unwrap()).unwrap();

    let transfer = state.command(&["transfer-signed", key_path.to_str().unwrap(), "bob", "25"]);
    assert!(
        transfer.status.success(),
        "{}",
        String::from_utf8_lossy(&transfer.stderr)
    );
    let ledger: resonant::ledger::Ledger =
        serde_json::from_slice(&fs::read(&state.path).unwrap()).unwrap();

    assert_eq!(ledger.pending.len(), 1);
    assert_eq!(ledger.pending[0].sender, wallet.address);
    assert!(resonant::ledger::validate_transaction_authorization(&ledger.pending[0]).is_ok());
}
