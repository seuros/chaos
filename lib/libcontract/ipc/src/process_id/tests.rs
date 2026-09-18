use super::*;
#[test]
fn test_process_id_default_is_not_zeroes() {
    let id = ProcessId::default();
    assert_ne!(id.uuid, Uuid::nil());
}
