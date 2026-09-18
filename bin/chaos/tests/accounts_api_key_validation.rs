use assert_cmd::Command;
use chaos_kern::auth::AuthCredentialsStoreMode;
use chaos_kern::auth::load_auth_dot_json;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn account_command(home: &TempDir, provider: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_chaos"));
    command
        .current_dir(home.path())
        .env("CHAOS_HOME", home.path())
        .env_remove("CHAOS_STORAGE_URL")
        .env_remove("CHAOS_SQLITE_HOME")
        .env("NO_COLOR", "1")
        .timeout(std::time::Duration::from_secs(15))
        .args([
            "-c",
            "cli_auth_credentials_store=\"file\"",
            "-c",
            &format!("model_provider=\"{provider}\""),
            "accounts",
            "--with-api-key",
        ]);
    command
}

#[test]
fn cli_account_key_validation_rejects_without_creating_or_overwriting_credentials() {
    let home = TempDir::new().unwrap();
    let wrong_key = "sk-proj-wrong-provider-secret";
    let rejected = account_command(&home, "anthropic")
        .write_stdin(wrong_key)
        .assert()
        .failure()
        .stderr(contains("Invalid API key for Anthropic"));
    assert!(!String::from_utf8_lossy(&rejected.get_output().stderr).contains(wrong_key));
    assert!(!home.path().join("auth.json").exists());

    let valid_key = "sk-ant-api03-cli-fixture";
    account_command(&home, "anthropic")
        .write_stdin(format!(" {valid_key}\n"))
        .assert()
        .success();
    let path = home.path().join("auth.json");
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        load_auth_dot_json(home.path(), AuthCredentialsStoreMode::File)
            .unwrap()
            .unwrap()
            .provider_record("anthropic")
            .unwrap()
            .api_key
            .as_deref(),
        Some(valid_key)
    );

    account_command(&home, "anthropic")
        .write_stdin(wrong_key)
        .assert()
        .failure()
        .stderr(contains("Invalid API key for Anthropic"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
