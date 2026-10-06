//! Process environment reads shared by every detector.

pub fn var(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            tracing::warn!(env_var = key, "ignoring non-unicode environment value");
            None
        }
    }
}

pub fn var_non_empty(key: &str) -> Option<String> {
    let value = var(key)?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

pub fn is_set(key: &str) -> bool {
    var(key).is_some_and(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests;
