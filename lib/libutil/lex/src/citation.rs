use crate::InlineHiddenTagParser;
use crate::InlineTagSpec;
use crate::StreamTextChunk;
use crate::StreamTextParser;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CitationTag {
    Citation,
}

const CITATION_OPEN: &str = "<oai-mem-citation>";
const CITATION_CLOSE: &str = "</oai-mem-citation>";

/// Stream parser for `<oai-mem-citation>...</oai-mem-citation>` tags.
///
/// This is a thin convenience wrapper around [`InlineHiddenTagParser`]. It returns citation bodies
/// as plain strings and omits the citation tags from visible text.
///
/// Matching is literal and non-nested. If EOF is reached before a closing
/// `</oai-mem-citation>`, the parser auto-closes the tag and returns the buffered body as an
/// extracted citation.
#[derive(Debug)]
pub struct CitationStreamParser {
    inner: InlineHiddenTagParser<CitationTag>,
}

impl CitationStreamParser {
    pub fn new() -> Self {
        Self {
            inner: InlineHiddenTagParser::new(vec![InlineTagSpec {
                tag: CitationTag::Citation,
                open: CITATION_OPEN,
                close: CITATION_CLOSE,
            }]),
        }
    }
}

impl Default for CitationStreamParser {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamTextParser for CitationStreamParser {
    type Extracted = String;

    fn push_str(&mut self, chunk: &str) -> StreamTextChunk<Self::Extracted> {
        let inner = self.inner.push_str(chunk);
        StreamTextChunk {
            visible_text: inner.visible_text,
            extracted: inner.extracted.into_iter().map(|tag| tag.content).collect(),
        }
    }

    fn finish(&mut self) -> StreamTextChunk<Self::Extracted> {
        let inner = self.inner.finish();
        StreamTextChunk {
            visible_text: inner.visible_text,
            extracted: inner.extracted.into_iter().map(|tag| tag.content).collect(),
        }
    }
}

/// Strip citation tags from a complete string and return `(visible_text, citations)`.
///
/// This uses [`CitationStreamParser`] internally, so it inherits the same semantics:
/// literal, non-nested matching and auto-closing unterminated citations at EOF.
pub fn strip_citations(text: &str) -> (String, Vec<String>) {
    let mut parser = CitationStreamParser::new();
    let mut out = parser.push_str(text);
    let tail = parser.finish();
    out.visible_text.push_str(&tail.visible_text);
    out.extracted.extend(tail.extracted);
    (out.visible_text, out.extracted)
}
