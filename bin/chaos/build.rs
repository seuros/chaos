use std::time::SystemTime;

fn main() {
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    println!("cargo::rerun-if-env-changed=CHAOS_BUILD_TS");
    println!("cargo::rustc-env=CHAOS_BUILD_TS={ts}");
}
