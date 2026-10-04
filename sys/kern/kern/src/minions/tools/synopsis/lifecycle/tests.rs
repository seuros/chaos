use super::*;

#[test]
fn terminal_results_cannot_be_reopened_or_overwritten() {
    for terminal in [
        ResultState::Completed,
        ResultState::Failed,
        ResultState::Cancelled,
    ] {
        let mut job = Job::new(&NormalizedJob {
            id: "job".to_string(),
            message: "work".to_string(),
            agent_type: None,
        });
        assert!(job.apply(SynopsisJobEvent::Start));
        assert!(job.apply(match terminal {
            ResultState::Completed => SynopsisJobEvent::Complete,
            ResultState::Failed => SynopsisJobEvent::Fail,
            ResultState::Cancelled => SynopsisJobEvent::Cancel,
            _ => unreachable!(),
        }));
        assert!(!job.apply(SynopsisJobEvent::Start));
        assert!(!job.apply(SynopsisJobEvent::Complete));
        assert!(!job.apply(SynopsisJobEvent::Fail));
        assert!(!job.apply(SynopsisJobEvent::Cancel));
        assert_eq!(job.snapshot().state, terminal);
    }
    let mut job = Job::new(&NormalizedJob {
        id: "job".to_string(),
        message: "work".to_string(),
        agent_type: None,
    });
    assert!(job.apply(SynopsisJobEvent::Fail));
    assert_eq!(job.snapshot().state, ResultState::Failed);
}
