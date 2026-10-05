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
