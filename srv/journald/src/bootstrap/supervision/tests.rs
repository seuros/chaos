use std::time::Duration;

use super::HISTORY_FILENAME;
use super::Verdict;
use super::decide;
use super::read_history;
use super::write_history;

const SECOND: Duration = Duration::from_secs(1);

#[test]
fn sixth_restart_within_the_window_is_refused() {
    let mut history = Vec::new();
    for n in 0..5_u32 {
        let decision = decide(history.clone(), n * SECOND).expect("decision");
        assert!(
            matches!(decision.verdict, Verdict::Spawn { .. }),
            "restart {n}"
        );
        history = decision.history;
    }
    assert_eq!(history.len(), 5);

    let refused = decide(history.clone(), 5 * SECOND).expect("decision");
    assert_eq!(refused.verdict, Verdict::Refuse { recent: 5 });
    assert_eq!(refused.history, history);
}

#[test]
fn history_from_another_process_counts() {
    let foreign: Vec<Duration> = (1..=5_u32).map(|n| n * SECOND).collect();
    let decision = decide(foreign, 10 * SECOND).expect("decision");
    assert_eq!(decision.verdict, Verdict::Refuse { recent: 5 });
}

#[test]
fn history_file_round_trips_and_tolerates_absence() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join(HISTORY_FILENAME);
    assert_eq!(
        read_history(&path).expect("missing file reads empty"),
        Vec::<Duration>::new()
    );

    let history = vec![SECOND, 2 * SECOND, 3 * SECOND];
    write_history(&path, &history).expect("write history");
    assert_eq!(read_history(&path).expect("read history"), history);
    assert_eq!(
        std::fs::read_to_string(&path).expect("raw file"),
        "1000000000\n2000000000\n3000000000\n"
    );
}
