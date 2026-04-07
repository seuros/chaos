//! Helper binary for exercising shared custom CA environment handling in tests.
//!
//! The shared HTTP client honors `CODEX_CA_CERTIFICATE` and `SSL_CERT_FILE`, but those
//! environment variables are process-global and unsafe to mutate in parallel test execution. This
//! probe keeps the behavior under test while letting integration tests (`tests/ca_env.rs`) set
//! env vars per-process, proving:
//!
//! - env precedence is respected,
//! - multi-cert PEM bundles load,
//! - error messages guide users when CA files are invalid.
//!
//! The detailed explanation of what "hermetic" means here lives in `codex_client::custom_ca`.

use std::process;

fn main() {
    match codex_client::maybe_build_rustls_client_config_with_custom_ca() {
        Ok(config) => {
            if config.is_some() {
                println!("ok (custom CA loaded)");
            } else {
                println!("ok (system roots)");
            }
        }
        Err(error) => {
            eprintln!("{error}");
            process::exit(1);
        }
    }
}
