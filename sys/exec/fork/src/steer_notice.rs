//! Host notices for `chaos exec --steer-file`.
//!
//! A host that drives `chaos exec` (for example a hosting shim) has no other
//! way to speak to a running turn: the prompt arrives once on stdin. The host
//! keeps at most one notice in the steer file, `{"id": ..., "text": ...,
//! "expires_at": unix_secs}`, replacing it by rename and deleting it to
//! withdraw it. Exec mirrors the file into the session's host-notice slot:
//! a changed file replaces the notice, a missing or expired one withdraws it.
//! The kernel checks expiry again at the delivery boundary and delivers each
//! notice id at most once, so a notice that expires or is withdrawn while a
//! long tool call runs never reaches the model, and repeats never pile up.

use chaos_kern::host_notice::HostNotice;
use serde::Deserialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::path::Path;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

#[derive(Deserialize)]
struct NoticeFile {
    #[serde(default)]
    id: Option<String>,
    text: String,
    #[serde(default)]
    expires_at: Option<f64>,
}

/// What the steer file currently says, as a notice the slot should hold.
pub(crate) fn read(path: &Path, now_unix: f64) -> Option<HostNotice> {
    parse(&std::fs::read_to_string(path).ok()?, now_unix)
}

pub(crate) fn parse(raw: &str, now_unix: f64) -> Option<HostNotice> {
    let file: NoticeFile = serde_json::from_str(raw.trim()).ok()?;
    if file
        .expires_at
        .is_some_and(|expires_at| expires_at <= now_unix)
    {
        return None;
    }
    let text = file.text.trim();
    if text.is_empty() {
        return None;
    }
    let id = file.id.unwrap_or_else(|| {
        let mut hasher = DefaultHasher::new();
        raw.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    });
    Some(HostNotice {
        id,
        text: text.to_string(),
        expires_at: file.expires_at,
    })
}

/// Mirrors the steer file into the process's host-notice slot, calling the
/// setter only when what the file says has changed.
#[derive(Default)]
pub(crate) struct Mirror {
    last: Option<Option<HostNotice>>,
}

impl Mirror {
    pub(crate) fn observe(&mut self, current: Option<HostNotice>) -> Option<Option<HostNotice>> {
        if self.last.as_ref() == Some(&current) {
            return None;
        }
        self.last = Some(current.clone());
        Some(current)
    }
}

pub(crate) fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_a_live_notice_with_its_id_and_expiry() {
        let notice = parse(
            r#"{"id":"run-2","text":"  post an update  ","expires_at":200}"#,
            100.0,
        )
        .expect("live notice");
        assert_eq!(notice.id, "run-2");
        assert_eq!(notice.text, "post an update");
        assert_eq!(notice.expires_at, Some(200.0));
    }

    #[test]
    fn drops_expired_empty_and_malformed_notices() {
        assert_eq!(parse(r#"{"text":"late","expires_at":100}"#, 100.0), None);
        assert_eq!(parse(r#"{"text":"   "}"#, 100.0), None);
        assert_eq!(parse("not json", 100.0), None);
    }

    #[test]
    fn a_notice_without_an_id_gets_a_stable_one_from_its_content() {
        let a = parse(r#"{"text":"hello"}"#, 0.0).expect("notice");
        let b = parse(r#"{"text":"hello"}"#, 0.0).expect("notice");
        let c = parse(r#"{"text":"hello again"}"#, 0.0).expect("notice");
        assert_eq!(a.id, b.id);
        assert!(a.id != c.id);
    }

    #[test]
    fn reading_leaves_the_file_for_the_host_to_replace_or_withdraw() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("steer.json");
        assert_eq!(read(&path, 0.0), None);
        std::fs::write(&path, r#"{"id":"a","text":"hello"}"#).expect("write notice");
        assert_eq!(
            read(&path, 0.0).map(|notice| notice.text),
            Some("hello".into())
        );
        assert!(path.exists());
    }

    #[test]
    fn the_mirror_only_reports_changes() {
        let mut mirror = Mirror::default();
        let notice = parse(r#"{"id":"a","text":"hello"}"#, 0.0);
        assert_eq!(mirror.observe(None), Some(None));
        assert_eq!(mirror.observe(None), None);
        assert_eq!(mirror.observe(notice.clone()), Some(notice.clone()));
        assert_eq!(mirror.observe(notice), None);
        assert_eq!(mirror.observe(None), Some(None));
    }
}
