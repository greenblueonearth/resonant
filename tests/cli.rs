use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

struct StateFile(PathBuf);

impl StateFile {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Self(std::env::temp_dir().join(format!(
            "resonant-cli-test-{}-{nonce}.json",
            std::process::id()
        )))
    }

    fn command(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_resonant"))
            .args(arguments)
            .env("RESONANT_LEDGER_PATH", &self.0)
            .output()
            .unwrap()
    }

    fn spawn(&self, arguments: &[&str]) -> std::process::Child {
        Command::new(env!("CARGO_BIN_EXE_resonant"))
            .args(arguments)
            .env("RESONANT_LEDGER_PATH", &self.0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
}

impl Drop for StateFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        let _ = fs::remove_file(self.0.with_extension("json.tmp"));
        let _ = fs::remove_file(self.0.with_extension("lock"));
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
        &state.0,
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
    fs::write(&state.0, b"not json").unwrap();

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
    fs::write(&state.0, invalid_state).unwrap();

    let output = state.command(&["benchmark", "1", "1"]);

    assert!(output.status.success());
    assert_eq!(fs::read(&state.0).unwrap(), invalid_state);
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
