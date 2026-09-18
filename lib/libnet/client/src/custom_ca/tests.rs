use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::BuildCustomCaTransportError;
use super::EnvSource;
use super::SSL_CERT_FILE_ENV;
use super::maybe_build_rustls_client_config_with_env_and_native_roots;

const TEST_CERT: &str = include_str!("../../tests/fixtures/test-ca.pem");

struct MapEnv {
    values: HashMap<String, String>,
}

impl EnvSource for MapEnv {
    fn var(&self, key: &str) -> Option<String> {
        self.values.get(key).cloned()
    }
}

fn map_env(pairs: &[(&str, &str)]) -> MapEnv {
    MapEnv {
        values: pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
    }
}

fn write_cert_file(temp_dir: &TempDir, name: &str, contents: &str) -> PathBuf {
    let path = temp_dir.path().join(name);
    fs::write(&path, contents).unwrap_or_else(|error| {
        panic!("write cert fixture failed for {}: {error}", path.display())
    });
    path
}

#[test]
fn custom_ca_suite() {
    ca_path_uses_ssl_cert_file();
    ca_path_ignores_empty_values();
    rustls_config_uses_custom_ca_bundle_when_configured();
    rustls_config_reports_invalid_ca_file();
}

fn ca_path_uses_ssl_cert_file() {
    let env = map_env(&[(SSL_CERT_FILE_ENV, "/tmp/fallback.pem")]);

    assert_eq!(
        env.configured_ca_bundle().map(|bundle| bundle.path),
        Some(PathBuf::from("/tmp/fallback.pem"))
    );
}

fn ca_path_ignores_empty_values() {
    let env = map_env(&[(SSL_CERT_FILE_ENV, "")]);

    assert_eq!(env.configured_ca_bundle().map(|bundle| bundle.path), None);
}

fn rustls_config_uses_custom_ca_bundle_when_configured() {
    let temp_dir = TempDir::new().expect("tempdir");
    let cert_path = write_cert_file(&temp_dir, "ca.pem", TEST_CERT);
    let env = map_env(&[(SSL_CERT_FILE_ENV, cert_path.to_string_lossy().as_ref())]);

    let config = maybe_build_rustls_client_config_with_env_and_native_roots(
        &env,
        rustls_native_certs::CertificateResult::default,
    )
    .expect("rustls config")
    .expect("custom CA config should be present");

    assert!(config.enable_sni);
}

fn rustls_config_reports_invalid_ca_file() {
    let temp_dir = TempDir::new().expect("tempdir");
    let cert_path = write_cert_file(&temp_dir, "empty.pem", "");
    let env = map_env(&[(SSL_CERT_FILE_ENV, cert_path.to_string_lossy().as_ref())]);

    let error = maybe_build_rustls_client_config_with_env_and_native_roots(
        &env,
        rustls_native_certs::CertificateResult::default,
    )
    .expect_err("invalid CA");

    assert!(matches!(
        error,
        BuildCustomCaTransportError::InvalidCaFile { .. }
    ));
}
