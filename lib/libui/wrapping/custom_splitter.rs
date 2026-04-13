use super::RtOptions;
use super::url_detection::is_url_like_token;

/// Reconfigures wrapping options so that URL-like tokens are never split.
///
/// Sets `AsciiSpace` word separation (so `/` and `-` inside URLs are
/// not treated as break points), disables `break_words`, and installs a
/// custom `WordSplitter` that returns no split points for URL tokens
/// while still allowing character-level splitting for non-URL words.
pub fn url_preserving_wrap_options<'a>(opts: RtOptions<'a>) -> RtOptions<'a> {
    opts.word_separator(textwrap::WordSeparator::AsciiSpace)
        .word_splitter(textwrap::WordSplitter::Custom(split_non_url_word))
        .break_words(/*break_words*/ false)
}

/// Custom `textwrap::WordSplitter` callback. Returns empty (no split
/// points) for URL-like tokens so they are kept intact; returns every
/// char-boundary index for everything else so non-URL words can still
/// break at any position.
pub(super) fn split_non_url_word(word: &str) -> Vec<usize> {
    if is_url_like_token(word) {
        return Vec::new();
    }

    word.char_indices().skip(1).map(|(idx, _)| idx).collect()
}
