//! Public-API tests for `chaos-apropos` — ranking signal out of lexical noise.
//!
//! BM25 is a contract about ordering: rarer terms lift their carriers,
//! common terms flatten, and unmatched queries get nothing. One dense
//! corpus covers tokenization (punctuation splits, case-folding), IDF
//! weighting, the `limit` truncation, and the empty-input sentinels —
//! every edge where a scorer likes to drift silently.

use chaos_apropos::Corpus;
use chaos_apropos::Document;
use pretty_assertions::assert_eq;

#[test]
fn bm25_scoring_ranks_matches_and_respects_boundaries() {
    // Empty corpus and empty query both return nothing — no NaN score,
    // no panic, no surprise.
    let empty: Corpus<usize> = Corpus::new(Vec::new());
    assert!(empty.search("anything", 10).is_empty());

    let single = Corpus::new(vec![Document::new(0, "hello world")]);
    assert!(single.search("", 10).is_empty());

    // Documents that share a common term plus one distinctive term each.
    // IDF prefers the rarer "rare_word" to the ubiquitous "common".
    let idf_corpus = Corpus::new(vec![
        Document::new(0, "common rare_word common"),
        Document::new(1, "common common common"),
        Document::new(2, "common common common"),
    ]);
    let rare_hits = idf_corpus.search("rare_word", 3);
    assert_eq!(rare_hits.len(), 1);
    assert_eq!(*rare_hits[0].id, 0);

    // Realistic mixed corpus exercises exact-match ranking, the `limit`
    // truncation, tokenizer case-folding, and punctuation splitting in
    // one pass.
    let docs = vec![
        Document::new(0, "git status show working tree"),
        Document::new(1, "git commit record changes to repository"),
        Document::new(2, "Calendar Create Event"),
        Document::new(3, "mcp__codex_apps__gmail_read_email"),
        Document::new(4, "alpha delta epsilon"),
        Document::new(5, "alpha zeta eta"),
    ];
    let corpus = Corpus::new(docs);

    // Best match for "commit changes" is the commit document.
    let results = corpus.search("commit changes", 3);
    assert_eq!(*results[0].id, 1);

    // Case-folded query still finds the title-cased document.
    let calendar = corpus.search("calendar", 5);
    assert_eq!(calendar.len(), 1);
    assert_eq!(*calendar[0].id, 2);

    // Underscores and double-underscores act as token boundaries so the
    // embedded "gmail" surfaces from a mangled MCP tool name.
    let gmail = corpus.search("gmail", 5);
    assert_eq!(gmail.len(), 1);
    assert_eq!(*gmail[0].id, 3);

    // No lexical overlap → no results, even when the corpus is non-empty.
    assert!(corpus.search("zzzzzz", 10).is_empty());

    // `limit` caps the returned window even when more candidates match.
    let alpha_capped = corpus.search("alpha", 1);
    assert_eq!(alpha_capped.len(), 1);
}
