//! Public-API tests for `chaos-wchar` — taming raw bytes into safe strings.
//!
//! Three small functions, three places where chaos likes to leak in:
//! UUIDs hiding in the middle of arbitrary text (with emoji for spite),
//! tag values carrying garbage that breaks downstream metric pipelines,
//! and markdown link suffixes that look one way to humans and another
//! way to terminals. One dense test per function — the boundaries are
//! the bug.

use chaos_wchar::find_uuids;
use chaos_wchar::normalize_markdown_hash_location_suffix;
use chaos_wchar::sanitize_metric_tag_value;
use pretty_assertions::assert_eq;

#[test]
fn find_uuids_finds_real_ones_and_rejects_lookalikes() {
    // Two valid UUIDs separated by junk, plus a non-ASCII character to
    // prove byte/char index handling doesn't overlap or panic.
    let mixed = "x 00112233-4455-6677-8899-aabbccddeeff-k y 12345678-90ab-cdef-0123-456789abcdef";
    assert_eq!(
        find_uuids(mixed),
        vec![
            "00112233-4455-6677-8899-aabbccddeeff".to_string(),
            "12345678-90ab-cdef-0123-456789abcdef".to_string(),
        ]
    );

    // A string shaped like a UUID but with the wrong group sizes must
    // not match — the regex is the contract.
    assert_eq!(
        find_uuids("not-a-uuid-1234-5678-9abc-def0-123456789abc"),
        Vec::<String>::new()
    );

    // Emoji-adjacent UUIDs must extract cleanly and stop at the first
    // invalid hex char without bleeding into the trailing ASCII.
    assert_eq!(
        find_uuids("🙂 55e5d6f7-8a7f-4d2a-8d88-123456789012abc"),
        vec!["55e5d6f7-8a7f-4d2a-8d88-123456789012".to_string()]
    );
}

#[test]
fn sanitize_metric_tag_value_keeps_it_safe_for_pipelines() {
    // Pure punctuation has no signal — fall back to the sentinel so
    // downstream dashboards don't render an empty key.
    assert_eq!(sanitize_metric_tag_value("///"), "unspecified");

    // Non-allowed chars become underscores; trailing underscore is
    // trimmed so we never emit `bad_value_`.
    assert_eq!(sanitize_metric_tag_value("bad value!"), "bad_value");
}

#[test]
fn normalize_markdown_hash_location_suffix_translates_for_terminals() {
    // Single point: #L74C3 → :74:3
    assert_eq!(
        normalize_markdown_hash_location_suffix("#L74C3"),
        Some(":74:3".to_string())
    );

    // Range: #L74C3-L76C9 → :74:3-76:9
    assert_eq!(
        normalize_markdown_hash_location_suffix("#L74C3-L76C9"),
        Some(":74:3-76:9".to_string())
    );
}
