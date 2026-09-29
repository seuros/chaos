use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::sync::atomic::compiler_fence;

use age::decrypt;
use age::encrypt;
use age::scrypt::Identity as ScryptIdentity;
use age::scrypt::Recipient as ScryptRecipient;
use age::secrecy::ExposeSecret;
use age::secrecy::SecretString;
use anyhow::Context;
use anyhow::Result;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use chaos_keyring::DefaultKeyringStore;
use chaos_keyring::KeyringStore;
use rand::TryRng;
use rand::rngs::SysRng;
use serde::Deserialize;
use serde::Serialize;
use tracing::warn;
use zeroize::Zeroize;
use zeroize::Zeroizing;

use super::SecretListEntry;
use super::SecretName;
use super::SecretScope;
use super::SecretsBackend;
use super::compute_keyring_account;
use super::keyring_service;

const SECRETS_VERSION: u8 = 2;
const LOCAL_SECRETS_FILENAME: &str = "local.age";

#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct SecretsFile {
    version: u8,
    secrets: BTreeMap<String, String>,
    #[serde(default)]
    // A null record is a deletion tombstone: retrying migration must not
    // resurrect credentials removed by logout or key rotation.
    credentials: BTreeMap<String, Option<String>>,
}

impl SecretsFile {
    fn new_empty() -> Self {
        Self {
            version: SECRETS_VERSION,
            secrets: BTreeMap::new(),
            credentials: BTreeMap::new(),
        }
    }
}

impl Drop for SecretsFile {
    fn drop(&mut self) {
        for value in self
            .secrets
            .values_mut()
            .chain(self.credentials.values_mut().flatten())
        {
            value.zeroize();
        }
    }
}

impl std::fmt::Debug for SecretsFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretsFile")
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct VaultState {
    passphrase: Option<SecretString>,
    snapshot: Option<(Vec<u8>, SecretsFile)>,
}

// The unlock key belongs to the process, not a particular caller. A snapshot is
// reused only while the encrypted bytes on disk are unchanged, so other
// processes' rotations and deletions are observed without another Keychain read.
static VAULTS: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<VaultState>>>>> =
    LazyLock::new(Mutex::default);

#[derive(Clone)]
pub struct LocalSecretsBackend {
    chaos_home: PathBuf,
    keyring_store: Arc<dyn KeyringStore>,
    state: Arc<Mutex<VaultState>>,
}

impl std::fmt::Debug for LocalSecretsBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalSecretsBackend")
            .field("chaos_home", &self.chaos_home)
            .finish_non_exhaustive()
    }
}

impl LocalSecretsBackend {
    /// Open the process-shared vault. No Keychain access until a value is used.
    pub fn shared(chaos_home: PathBuf) -> Self {
        let home = chaos_home.canonicalize().unwrap_or(chaos_home);
        let state = VAULTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(home.clone())
            .or_default()
            .clone();
        Self {
            chaos_home: home,
            keyring_store: Arc::new(DefaultKeyringStore),
            state,
        }
    }

    pub fn new(chaos_home: PathBuf, keyring_store: Arc<dyn KeyringStore>) -> Self {
        Self {
            chaos_home,
            keyring_store,
            state: Arc::new(Mutex::new(VaultState::default())),
        }
    }

    pub fn set(&self, scope: &SecretScope, name: &SecretName, value: &str) -> Result<()> {
        anyhow::ensure!(!value.is_empty(), "secret value must not be empty");
        let canonical_key = scope.canonical_key(name);
        self.update(|file| {
            file.secrets
                .insert(canonical_key, value.to_string())
                .zeroize();
        })
    }

    pub fn get(&self, scope: &SecretScope, name: &SecretName) -> Result<Option<String>> {
        let canonical_key = scope.canonical_key(name);
        let file = self.load_file()?;
        Ok(file.secrets.get(&canonical_key).cloned())
    }

    pub fn delete(&self, scope: &SecretScope, name: &SecretName) -> Result<bool> {
        let canonical_key = scope.canonical_key(name);
        self.update(|file| {
            let mut removed = file.secrets.remove(&canonical_key);
            let existed = removed.is_some();
            removed.zeroize();
            existed
        })
    }

    /// Internal credentials are separate from operator-managed named secrets.
    pub fn load_credential(&self, key: &str) -> Result<Option<String>> {
        Ok(self.load_file()?.credentials.get(key).cloned().flatten())
    }

    /// Includes deletion tombstones, which must not be imported again.
    pub fn has_credential_record(&self, key: &str) -> Result<bool> {
        Ok(self.load_file()?.credentials.contains_key(key))
    }

