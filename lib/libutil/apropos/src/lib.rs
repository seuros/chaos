//! BM25 keyword relevance ranking.
//!
//! A zero-dependency implementation of Okapi BM25 for ranking documents
//! against a free-text query. Designed for small corpora (tool search,
//! man page lookup) where building a full inverted index is overkill.
//!
//! ```
//! use chaos_apropos::{Corpus, Document};
//!
//! let docs = vec![
//!     Document::new(0, "git status show working tree"),
//!     Document::new(1, "git commit record changes"),
//!     Document::new(2, "cargo build compile project"),
//! ];
//! let corpus = Corpus::new(docs);
//! let results = corpus.search("commit changes", 2);
//! assert_eq!(*results[0].id, 1);
//! ```

use std::collections::HashMap;

/// Tuning parameters for BM25 scoring.
const K1: f64 = 1.2;
const B: f64 = 0.75;

/// A document in the corpus.
#[derive(Debug, Clone)]
pub struct Document<Id> {
    /// Caller-defined identifier returned in search results.
    pub id: Id,
    /// Pre-tokenized terms (lowercased, split on whitespace/punctuation).
    terms: Vec<String>,
}

impl<Id> Document<Id> {
    /// Create a document from raw text. Tokenizes by splitting on
    /// non-alphanumeric boundaries and lowercasing.
    pub fn new(id: Id, text: impl AsRef<str>) -> Self {
        Self {
            id,
            terms: tokenize(text.as_ref()),
        }
    }
}

/// A scored search result.
#[derive(Debug, Clone)]
pub struct SearchResult<'a, Id> {
    /// Reference to the matching document's id.
    pub id: &'a Id,
    /// BM25 relevance score (higher is better).
    pub score: f64,
}

/// An indexed corpus ready for search.
pub struct Corpus<Id> {
    documents: Vec<Document<Id>>,
    /// term → number of documents containing that term
    doc_freq: HashMap<String, usize>,
    /// Average document length across the corpus.
    avg_dl: f64,
}

impl<Id> Corpus<Id> {
    /// Build a corpus from a collection of documents.
    pub fn new(documents: Vec<Document<Id>>) -> Self {
        let n = documents.len();
        let mut doc_freq: HashMap<String, usize> = HashMap::new();
        let mut total_len: usize = 0;

        for doc in &documents {
            total_len += doc.terms.len();
            // Count each unique term once per document.
            let mut seen = HashMap::new();
            for term in &doc.terms {
                seen.entry(term.as_str()).or_insert(true);
            }
            for term in seen.into_keys() {
                *doc_freq.entry(term.to_string()).or_insert(0) += 1;
            }
        }

        let avg_dl = if n == 0 {
            0.0
        } else {
            total_len as f64 / n as f64
        };

        Self {
            documents,
            doc_freq,
            avg_dl,
        }
    }

    /// Search the corpus and return up to `limit` results ranked by BM25 score.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchResult<'_, Id>> {
        let query_terms = tokenize(query);
        if query_terms.is_empty() || self.documents.is_empty() {
            return Vec::new();
        }

        let n = self.documents.len() as f64;

        let mut scored: Vec<SearchResult<'_, Id>> = self
            .documents
            .iter()
            .filter_map(|doc| {
                let dl = doc.terms.len() as f64;
                let mut score = 0.0f64;

                // Build term frequency map for this document.
                let mut tf_map: HashMap<&str, usize> = HashMap::new();
                for term in &doc.terms {
                    *tf_map.entry(term.as_str()).or_insert(0) += 1;
                }

                for qt in &query_terms {
                    let tf = *tf_map.get(qt.as_str()).unwrap_or(&0) as f64;
                    if tf == 0.0 {
                        continue;
                    }
                    let df = *self.doc_freq.get(qt.as_str()).unwrap_or(&0) as f64;
                    // IDF with smoothing to avoid negative values.
                    let idf = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
                    let tf_norm = (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * dl / self.avg_dl));
                    score += idf * tf_norm;
                }

                if score > 0.0 {
                    Some(SearchResult {
                        id: &doc.id,
                        score,
                    })
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);
        scored
    }
}

/// Tokenize text into lowercase terms split on non-alphanumeric boundaries.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_corpus_returns_no_results() {
        let corpus: Corpus<usize> = Corpus::new(Vec::new());
        assert!(corpus.search("anything", 10).is_empty());
    }

    #[test]
    fn empty_query_returns_no_results() {
        let corpus = Corpus::new(vec![Document::new(0, "hello world")]);
        assert!(corpus.search("", 10).is_empty());
    }

    #[test]
    fn exact_match_ranks_first() {
        let docs = vec![
            Document::new(0, "git status show working tree"),
            Document::new(1, "git commit record changes to repository"),
            Document::new(2, "cargo build compile the project"),
        ];
        let corpus = Corpus::new(docs);
        let results = corpus.search("commit changes", 3);
        assert_eq!(*results[0].id, 1);
    }

    #[test]
    fn limit_is_respected() {
        let docs = vec![
            Document::new(0, "alpha beta gamma"),
            Document::new(1, "alpha delta epsilon"),
            Document::new(2, "alpha zeta eta"),
        ];
        let corpus = Corpus::new(docs);
        let results = corpus.search("alpha", 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn no_match_returns_empty() {
        let docs = vec![Document::new(0, "hello world")];
        let corpus = Corpus::new(docs);
        assert!(corpus.search("zzzzzz", 10).is_empty());
    }

    #[test]
    fn case_insensitive() {
        let docs = vec![Document::new(0, "Calendar Create Event")];
        let corpus = Corpus::new(docs);
        let results = corpus.search("calendar", 5);
        assert_eq!(results.len(), 1);
        assert_eq!(*results[0].id, 0);
    }

    #[test]
    fn punctuation_is_split() {
        let docs = vec![Document::new(0, "mcp__codex_apps__gmail_read_email")];
        let corpus = Corpus::new(docs);
        let results = corpus.search("gmail", 5);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn idf_prefers_rarer_terms() {
        let docs = vec![
            Document::new(0, "common rare_word common"),
            Document::new(1, "common common common"),
            Document::new(2, "common common common"),
        ];
        let corpus = Corpus::new(docs);
        let results = corpus.search("rare_word", 3);
        assert_eq!(results.len(), 1);
        assert_eq!(*results[0].id, 0);
    }
}
