//! Runtime steer notices for `chaos exec --steer-file`.
//!
//! A host that drives `chaos exec` (for example a hosting shim) has no other
//! way to speak to a running turn: the prompt arrives once on stdin. The host
//! writes a small JSON notice to the steer file, replacing it by rename so a
//! reader never sees a partial write. Exec polls the path, claims the notice by
//! renaming it away (so it is delivered at most once), drops it if it has
//! expired, and steers its text into the active turn without cancelling it.

use serde::Deserialize;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SteerNotice {
    pub(crate) text: String,
}

#[derive(Deserialize)]
struct NoticeFile {
    text: String,
    /// Unix seconds after which the notice is stale and must not be delivered.
    #[serde(default)]
    expires_at: Option<f64>,
}

/// Claim and parse the notice at `path`, if one is waiting.
pub(crate) fn claim(path: &Path, now_unix: f64) -> Option<SteerNotice> {
    let claimed = claimed_path(path);
    std::fs::rename(path, &claimed).ok()?;
    let raw = std::fs::read_to_string(&claimed);
    let _ = std::fs::remove_file(&claimed);
    parse(&raw.ok()?, now_unix)
}

pub(crate) fn parse(raw: &str, now_unix: f64) -> Option<SteerNotice> {
    let file: NoticeFile = serde_json::from_str(raw.trim()).ok()?;
    if file
        .expires_at
        .is_some_and(|expires_at| expires_at <= now_unix)
    {
        return None;
    }
    let text = file.text.trim();
    (!text.is_empty()).then(|| SteerNotice {
        text: text.to_string(),
    })
}

pub(crate) fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0)
}

fn claimed_path(path: &Path) -> PathBuf {
    let mut claimed = OsString::from(path.as_os_str());
    claimed.push(format!(".claimed-{}", std::process::id()));
    PathBuf::from(claimed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_live_notice() {
        assert_eq!(
            parse(r#"{"text":"  post an update  ","expires_at":200}"#, 100.0),
            Some(SteerNotice {
                text: "post an update".to_string()
            })
        );
    }

    #[test]
    fn drops_expired_empty_and_malformed_notices() {
        assert_eq!(parse(r#"{"text":"late","expires_at":100}"#, 100.0), None);
        assert_eq!(parse(r#"{"text":"   "}"#, 100.0), None);
        assert_eq!(parse("not json", 100.0), None);
    }

    #[test]
    fn claim_delivers_once_and_removes_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("steer.json");
        std::fs::write(&path, r#"{"text":"hello"}"#).expect("write notice");

        assert_eq!(
            claim(&path, 0.0),
            Some(SteerNotice {
                text: "hello".to_string()
            })
        );
        assert_eq!(claim(&path, 0.0), None);
        assert_eq!(std::fs::read_dir(dir.path()).expect("read dir").count(), 0);
    }

    #[test]
    fn claim_without_a_file_is_quiet() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(claim(&dir.path().join("absent.json"), 0.0), None);
    }
}