    pub fn save_credential(&self, key: &str, value: &str) -> Result<()> {
        self.update(|file| {
            file.credentials
                .insert(key.to_owned(), Some(value.to_owned()))
                .zeroize();
        })
    }

    pub fn delete_credential(&self, key: &str) -> Result<bool> {
        self.update(|file| {
            let mut removed = file.credentials.insert(key.to_owned(), None).flatten();
            let existed = removed.is_some();
            removed.zeroize();
            existed
        })
    }

    /// Explicit, retryable migration. Never overwrite a live vault credential.
    pub fn import_credentials(&self, values: &BTreeMap<String, String>) -> Result<()> {
        self.update(|file| {
            for (key, value) in values {
                file.credentials
                    .entry(key.clone())
                    .or_insert_with(|| Some(value.clone()));
            }
        })
    }

    pub fn list(&self, scope_filter: Option<&SecretScope>) -> Result<Vec<SecretListEntry>> {
        let file = self.load_file()?;
        let mut entries = Vec::new();
        for canonical_key in file.secrets.keys() {
            let Some(entry) = parse_canonical_key(canonical_key) else {
                warn!("skipping invalid canonical secret key: {canonical_key}");
                continue;
            };
            if let Some(scope) = scope_filter
                && entry.scope != *scope
            {
                continue;
            }
            entries.push(entry);
        }
        Ok(entries)
    }

    fn secrets_dir(&self) -> PathBuf {
        self.chaos_home.join("secrets")
    }

    fn secrets_path(&self) -> PathBuf {
        self.secrets_dir().join(LOCAL_SECRETS_FILENAME)
    }

    fn load_file(&self) -> Result<SecretsFile> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("vault lock poisoned"))?;
        if !self.secrets_path().exists() {
            state.snapshot = None;
            return Ok(SecretsFile::new_empty());
        }
        let _lock = self.lock_file()?;
        self.read_locked(&mut state)
    }

    fn lock_file(&self) -> Result<fs::File> {
        use std::os::unix::fs::DirBuilderExt;
        use std::os::unix::fs::OpenOptionsExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(self.secrets_dir())?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(self.secrets_dir().join(".lock"))?;
        lock.lock().context("failed to lock credential vault")?;
        Ok(lock)
    }

    fn update<T>(&self, edit: impl FnOnce(&mut SecretsFile) -> T) -> Result<T> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("vault lock poisoned"))?;
        let _lock = self.lock_file()?;
        let mut file = self.read_locked(&mut state)?;
        let before = file.clone();
        let result = edit(&mut file);
        if file != before {
            self.write_locked(&mut state, file)?;
        }
        Ok(result)
    }

    fn read_locked(&self, state: &mut VaultState) -> Result<SecretsFile> {
        let path = self.secrets_path();
        if !path.exists() {
            state.snapshot = None;
            return Ok(SecretsFile::new_empty());
        }

        let ciphertext = fs::read(&path)
            .with_context(|| format!("failed to read secrets file at {}", path.display()))?;
        if let Some((previous, file)) = &state.snapshot
            && previous == &ciphertext
        {
            return Ok(file.clone());
        }
        let passphrase = self.passphrase(state, false)?;
        let plaintext = Zeroizing::new(decrypt_with_passphrase(&ciphertext, &passphrase)?);
        let mut parsed: SecretsFile = serde_json::from_slice(&plaintext)
            .map_err(|_| anyhow::anyhow!("invalid decrypted credential vault contents"))?;
        if parsed.version == 0 {
            parsed.version = SECRETS_VERSION;
        }
        anyhow::ensure!(
            parsed.version <= SECRETS_VERSION,
            "secrets file version {} is newer than supported version {}",
            parsed.version,
            SECRETS_VERSION
        );
        state.snapshot = Some((ciphertext, parsed.clone()));
        Ok(parsed)
    }

    #[cfg(test)]
    fn save_file(&self, file: &SecretsFile) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("vault lock poisoned"))?;
        let _lock = self.lock_file()?;
        self.write_locked(&mut state, file.clone())
    }

    fn write_locked(&self, state: &mut VaultState, mut file: SecretsFile) -> Result<()> {
        let passphrase = self.passphrase(state, !self.secrets_path().exists())?;
        file.version = file.version.max(SECRETS_VERSION);
        let plaintext =
            Zeroizing::new(serde_json::to_vec(&file).context("failed to serialize secrets file")?);
        let ciphertext = encrypt_with_passphrase(&plaintext, &passphrase)?;
        let path = self.secrets_path();
        write_file_atomically(&path, &ciphertext)?;
        state.snapshot = Some((ciphertext, file));
        Ok(())
    }

    fn passphrase(&self, state: &mut VaultState, create: bool) -> Result<SecretString> {
        if let Some(key) = &state.passphrase {
            return Ok(key.clone());
        }
        let account = compute_keyring_account(&self.chaos_home);
        let loaded = self
            .keyring_store
            .load(keyring_service(), &account)
            .map_err(|err| anyhow::anyhow!(err.message()))
            .with_context(|| format!("failed to load secrets key from keyring for {account}"))?;
        let key = match loaded {
            Some(existing) => SecretString::from(existing),
            None => {
                anyhow::ensure!(
                    create,
                    "credential vault unlock key is missing; restore it from backup"
                );
                // Generate a high-entropy key and persist it in the OS keyring.
                // This keeps secrets out of plaintext config while remaining
                // fully local/offline for the MVP.
                let generated = generate_passphrase()?;
                self.keyring_store
                    .save(keyring_service(), &account, generated.expose_secret())
                    .map_err(|err| anyhow::anyhow!(err.message()))
                    .context("failed to persist secrets key in keyring")?;
                let verified = self
                    .keyring_store
                    .load(keyring_service(), &account)
                    .map_err(|err| anyhow::anyhow!(err.message()))?
                    .map(SecretString::from);
                anyhow::ensure!(
                    verified
                        .as_ref()
                        .is_some_and(|key| key.expose_secret() == generated.expose_secret()),
                    "credential vault unlock key failed read-back verification"
                );
                generated
            }
        };
        state.passphrase = Some(key.clone());
        Ok(key)
    }
}

