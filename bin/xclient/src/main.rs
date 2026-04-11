//! `chaos-xclient` binary entry — launches the iced application.
//!
//! Kept minimal: all real work lives in [`chaos_xclient`]. This file exists
//! so `cargo run -p chaos-xclient` produces a runnable GUI.

fn main() -> anyhow::Result<()> {
    chaos_xclient::run()
}
