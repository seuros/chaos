use super::*;

#[test]
fn sleep_seconds_is_i32_max() {
    assert_eq!(BLOCKER_SLEEP_SECONDS, format!("{}", i32::MAX));

    for release_explicitly in [true, false] {
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();
        let mut inhibitor = LinuxSleepInhibitor::new();
        assert!(
            inhibitor
                .machine
                .handle(InhibitorLifecycleEvent::Acquire(Some(BackendProcess {
                    backend: LinuxBackend::SystemdInhibit,
                    child,
                })))
                .is_ok()
        );
        if release_explicitly {
            inhibitor.release();
            inhibitor.release();
        }
        drop(inhibitor);
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }
}
