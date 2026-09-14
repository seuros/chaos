use super::CredentialStoreError;
use super::KeyringStore;
use keyring_core::Error as KeyringError;
use keyring_core::mock;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;

/// Mock keyring store backed by `keyring_core::mock::Store`.
///
/// Sets the mock store as the default credential store on creation,
/// then uses `Entry` which dispatches to the mock automatically.
#[derive(Clone, Debug)]
pub struct MockKeyringStore {
    _store: Arc<mock::Store>,
    /// Track which accounts have been touched (for `contains()`).
    accounts: Arc<Mutex<std::collections::HashSet<String>>>,
    values: Arc<Mutex<HashMap<String, String>>>,
    errors: Arc<Mutex<HashMap<String, String>>>,
}

impl Default for MockKeyringStore {
    #[allow(clippy::expect_used)]
    fn default() -> Self {
        let store = mock::Store::new().expect("mock store");
        // Register as default so Entry::new() uses the mock.
        keyring_core::set_default_store(store.clone());
        Self {
            _store: store,
            accounts: Arc::new(Mutex::new(std::collections::HashSet::new())),
            values: Arc::new(Mutex::new(HashMap::new())),
            errors: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl MockKeyringStore {
    pub fn saved_value(&self, account: &str) -> Option<String> {
        self.values
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(account)
            .cloned()
    }

    pub fn set_error(&self, account: &str, error: KeyringError) {
        self.errors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(account.to_string(), error.to_string());
    }

    pub fn contains(&self, account: &str) -> bool {
        let guard = self.accounts.lock().unwrap_or_else(PoisonError::into_inner);
        guard.contains(account)
    }
}

impl KeyringStore for MockKeyringStore {
    fn load(&self, _service: &str, account: &str) -> Result<Option<String>, CredentialStoreError> {
        if let Some(message) = self
            .errors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(account)
            .cloned()
        {
            return Err(CredentialStoreError::new(KeyringError::Invalid(
                "mock".to_string(),
                message,
            )));
        }
        Ok(self.saved_value(account))
    }

    fn save(&self, _service: &str, account: &str, value: &str) -> Result<(), CredentialStoreError> {
        if let Some(message) = self
            .errors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(account)
            .cloned()
        {
            return Err(CredentialStoreError::new(KeyringError::Invalid(
                "mock".to_string(),
                message,
            )));
        }
        self.accounts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(account.to_string());
        self.values
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(account.to_string(), value.to_string());
        Ok(())
    }

    fn delete(&self, _service: &str, account: &str) -> Result<bool, CredentialStoreError> {
        if let Some(message) = self
            .errors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(account)
            .cloned()
        {
            return Err(CredentialStoreError::new(KeyringError::Invalid(
                "mock".to_string(),
                message,
            )));
        }
        self.accounts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(account);
        Ok(self
            .values
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(account)
            .is_some())
    }
}
