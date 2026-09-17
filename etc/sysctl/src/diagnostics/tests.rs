use super::format_error_chain;
use pretty_assertions::assert_eq;
use std::io;

#[test]
fn format_error_chain_preserves_io_wrapped_anyhow_contexts() {
    let error = io::Error::other(
        anyhow::anyhow!("permission denied")
            .context("failed to open settings database")
            .context("failed to load settings"),
    );

    assert_eq!(
        format_error_chain(&error),
        "failed to load settings\nCaused by: failed to open settings database\nCaused by: permission denied"
    );
}
