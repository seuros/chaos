//! Chaos Contraband — secrets management and credential handling.
//!
//! Handles API keys, tokens, keyring integration, environment variable
//! scrubbing, and encrypted credential storage. Nothing secret should
//! leak through tool output, logs, or telemetry.
