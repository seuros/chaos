use super::*;

#[test]
fn preferred_locale_is_resolvable() {
    assert!(resolves(preferred_utf8_locale()));
}

#[test]
fn c_locale_always_resolves() {
    assert!(resolves(FALLBACK_LOCALE));
}

#[test]
fn nonsense_locale_does_not_resolve() {
    assert!(!resolves("xx_NOPE.UTF-8"));
}

#[test]
fn rejects_embedded_nul() {
    assert!(!resolves("C\0UTF-8"));
}
