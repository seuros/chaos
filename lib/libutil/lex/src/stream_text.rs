/// Incremental parser result for one pushed chunk (or final flush).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamTextChunk<T> {
    /// Text safe to render immediately.
    pub visible_text: String,
    /// Hidden payloads extracted from the chunk.
    pub extracted: Vec<T>,
}

impl<T> Default for StreamTextChunk<T> {
    fn default() -> Self {
        Self {
            visible_text: String::new(),
            extracted: Vec::new(),
        }
    }
}

impl<T> StreamTextChunk<T> {
    /// Returns true when no visible text or extracted payloads were produced.
    pub fn is_empty(&self) -> bool {
        self.visible_text.is_empty() && self.extracted.is_empty()
    }
}

/// Feed all `chunks` through `parser`, call `finish`, and return the accumulated result.
///
/// Useful for asserting parser behavior across arbitrary chunk boundaries —
/// the same input split a hundred different ways must produce the same
/// extracted output.
pub fn collect_chunks<P: StreamTextParser>(
    parser: &mut P,
    chunks: &[&str],
) -> StreamTextChunk<P::Extracted> {
    let mut all = StreamTextChunk::default();
    for chunk in chunks {
        let next = parser.push_str(chunk);
        all.visible_text.push_str(&next.visible_text);
        all.extracted.extend(next.extracted);
    }
    let tail = parser.finish();
    all.visible_text.push_str(&tail.visible_text);
    all.extracted.extend(tail.extracted);
    all
}

/// Collect visible text and extracted payloads from a complete string.
///
/// This is the batch-processing version of streaming parsers. It feeds the
/// entire input in one `push_str` call, flushes with `finish`, and returns
/// the accumulated visible text and extracted payloads.
///
/// Useful for simple one-shot text processing where streaming is not needed.
pub fn collect_visible_text<P: StreamTextParser>(
    mut parser: P,
    input: &str,
) -> StreamTextChunk<P::Extracted> {
    let mut out = parser.push_str(input);
    let tail = parser.finish();
    out.visible_text.push_str(&tail.visible_text);
    out.extracted.extend(tail.extracted);
    out
}

/// Trait for parsers that consume streamed text and emit visible text plus extracted payloads.
pub trait StreamTextParser {
    /// Payload extracted by this parser (for example a citation body).
    type Extracted;

    /// Feed a new text chunk.
    fn push_str(&mut self, chunk: &str) -> StreamTextChunk<Self::Extracted>;

    /// Flush any buffered state at end-of-stream (or end-of-item).
    fn finish(&mut self) -> StreamTextChunk<Self::Extracted>;
}
