/// Canonical product name exposed across the ChaOS workspace.
pub const PRODUCT_NAME: &str = "Chaos";

/// Canonical product version embedded at compile time for workspace crates.
pub const CHAOS_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Shared product/build identity used by UI and CLI surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductInfo {
    pub name: &'static str,
    pub version: &'static str,
}

/// Canonical product/build identity for this binary set.
pub const PRODUCT_INFO: ProductInfo = ProductInfo {
    name: PRODUCT_NAME,
    version: CHAOS_VERSION,
};

/// Returns the canonical product display name.
pub const fn display_name() -> &'static str {
    PRODUCT_INFO.name
}

/// Returns the canonical version badge text for a given version.
pub fn version_badge_for(version: &str) -> String {
    format!("(v{version})")
}

/// Returns the canonical version badge text for the current product version.
pub fn version_badge() -> String {
    version_badge_for(PRODUCT_INFO.version)
}

/// Returns the canonical display string including the current version.
pub fn display_name_with_version() -> String {
    format!("{} {}", PRODUCT_INFO.name, version_badge())
}
