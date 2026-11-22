use super::*;
use std::ffi::OsStr;
use std::time::Duration;

fn env_of(cmd: &Command) -> Vec<(String, Option<String>)> {
    cmd.as_std()
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value
                    .map(OsStr::to_string_lossy)
                    .map(std::borrow::Cow::into_owned),
            )
        })
        .collect()
}

#[test]
fn strips_parent_session_and_owns_resume_and_stream_timeout() {
    let mut cmd = Command::new("claude");
    apply(
        &mut cmd,
        &["CHAOS_DAEMON_ONLY".to_owned()],
        Some(Duration::from_secs(600)),
    );
    let env = env_of(&cmd);
    for name in INHERITED_SESSION_ENV {
        assert!(
            env.contains(&((*name).to_owned(), None)),
            "{name} must be removed"
        );
    }
    assert!(env.contains(&(
        "CLAUDE_CODE_DISABLE_DEFERRED_TOOL_RESUME".to_owned(),
        Some("1".to_owned())
    )));
    assert!(env.contains(&(
        "CLAUDE_STREAM_IDLE_TIMEOUT_MS".to_owned(),
        Some("600000".to_owned())
    )));
}

#[test]
fn leaves_the_stream_timeout_alone_when_chaos_has_none() {
    let mut cmd = Command::new("claude");
    apply(&mut cmd, &[], None);
    assert!(
        !env_of(&cmd)
            .iter()
            .any(|(key, _)| key == "CLAUDE_STREAM_IDLE_TIMEOUT_MS")
    );
}

#[test]
fn a_forbidden_name_stays_removed_even_when_set_earlier() {
    let mut cmd = Command::new("claude");
    cmd.env("ANTHROPIC_BASE_URL", "http://127.0.0.1:1");
    let forbidden = [
        "ANTHROPIC_BASE_URL".to_owned(),
        "CLAUDE_STREAM_IDLE_TIMEOUT_MS".to_owned(),
    ];
    apply(&mut cmd, &forbidden, Some(Duration::from_secs(600)));
    let env = env_of(&cmd);
    for name in &forbidden {
        assert!(
            env.contains(&(name.clone(), None)),
            "{name} must stay removed"
        );
    }
}
