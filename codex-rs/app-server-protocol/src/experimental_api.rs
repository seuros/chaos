use std::collections::BTreeMap;
use std::collections::HashMap;

/// Marker trait for protocol types that can signal experimental usage.
pub trait ExperimentalApi {
    /// Returns a short reason identifier when an experimental method or field is
    /// used, or `None` when the value is entirely stable.
    fn experimental_reason(&self) -> Option<&'static str>;
}

/// Describes an experimental field on a specific type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExperimentalField {
    pub type_name: &'static str,
    pub field_name: &'static str,
    /// Stable identifier returned when this field is used.
    /// Convention: `<method>` for method-level gates or `<method>.<field>` for
    /// field-level gates.
    pub reason: &'static str,
}

inventory::collect!(ExperimentalField);

/// Returns all experimental fields registered across the protocol types.
pub fn experimental_fields() -> Vec<&'static ExperimentalField> {
    inventory::iter::<ExperimentalField>.into_iter().collect()
}

/// Constructs a consistent error message for experimental gating.
pub fn experimental_required_message(reason: &str) -> String {
    format!("{reason} requires experimentalApi capability")
}

impl<T: ExperimentalApi> ExperimentalApi for Option<T> {
    fn experimental_reason(&self) -> Option<&'static str> {
        self.as_ref().and_then(ExperimentalApi::experimental_reason)
    }
}

impl<T: ExperimentalApi> ExperimentalApi for Vec<T> {
    fn experimental_reason(&self) -> Option<&'static str> {
        self.iter().find_map(ExperimentalApi::experimental_reason)
    }
}

impl<K, V: ExperimentalApi, S> ExperimentalApi for HashMap<K, V, S> {
    fn experimental_reason(&self) -> Option<&'static str> {
        self.values().find_map(ExperimentalApi::experimental_reason)
    }
}

impl<K: Ord, V: ExperimentalApi> ExperimentalApi for BTreeMap<K, V> {
    fn experimental_reason(&self) -> Option<&'static str> {
        self.values().find_map(ExperimentalApi::experimental_reason)
    }
}

