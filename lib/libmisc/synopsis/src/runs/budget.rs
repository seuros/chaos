use std::io::{self, Write};

use serde_json::Value;

use super::MAX_SNAPSHOT_BYTES;

// At most256 attempts, each with a128-byte ID (at worst768 escaped JSON
// bytes), bounded numeric/enumerated fields, and no other free-form metadata.
// Reserve256 KiB for that metadata, separators, and the run envelope.
const METADATA_RESERVE: usize = 256 * 1024;

#[derive(Default)]
pub(super) struct PayloadBudget {
    used: usize,
}

impl PayloadBudget {
    pub(super) fn take(&mut self, value: Option<Value>) -> (Option<Value>, bool) {
        let Some(value) = value else {
            return (None, false);
        };
        let mut counter = Counter {
            remaining: MAX_SNAPSHOT_BYTES - METADATA_RESERVE - self.used,
            used: 0,
        };
        if serde_json::to_writer(&mut counter, &value).is_err() {
            return (None, true);
        }
        self.used += counter.used;
        (Some(value), false)
    }
}

// Count serialized bytes with an early bound, without allocating a second
// buffer proportional to a potentially oversized tool result.
struct Counter {
    remaining: usize,
    used: usize,
}

impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(io::Error::other("snapshot budget exhausted"));
        }
        self.remaining -= bytes.len();
        self.used += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
