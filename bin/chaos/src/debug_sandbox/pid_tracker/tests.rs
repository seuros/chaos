use super::*;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

#[test]
fn pid_is_alive_detects_current_process() {
    let pid = std::process::id() as i32;
    assert!(pid_is_alive(pid));
}

#[cfg(target_os = "macos")]
#[test]
fn list_child_pids_includes_spawned_child() {
    let mut child = Command::new("/bin/sleep")
        .arg("5")
        .stdin(Stdio::null())
        .spawn()
        .expect("failed to spawn child process");

    let child_pid = child.id() as i32;
    let parent_pid = std::process::id() as i32;

    let mut found = false;
    for _ in 0..100 {
        if list_child_pids(parent_pid).contains(&child_pid) {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let _ = child.kill();
    let _ = child.wait();

    assert!(found, "expected to find child pid {child_pid} in list");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn pid_tracker_collects_spawned_children() {
    let tracker = PidTracker::new(std::process::id() as i32).expect("failed to create tracker");

    let mut child = Command::new("/bin/sleep")
        .arg("0.1")
        .stdin(Stdio::null())
        .spawn()
        .expect("failed to spawn child process");

    let child_pid = child.id() as i32;
    let parent_pid = std::process::id() as i32;

    let _ = child.wait();

    let seen = tracker.stop().await;

    assert!(
        seen.contains(&parent_pid),
        "expected tracker to include parent pid {parent_pid}"
    );
    assert!(
        seen.contains(&child_pid),
        "expected tracker to include child pid {child_pid}"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn pid_tracker_collects_bash_subshell_descendants() {
    let tracker = PidTracker::new(std::process::id() as i32).expect("failed to create tracker");

    let child = Command::new("/bin/bash")
        .arg("-c")
        .arg("(sleep 0.1 & echo $!; wait)")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn bash");

    let output = child.wait_with_output().unwrap().stdout;
    let subshell_pid = String::from_utf8_lossy(&output)
        .trim()
        .parse::<i32>()
        .expect("failed to parse subshell pid");

    let seen = tracker.stop().await;

    assert!(
        seen.contains(&subshell_pid),
        "expected tracker to include subshell pid {subshell_pid}"
    );
}
