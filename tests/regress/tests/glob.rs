//! Public-API tests for `chaos-glob` — fuzzy matching, order from noise.
//!
//! The scoring function is the contract: prefix hits beat interior hits,
//! contiguous spans beat scattered ones, and Unicode casefolding doesn't
//! get to cheat (ß ≠ ss here). Every assertion pins a specific edge
//! where the score differential actually changes user-visible ranking.

use chaos_glob::fuzzy_match;

#[test]
fn fuzzy_match_scores_prefix_contiguous_and_unicode_edges() {
    // Basic ASCII hit: 'h' at 0, 'l' at 2 → window 1, prefix bonus -100 → -99.
    let (idx, score) = fuzzy_match("hello", "hl").expect("match");
    assert_eq!(idx, vec![0, 2]);
    assert_eq!(score, -99);

    // Unicode: İ lowercases to i + combining dot; needle "is" hits at
    // lowered positions 0 and 2, maps back to original char indices [0, 1].
    let (idx, score) = fuzzy_match("İstanbul", "is").expect("match");
    assert_eq!(idx, vec![0, 1]);
    assert_eq!(score, -99);

    // German sharp-s: ß does NOT casefold to "ss" for matching purposes.
    assert!(fuzzy_match("straße", "strasse").is_none());

    // Prefix bonus pins ranking: contiguous prefix (-100) beats scattered
    // prefix (-98) beats non-prefix contiguous (0).
    let (_, score_contiguous_prefix) = fuzzy_match("abc", "abc").expect("match");
    let (_, score_scattered_prefix) = fuzzy_match("a-b-c", "abc").expect("match");
    let (_, score_non_prefix) = fuzzy_match("my_file_name", "file").expect("match");
    assert_eq!(score_contiguous_prefix, -100);
    assert_eq!(score_scattered_prefix, -98);
    assert_eq!(score_non_prefix, 0);
    assert!(score_contiguous_prefix < score_scattered_prefix);
    assert!(score_scattered_prefix < score_non_prefix);

    // Empty needle: sentinel match, no indices, worst possible score so
    // it loses every ranking comparison.
    let (idx, score) = fuzzy_match("anything", "").expect("empty needle matches");
    assert!(idx.is_empty());
    assert_eq!(score, i32::MAX);

    // Case-insensitive ASCII is a contiguous prefix hit → -100.
    let (idx, score) = fuzzy_match("FooBar", "foO").expect("match");
    assert_eq!(idx, vec![0, 1, 2]);
    assert_eq!(score, -100);

    // Multichar lowercase expansion (İ → i + combining dot) must dedupe
    // to a single original-char index, not two.
    let needle = "\u{0069}\u{0307}"; // "i" + combining dot above
    let (idx, score) = fuzzy_match("İ", needle).expect("match");
    assert_eq!(idx, vec![0]);
    assert_eq!(score, -100);
}
