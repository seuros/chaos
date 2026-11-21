use std::fs;
use std::path::Path;

use age::secrecy::ExposeSecret;
use age::secrecy::SecretString;
use anyhow::Context;
use anyhow::Result;
use custody_machines::Refusal;
use custody_machines::Vault;
use custody_machines::stores::CredentialsDir;
use custody_machines::stores::Custodian;
use custody_machines::stores::CustodyError;
use custody_machines::stores::Secret;
use custody_machines::stores::Source;

#[cfg(not(target_os = "macos"))]
use custody_machines::stores::KeyFile;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
use custody_machines::stores::KeyringSource;

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
use super::KEYRING_SERVICE;
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "freebsd"))]
use super::compute_keyring_account;

const CUSTODY_DOMAIN: &str = "chaos/vault-key/v1";
const CREDENTIAL_NAME: &str = "chaos-vault-key";
#[cfg(not(target_os = "macos"))]
const KEY_FILENAME: &str = "vault.key";

#[cfg(target_os = "macos")]
pub const LADDER: usize = 2;
#[cfg(target_os = "linux")]
pub const LADDER: usize = 4;
#[cfg(target_os = "freebsd")]
pub const LADDER: usize = 3;
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "freebsd")))]
pub const LADDER: usize = 2;

/// Where the vault key may live, most preferred first.
pub type VaultSources = [Box<dyn Source>; LADDER];

pub(crate) type VaultCustodian = Custodian<LADDER>;

pub(crate) struct Unlocked {
    pub passphrase: SecretString,
    /// Why data may not be sealed under this key, when its only copy dies on reboot.
    pub volatile: Option<String>,
}

pub(crate) fn custodian(sources: VaultSources) -> Result<VaultCustodian> {
    Custodian::new(CUSTODY_DOMAIN, sources).context("the vault key ladder has no durable source")
}

#[cfg(target_os = "macos")]
pub(crate) fn platform_sources(chaos_home: &Path) -> VaultSources {
    let account = compute_keyring_account(chaos_home);
    [
        Box::new(CredentialsDir::from_env(CREDENTIAL_NAME)),
        Box::new(KeyringSource::keychain(KEYRING_SERVICE, account)),
    ]
}

#[cfg(target_os = "linux")]
pub(crate) fn platform_sources(chaos_home: &Path) -> VaultSources {
    let account = compute_keyring_account(chaos_home);
    [
        Box::new(CredentialsDir::from_env(CREDENTIAL_NAME)),
        Box::new(KeyringSource::secret_service(
            KEYRING_SERVICE,
            account.clone(),
        )),
        Box::new(KeyFile::new(chaos_home.join("secrets").join(KEY_FILENAME))),
        Box::new(KeyringSource::keyutils(KEYRING_SERVICE, account)),
    ]
}

#[cfg(target_os = "freebsd")]
pub(crate) fn platform_sources(chaos_home: &Path) -> VaultSources {
    let account = compute_keyring_account(chaos_home);
    [
        Box::new(CredentialsDir::from_env(CREDENTIAL_NAME)),
        Box::new(KeyringSource::secret_service(KEYRING_SERVICE, account)),
        Box::new(KeyFile::new(chaos_home.join("secrets").join(KEY_FILENAME))),
    ]
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "freebsd")))]
pub(crate) fn platform_sources(chaos_home: &Path) -> VaultSources {
    [
        Box::new(CredentialsDir::from_env(CREDENTIAL_NAME)),
        Box::new(KeyFile::new(chaos_home.join("secrets").join(KEY_FILENAME))),
    ]
}

/// Open the vault key, or mint one when no vault exists yet. Over an existing
/// vault, a key is proven by decrypting `secrets_path` whenever sources
/// disagree or one is silent.
pub(crate) fn unlock(
    custodian: &VaultCustodian,
    secrets_path: &Path,
    create: bool,
    generate: impl FnOnce() -> Result<SecretString>,
    decrypts: impl Fn(&[u8], &SecretString) -> bool,
) -> Result<Unlocked> {
    let sealed = if create {
        None
    } else {
        Some(fs::read(secrets_path).with_context(|| {
            format!("failed to read secrets file at {}", secrets_path.display())
        })?)
    };
    let opens = |key: &[u8]| {
        sealed.as_deref().is_some_and(|ciphertext| {
            passphrase_of(key).is_ok_and(|passphrase| decrypts(ciphertext, &passphrase))
        })
    };
    let key = if create {
        let fresh = generate()?;
        let fresh = Secret::new(fresh.expose_secret().as_bytes().to_vec());
        custodian.open_or_mint(Vault::Unsealed, opens, move || fresh)
    } else {
        custodian.open(Vault::Sealed, opens)
    }
    .map_err(|error| anyhow::anyhow!("cannot unlock the credential vault: {error}"))?;
    Ok(Unlocked {
        passphrase: passphrase_of(key.secret())?,
        volatile: key.permit_write().err().map(|error| error.to_string()),
    })
}

/// Before sealing data, survey the sources again: the key in use must still
/// be held there, and not only where a reboot erases it. A source that does
/// not answer (a locked Keychain) contradicts nothing, so the earlier answer
/// stands: refusing then would drop a rotated token the server already
/// replaced. A conclusive answer (the key is gone, different or volatile)
/// replaces it either way. Answers why writes are refused, if they are.
pub(crate) fn recheck(
    custodian: &VaultCustodian,
    in_use: &SecretString,
    earlier: Option<String>,
) -> Option<String> {
    let current = in_use.expose_secret().as_bytes();
    match custodian.open(Vault::Sealed, |key| key == current) {
        Ok(key) if key.secret() != current => {
            Some("its key sources now hold a different key".to_owned())
        }
        Ok(key) => key.permit_write().err().map(|error| error.to_string()),
        Err(CustodyError::Refused {
            refusal: Refusal::Unavailable,
            ..
        }) => earlier,
        Err(error) => Some(error.to_string()),
    }
}

fn passphrase_of(key: &[u8]) -> Result<SecretString> {
    std::str::from_utf8(key)
        .map(|key| SecretString::from(key.to_owned()))
        .map_err(|_| anyhow::anyhow!("the credential vault key is not UTF-8"))
}
