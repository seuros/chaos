use std::error::Error;
use std::fmt;

use crate::StreamTextChunk;
use crate::StreamTextParser;

/// Error returned by [`Utf8StreamParser`] when streamed bytes are not valid UTF-8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Utf8StreamParserError {
    /// The provided bytes contain an invalid UTF-8 sequence.
    InvalidUtf8 {
        /// Byte offset in the parser's buffered bytes where decoding failed.
        valid_up_to: usize,
        /// Length in bytes of the invalid sequence.
        error_len: usize,
    },
    /// EOF was reached with a buffered partial UTF-8 code point.
    IncompleteUtf8AtEof,
}

impl fmt::Display for Utf8StreamParserError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUtf8 {
                valid_up_to,
                error_len,
            } => write!(
                f,
                "invalid UTF-8 in streamed bytes at offset {valid_up_to} (error length {error_len})"
            ),
            Self::IncompleteUtf8AtEof => {
                write!(f, "incomplete UTF-8 code point at end of stream")
            }
        }
    }
}

impl Error for Utf8StreamParserError {}

/// Wraps a [`StreamTextParser`] and accepts raw bytes, buffering partial UTF-8 code points.
///
/// This is useful when upstream data arrives as `&[u8]` and a code point may be split across
/// chunk boundaries (for example `0xC3` followed by `0xA9` for `é`).
#[derive(Debug)]
pub struct Utf8StreamParser<P> {
    inner: P,
    pending_utf8: Vec<u8>,
}

impl<P> Utf8StreamParser<P>
where
    P: StreamTextParser,
{
    pub fn new(inner: P) -> Self {
        Self {
            inner,
            pending_utf8: Vec::new(),
        }
    }

    /// Feed a raw byte chunk.
    ///
    /// If the chunk contains invalid UTF-8, this returns an error and rolls back the entire
    /// pushed chunk so callers can decide how to recover without the inner parser seeing a partial
    /// prefix from that chunk.
    pub fn push_bytes(
        &mut self,
        chunk: &[u8],
    ) -> Result<StreamTextChunk<P::Extracted>, Utf8StreamParserError> {
        let old_len = self.pending_utf8.len();
        self.pending_utf8.extend_from_slice(chunk);

        match std::str::from_utf8(&self.pending_utf8) {
            Ok(text) => {
                let out = self.inner.push_str(text);
                self.pending_utf8.clear();
                Ok(out)
            }
            Err(err) => {
                if let Some(error_len) = err.error_len() {
                    self.pending_utf8.truncate(old_len);
                    return Err(Utf8StreamParserError::InvalidUtf8 {
                        valid_up_to: err.valid_up_to(),
                        error_len,
                    });
                }

                let valid_up_to = err.valid_up_to();
                if valid_up_to == 0 {
                    return Ok(StreamTextChunk::default());
                }

                let text = match std::str::from_utf8(&self.pending_utf8[..valid_up_to]) {
                    Ok(text) => text,
                    Err(prefix_err) => {
                        self.pending_utf8.truncate(old_len);
                        let error_len = prefix_err.error_len().unwrap_or(0);
                        return Err(Utf8StreamParserError::InvalidUtf8 {
                            valid_up_to: prefix_err.valid_up_to(),
                            error_len,
                        });
                    }
                };
                let out = self.inner.push_str(text);
                self.pending_utf8.drain(..valid_up_to);
                Ok(out)
            }
        }
    }

    pub fn finish(&mut self) -> Result<StreamTextChunk<P::Extracted>, Utf8StreamParserError> {
        if !self.pending_utf8.is_empty() {
            match std::str::from_utf8(&self.pending_utf8) {
                Ok(_) => {}
                Err(err) => {
                    if let Some(error_len) = err.error_len() {
                        return Err(Utf8StreamParserError::InvalidUtf8 {
                            valid_up_to: err.valid_up_to(),
                            error_len,
                        });
                    }
                    return Err(Utf8StreamParserError::IncompleteUtf8AtEof);
                }
            }
        }

        let mut out = if self.pending_utf8.is_empty() {
            StreamTextChunk::default()
        } else {
            let text = match std::str::from_utf8(&self.pending_utf8) {
                Ok(text) => text,
                Err(err) => {
                    let error_len = err.error_len().unwrap_or(0);
                    return Err(Utf8StreamParserError::InvalidUtf8 {
                        valid_up_to: err.valid_up_to(),
                        error_len,
                    });
                }
            };
            let out = self.inner.push_str(text);
            self.pending_utf8.clear();
            out
        };

        let mut tail = self.inner.finish();
        out.visible_text.push_str(&tail.visible_text);
        out.extracted.append(&mut tail.extracted);
        Ok(out)
    }

    /// Return the wrapped parser if no undecoded UTF-8 bytes are buffered.
    ///
    /// Use [`Self::finish`] first if you want to flush buffered text into the wrapped parser.
    pub fn into_inner(self) -> Result<P, Utf8StreamParserError> {
        if self.pending_utf8.is_empty() {
            return Ok(self.inner);
        }
        match std::str::from_utf8(&self.pending_utf8) {
            Ok(_) => Ok(self.inner),
            Err(err) => {
                if let Some(error_len) = err.error_len() {
                    return Err(Utf8StreamParserError::InvalidUtf8 {
                        valid_up_to: err.valid_up_to(),
                        error_len,
                    });
                }
                Err(Utf8StreamParserError::IncompleteUtf8AtEof)
            }
        }
    }

    /// Return the wrapped parser without validating or flushing buffered undecoded bytes.
    ///
    /// This may drop a partial UTF-8 code point that was buffered across chunk boundaries.
    pub fn into_inner_lossy(self) -> P {
        self.inner
    }
}
