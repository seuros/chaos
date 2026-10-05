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
fn ca_path_uses_non_empty_ssl_cert_file() {
    for (label, value, expected) in [
        (
            "configured path",
            "/tmp/fallback.pem",
            Some(PathBuf::from("/tmp/fallback.pem")),
        ),
        ("empty override", "", None),
    ] {
        let env = map_env(&[(SSL_CERT_FILE_ENV, value)]);
        assert_eq!(
            env.configured_ca_bundle().map(|bundle| bundle.path),
            expected,
            "{label}"
        );
    }
}

#[test]
fn rustls_config_uses_custom_ca_bundle_when_configured() {
    let temp_dir = TempDir::new().expect("tempdir");
    let cert_path = write_cert_file(&temp_dir, "ca.pem", TEST_CERT);
    let env = map_env(&[(SSL_CERT_FILE_ENV, cert_path.to_string_lossy().as_ref())]);

    maybe_build_rustls_client_config_with_env_and_native_roots(
        &env,
        rustls_native_certs::CertificateResult::default,
    )
    .expect("rustls config")
    .expect("custom CA config should be present");
}

#[test]
fn rustls_config_reports_invalid_ca_file() {
    let temp_dir = TempDir::new().expect("tempdir");
    let cert_path = write_cert_file(&temp_dir, "empty.pem", "");
    let env = map_env(&[(SSL_CERT_FILE_ENV, cert_path.to_string_lossy().as_ref())]);

    let error = maybe_build_rustls_client_config_with_env_and_native_roots(
        &env,
        rustls_native_certs::CertificateResult::default,
    )
    .expect_err("invalid CA");

    std::assert_matches!(error, BuildCustomCaTransportError::InvalidCaFile { .. });
}
