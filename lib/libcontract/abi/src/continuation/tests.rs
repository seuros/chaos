use super::Continuation;
use super::ContinuationSlot;

fn continuation(token: &str) -> Continuation {
    Continuation {
        token: token.to_string(),
        prefix_len: 3,
        fingerprint: 7,
    }
}

#[test]
fn clones_share_one_slot_and_each_response_replaces_the_token() {
    let slot = ContinuationSlot::default();
    let adapter_view = slot.clone();
    assert_eq!(slot.get(), None);

    adapter_view.set(continuation("first"));
    adapter_view.set(continuation("second"));
    assert_eq!(slot.get(), Some(continuation("second")));

    slot.clear();
    assert_eq!(adapter_view.get(), None);
}
