use super::*;

#[test]
fn ghostty_tmux_and_remote_transports_are_probed() {
    assert!(should_probe(TerminalName::Ghostty, false, false));
    assert!(should_probe(TerminalName::Ghostty, true, false));
    assert!(should_probe(TerminalName::Unknown, true, false));
    assert!(should_probe(TerminalName::Unknown, false, true));
    assert!(!should_probe(TerminalName::AppleTerminal, false, false));
    assert!(!should_probe(TerminalName::Dumb, false, false));
}

#[test]
fn remote_policy_uses_inline_data_and_allows_more_round_trip_time() {
    let local = query_options(false);
    let remote = query_options(true);
    assert!(local.kitty_shared_memory_object.is_none());
    assert!(remote.kitty_shared_memory_object.is_none());
    assert!(remote.kitty_compression);
    assert!(remote.timeout_ms > local.timeout_ms);
}

#[test]
fn tmux_and_ssh_require_reported_capabilities_not_environment_hints() {
    assert!(!protocol_confirmed(ProtocolType::Kitty, &[]));
    assert!(protocol_confirmed(
        ProtocolType::Kitty,
        &[Capability::Kitty]
    ));
    assert!(!protocol_confirmed(ProtocolType::Iterm2, &[]));
    assert!(!protocol_confirmed(
        ProtocolType::Halfblocks,
        &[Capability::Kitty]
    ));
}

#[test]
fn late_tmux_replies_are_drained_until_input_goes_quiet() {
    use std::io::Write;
    use std::os::fd::{AsFd, OwnedFd};

    let mut fds = [0; 2];
    // Safety: fds has room for the two descriptors pipe() returns.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // Safety: pipe() returned two fresh descriptors we now own.
    let (reader, writer) = unsafe {
        use std::os::fd::FromRawFd;
        (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1]))
    };
    let replies = b"\x1b_Gi=31;OK\x1b\\\x1b_Gi=32;OK\x1b\\";
    std::fs::File::from(writer.try_clone().unwrap())
        .write_all(replies)
        .unwrap();

    let quiet = Duration::from_millis(20);
    let limit = Duration::from_millis(200);
    assert_eq!(drain_late_replies(reader.as_fd(), quiet, limit), replies.len());
    assert_eq!(drain_late_replies(reader.as_fd(), quiet, limit), 0);
    drop(writer);
}
