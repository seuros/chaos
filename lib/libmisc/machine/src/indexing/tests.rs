use super::*;
use serde_json::json;

pub(super) fn output(text: &str) -> crate::command::Output {
    crate::command::Output {
        success: true,
        stdout: text.as_bytes().to_vec(),
        stderr: Vec::new(),
    }
}

#[test]
fn serialization_preserves_failures_separately_from_empty_measurements() {
    for (observation, expected) in [
        (
            IndexingObservation::Available {
                value: Vec::<IndexerProcess>::new(),
            },
            json!({"status": "available", "value": []}),
        ),
        (
            IndexingObservation::Unavailable {
                reason: "missing".into(),
            },
            json!({"status": "unavailable", "reason": "missing"}),
        ),
        (
            IndexingObservation::PermissionDenied,
            json!({"status": "permission_denied"}),
        ),
        (
            IndexingObservation::TimedOut,
            json!({"status": "timed_out"}),
        ),
        (
            IndexingObservation::Unsupported,
            json!({"status": "unsupported"}),
        ),
    ] {
        assert_eq!(serde_json::to_value(observation).unwrap(), expected);
    }
    assert_eq!(
        serde_json::to_value(IndexingObservation::Available {
            value: SpotlightState::Disabled
        })
        .unwrap(),
        json!({"status": "available", "value": "disabled"})
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn other_platforms_do_not_guess_spotlight_status() {
    let snapshot = inspect_indexing(Path::new("/workspace"));
    assert_eq!(snapshot.spotlight, IndexingObservation::Unsupported);
    assert_ne!(snapshot.processes, IndexingObservation::Unsupported);
    assert!(snapshot.volume.is_none());
}
