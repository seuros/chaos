/// Note that the cwd, env, and command args are preserved in the ultimate call
/// to `execv`, so the caller is responsible for ensuring those values are
/// correct.
fn main() -> ! {
    alcatraz_freebsd::run_main()
}
