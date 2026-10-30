#![cfg(unix)]
use super::*;
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::test]
async fn termination_reaps_the_owned_group_without_signalling_the_caller() {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "printf 'ready\\n'; read line"]);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    let mut child = NativeChild::spawn(&mut command).unwrap();
    let group = child.group.unwrap();
    // SAFETY: getpgrp only observes our own group.
    assert_ne!(group, unsafe { libc::getpgrp() });
    let mut lines = BufReader::new(child.child().stdout.take().unwrap()).lines();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), lines.next_line())
            .await
            .unwrap()
            .unwrap(),
        Some("ready".into())
    );
    child.terminate().await.unwrap();
    assert!(!group_exists(group).unwrap());
    assert!(child.child().try_wait().unwrap().is_some());
    child.terminate().await.unwrap();
}
