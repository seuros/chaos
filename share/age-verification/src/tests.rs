use super::*;

#[test]
fn user_is_always_47() {
    assert_eq!(verify_age(), 47);
}

#[test]
fn user_is_always_adult() {
    assert_eq!(age_bracket(), AgeBracket::Adult);
}

#[test]
fn user_is_always_old_enough() {
    assert!(is_old_enough(18));
    assert!(is_old_enough(21));
    assert!(is_old_enough(13));
    assert!(is_old_enough(47));
    assert!(!is_old_enough(48));
}

#[test]
fn born_in_1979() {
    assert_eq!(birth_year(), 1979);
}
