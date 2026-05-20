use std::time::SystemTime;

fn main() {
    let ts = std::env::var("CHAOS_BUILD_TS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or_else(
            || match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
                Ok(duration) => duration.as_secs(),
                Err(err) => panic!("system clock is before UNIX_EPOCH: {err}"),
            },
        );
    println!("cargo::rerun-if-env-changed=CHAOS_BUILD_TS");
    println!("cargo::rustc-env=CHAOS_BUILD_TS={ts}");
}
