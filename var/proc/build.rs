fn main() {
    // `sqlx::migrate!` embeds the migration set at compile time. Watching the
    // directories explicitly ensures that adding a new migration invalidates
    // cached chaos-proc artifacts, not only edits to files Cargo already knew.
    println!("cargo:rerun-if-changed=db/migrate/sqlite");
    println!("cargo:rerun-if-changed=db/migrate/postgres");
}
