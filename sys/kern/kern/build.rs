#[path = "build_support/manual_catalog.rs"]
mod manual_catalog;

fn main() {
    let crate_dir = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"),
    );
    let manual_dir = crate_dir.join("../../../man");
    // Watch the directory, not just today's files: adding/removing a page must
    // regenerate the catalog too.
    println!("cargo:rerun-if-changed={}", manual_dir.display());
    println!("cargo:rerun-if-changed=build_support/manual_catalog.rs");
    let pages = manual_catalog::read_catalog(&manual_dir)
        .unwrap_or_else(|error| panic!("invalid manual catalog: {error}"));
    let out_dir =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output directory"));
    std::fs::write(
        out_dir.join("manual_pages.rs"),
        manual_catalog::render_catalog(&pages),
    )
    .expect("write embedded manual catalog");
}
