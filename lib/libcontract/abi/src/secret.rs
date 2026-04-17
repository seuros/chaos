//! Redacted newtype for credentials.
//!
//! `Debug` prints `Secret("***")`, `Display` prints `***`, `Serialize`
//! refuses. Call [`Secret::expose`] to reach the plaintext.

use std::fmt;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;

/// A value that must never be displayed verbatim.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret<T> {
    inner: T,
}

impl<T> Secret<T> {
    pub fn new(value: T) -> Self {
        Self { inner: value }
    }

    pub fn expose(&self) -> &T {
        &self.inner
    }

    pub fn into_inner(self) -> T {
        self.inner
    }
}

impl<T> From<T> for Secret<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Secret").field(&"***").finish()
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl<T> Serialize for Secret<T> {
    fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("refusing to serialize Secret<_>"))
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Secret<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        T::deserialize(d).map(Self::new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_redacts_but_deserializes_and_exposes() {
        let s: Secret<String> = Secret::new("sk-ant-abcdef".into());

        assert_eq!(format!("{s}"), "***");
        assert_eq!(format!("{s:?}"), "Secret(\"***\")");
        assert_eq!(s.expose(), "sk-ant-abcdef");

        assert!(serde_json::to_string(&s).is_err());

        let loaded: Secret<String> = serde_json::from_str("\"hunter2\"").unwrap();
        assert_eq!(loaded.expose(), "hunter2");
        assert_eq!(format!("{loaded:?}"), "Secret(\"***\")");
    }
}
