use super::*;
use pretty_assertions::assert_eq;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct FakeEnvironment {
    pub(super) vars: HashMap<String, String>,
    #[cfg(feature = "tui")]
    pub(super) tmux_client_info: emulator::TmuxClientInfo,
}

impl FakeEnvironment {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn with_var(mut self, key: &str, value: &str) -> Self {
        self.vars.insert(key.to_string(), value.to_string());
        self
    }
}

impl Environment for FakeEnvironment {
    fn var(&self, name: &str) -> Option<String> {
        self.vars.get(name).cloned()
    }
}

#[test]
fn detects_multiplexer_pane_ids() {
    let env = FakeEnvironment::new()
        .with_var("TMUX", "/tmp/tmux-1000/default,123,0")
        .with_var("TMUX_PANE", "%3");
    assert_eq!(
        detect_multiplexer(&env),
        Some(Multiplexer::Tmux {
            version: None,
            pane: Some("%3".to_string()),
        })
    );

    let env = FakeEnvironment::new()
        .with_var("ZELLIJ", "0")
        .with_var("ZELLIJ_PANE_ID", "terminal_3");
    let zellij = detect_multiplexer(&env).expect("zellij");
    assert_eq!((zellij.kind(), zellij.id()), ("zellij", Some("terminal_3")));

    let env = FakeEnvironment::new().with_var("STY", "12345.pts-0.host");
    let screen = detect_multiplexer(&env).expect("screen");
    assert_eq!(
        (screen.kind(), screen.id()),
        ("screen", Some("12345.pts-0.host"))
    );
}

#[test]
fn multiplexer_precedence_is_tmux_then_zellij_then_screen() {
    let env = FakeEnvironment::new()
        .with_var("STY", "1.pts-0.host")
        .with_var("ZELLIJ", "0")
        .with_var("TMUX_PANE", "%1");
    assert_eq!(detect_multiplexer(&env).map(|mux| mux.kind()), Some("tmux"));

    let env = FakeEnvironment::new()
        .with_var("STY", "1.pts-0.host")
        .with_var("ZELLIJ", "0");
    assert_eq!(
        detect_multiplexer(&env).map(|mux| mux.kind()),
        Some("zellij")
    );

    let env = FakeEnvironment::new()
        .with_var("TMUX", " ")
        .with_var("STY", "");
    assert_eq!(detect_multiplexer(&env), None);
}

#[test]
fn detects_ssh_session() {
    for var in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"] {
        let env = FakeEnvironment::new().with_var(var, "10.0.0.1 22 10.0.0.2 22");
        assert!(detect_ssh_session(&env), "{var}");
    }
    assert!(!detect_ssh_session(&FakeEnvironment::new()));
    assert!(!detect_ssh_session(
        &FakeEnvironment::new().with_var("SSH_TTY", "")
    ));
}

#[test]
fn multiplexer_serializes_with_kind_tag() {
    let mux = Multiplexer::Screen {
        session: Some("1.pts-0.host".to_string()),
    };
    assert_eq!(
        serde_json::to_value(&mux).unwrap(),
        serde_json::json!({"kind": "screen", "session": "1.pts-0.host"})
    );
}
