fn main() {
    println!("cargo:rerun-if-changed=db/migrate/postgres");
}