impl SecretsBackend for LocalSecretsBackend {
    fn set(&self, scope: &SecretScope, name: &SecretName, value: &str) -> Result<()> {
        LocalSecretsBackend::set(self, scope, name, value)
    }

    fn get(&self, scope: &SecretScope, name: &SecretName) -> Result<Option<String>> {
        LocalSecretsBackend::get(self, scope, name)
    }

    fn delete(&self, scope: &SecretScope, name: &SecretName) -> Result<bool> {
        LocalSecretsBackend::delete(self, scope, name)
    }

    fn list(&self, scope_filter: Option<&SecretScope>) -> Result<Vec<SecretListEntry>> {
        LocalSecretsBackend::list(self, scope_filter)
    }
}

fn write_file_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path.parent().with_context(|| {
        format!(
            "failed to compute parent directory for secrets file at {}",
            path.display()
        )
    })?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(contents)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .context("failed to replace encrypted credential vault")?;
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

fn generate_passphrase() -> Result<SecretString> {
    let mut bytes = [0_u8; 32];
    let mut rng = SysRng;
    rng.try_fill_bytes(&mut bytes)
        .context("failed to generate random secrets key")?;
    // Base64 keeps the keyring payload ASCII-safe without reducing entropy.
    let encoded = BASE64_STANDARD.encode(bytes);
    wipe_bytes(&mut bytes);
    Ok(SecretString::from(encoded))
}

fn wipe_bytes(bytes: &mut [u8]) {
    for byte in bytes {
        // Volatile writes make it much harder for the compiler to elide the wipe.
        // SAFETY: `byte` is a valid mutable reference into `bytes`.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

fn encrypt_with_passphrase(plaintext: &[u8], passphrase: &SecretString) -> Result<Vec<u8>> {
    let recipient = ScryptRecipient::new(passphrase.clone());
    encrypt(&recipient, plaintext).context("failed to encrypt secrets file")
}

fn decrypt_with_passphrase(ciphertext: &[u8], passphrase: &SecretString) -> Result<Vec<u8>> {
    let identity = ScryptIdentity::new(passphrase.clone());
    decrypt(&identity, ciphertext).context("failed to decrypt secrets file")
}

fn parse_canonical_key(canonical_key: &str) -> Option<SecretListEntry> {
    let mut parts = canonical_key.split('/');
    let scope_kind = parts.next()?;
    match scope_kind {
        "global" => {
            let name = parts.next()?;
            if parts.next().is_some() {
                return None;
            }
            let name = SecretName::new(name).ok()?;
            Some(SecretListEntry {
                scope: SecretScope::Global,
                name,
            })
        }
        "env" => {
            let environment_id = parts.next()?;
            let name = parts.next()?;
            if parts.next().is_some() {
                return None;
            }
            let name = SecretName::new(name).ok()?;
            let scope = SecretScope::environment(environment_id.to_string()).ok()?;
            Some(SecretListEntry { scope, name })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
