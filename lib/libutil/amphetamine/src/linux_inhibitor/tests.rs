use super::BLOCKER_SLEEP_SECONDS;

#[test]
fn sleep_seconds_is_i32_max() {
    assert_eq!(BLOCKER_SLEEP_SECONDS, format!("{}", i32::MAX));
}
