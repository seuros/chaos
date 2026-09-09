//! Dependency-free fixtures shared by workspace tests.

/// Synthetic model identifier for tests that do not depend on model capabilities.
///
/// This is fixture data, not a claim that the model exists in a provider catalog.
/// Keep capability-specific or distinct-model test cases explicit instead.
pub const TEST_MODEL: &str = "gpt-6-astra";
