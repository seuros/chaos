use crate::InlineHiddenTagParser;
use crate::InlineTagSpec;
use crate::StreamTextChunk;
use crate::StreamTextParser;
use crate::collect_visible_text;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CitationTag {
    Citation,
}

const MEMORY_CITATION_OPEN: &str = "<oai-mem-citation>";
const MEMORY_CITATION_CLOSE: &str = "</oai-mem-citation>";
const WEB_CITATION_OPEN: &str = "\u{e200}cite\u{e202}";
const WEB_CITATION_CLOSE: &str = "\u{e201}";

/// Stream parser for hidden citation markup.
///
/// This is a thin convenience wrapper around [`InlineHiddenTagParser`]. It returns citation bodies
/// as plain strings and omits both `<oai-mem-citation>...</oai-mem-citation>` memory citations and
/// `\u{e200}cite\u{e202}...\u{e201}` web citations from visible text.
///
/// Matching is literal and non-nested. If EOF is reached before a closing
/// delimiter, the parser auto-closes the citation and returns the buffered body as extracted data.
#[derive(Debug)]
pub struct CitationStreamParser {
    inner: InlineHiddenTagParser<CitationTag>,
}

impl CitationStreamParser {
    pub fn new() -> Self {
        Self {
            inner: InlineHiddenTagParser::new(vec![
                InlineTagSpec {
                    tag: CitationTag::Citation,
                    open: MEMORY_CITATION_OPEN,
                    close: MEMORY_CITATION_CLOSE,
                },
                InlineTagSpec {
                    tag: CitationTag::Citation,
                    open: WEB_CITATION_OPEN,
                    close: WEB_CITATION_CLOSE,
                },
            ]),
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
    let parser = CitationStreamParser::new();
    let out = collect_visible_text(parser, text);
    (out.visible_text, out.extracted)
}
