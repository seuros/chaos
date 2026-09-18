use super::ProcessNameSource;

#[test]
fn legacy_or_unknown_process_name_sources_are_user_pinned() {
    assert_eq!(ProcessNameSource::from_db(None), ProcessNameSource::User);
    assert_eq!(
        ProcessNameSource::from_db(Some("unknown")),
        ProcessNameSource::User
    );
    assert_eq!(
        ProcessNameSource::from_db(Some("agent")),
        ProcessNameSource::Agent
    );
}
