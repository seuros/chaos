//! Console-facing accessors for canonical ChaOS product/build identity.
//!
//! The source of truth lives in `chaos_ipc::product` so every crate can reuse
//! the same name/version contract. This module only re-exports those values for
//! the TUI and keeps UI-specific code from importing shared build info directly
//! all over the tree.

pub use chaos_ipc::product::CHAOS_VERSION;
pub use chaos_ipc::product::PRODUCT_NAME;
pub use chaos_ipc::product::version_badge;
