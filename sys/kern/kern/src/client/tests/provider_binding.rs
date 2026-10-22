use super::*;
use pretty_assertions::assert_eq;
use std::sync::Arc;

#[test]
fn provider_rebinding_isolates_transports_and_shares_only_session_accounting() {
    let root = test_model_client(SessionSource::Cli);
    let provider = root.state.provider.clone();
    let second = root
        .for_provider("second", &provider)
        .expect("second account");
    let third = root
        .for_provider("third", &provider)
        .expect("third account");
    assert!(!Arc::ptr_eq(&root.state, &second.state));
    assert!(!Arc::ptr_eq(&second.state, &third.state));
    assert_eq!(second.state.provider_id, "second");
    assert_eq!(third.state.provider_id, "third");
    assert!(Arc::ptr_eq(
        &root.state.pending_submissions,
        &second.state.pending_submissions
    ));
    assert!(Arc::ptr_eq(
        &root.state.admission_epoch,
        &second.state.admission_epoch
    ));
    assert!(Arc::ptr_eq(
        &root.state.invocation_sink,
        &second.state.invocation_sink
    ));
    let again = root
        .for_provider("second", &provider)
        .expect("existing account");
    assert!(Arc::ptr_eq(&second.state, &again.state));
    let original = root
        .for_provider(&root.state.provider_id, &provider)
        .expect("original account");
    assert!(Arc::ptr_eq(&root.state, &original.state));
    let mut changed_endpoint = provider.clone();
    changed_endpoint.base_url = Some("https://other.example/v1".into());
    let changed = root
        .for_provider("second", &changed_endpoint)
        .expect("changed binding");
    assert!(!Arc::ptr_eq(&changed.state, &second.state));
    let weak = Arc::downgrade(&changed.state);
    drop(changed);
    drop(again);
    drop(second);
    drop(third);
    drop(original);
    drop(root);
    assert!(
        weak.upgrade().is_none(),
        "the transport registry must not retain itself"
    );
}
