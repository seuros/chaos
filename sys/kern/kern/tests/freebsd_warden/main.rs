//! Explicit live fixture, not an automatically privileged test.
//! Build with `cargo test -p chaos-kern --test freebsd_warden --no-run`.
//! Invoke through the production Warden launcher as a non-root user:
//! `alcatraz --warden-launch --user USER -- ABS_FIXTURE --run-live ABS_HELPER`.
//! Requires an already-loaded ABI-1 module. Never loads one or invokes sudo.

#[cfg(target_os = "freebsd")]
mod cases;

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    // This executable has no automatically runnable tests. Cargo/nextest use
    // libtest's --list protocol for discovery, so stdout must remain empty.
    // Handle discovery before platform checks or any live capability setup.
    if args.iter().any(|arg| arg == "--list") {
        return Ok(());
    }
    run(args)
}

#[cfg(target_os = "freebsd")]
fn run(args: Vec<std::ffi::OsString>) -> anyhow::Result<()> {
    use anyhow::ensure;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    if args.first().is_none_or(|arg| arg != "--run-live") {
        println!("freebsd_warden: skipped (requires explicit --run-live and Warden bootstrap)");
        return Ok(());
    }
    ensure!(args.len() == 2, "expected --run-live ABS_HELPER");
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "fixture must run as non-root"
    );
    let helper = PathBuf::from(&args[1]);
    ensure!(helper.is_absolute() && helper.is_file(), "invalid helper");
    // SAFETY: this custom test main runs before any runtime/worker threads or
    // subprocesses. The production launcher uniquely transfers the capability.
    unsafe { alcatraz::warden_client::initialize()? };
    let home = Arc::new(
        tempfile::Builder::new()
            .prefix("chaos-warden-home-")
            .tempdir_in("/tmp")?,
    );
    // SAFETY: still before runtime threads. Config-only overrides do not
    // isolate journald's process-wide discovery of the user's real daemon.
    unsafe { std::env::set_var("CHAOS_HOME", home.path()) };
    chaos_kern::test_support::bootstrap_sqlite(home.path())?;
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(std::io::stderr)
        .try_init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(60), cases::run(&helper, home.clone())).await?
    })
}

#[cfg(not(target_os = "freebsd"))]
fn run(_args: Vec<std::ffi::OsString>) -> anyhow::Result<()> {
    println!("freebsd_warden: skipped (FreeBSD only)");
    Ok(())
}
