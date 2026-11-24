//! Pure-Rust local audio validation and encoding helpers.
//!
//! Validates container magic bytes, structural headers, and file size caps
//! before any audio payload is serialized into a prompt or forwarded upstream.

use std::fs::OpenOptions;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use thiserror::Error;

/// Maximum raw audio file size accepted for inline upload (8 MiB).
///
/// Base64 expands the payload by ~33% (~10.67 MiB), leaving headroom under
/// Chaos's 16 MiB generation-request ceiling for prompt text and tool schemas.
pub const MAX_AUDIO_FILE_BYTES: usize = 8 * 1024 * 1024;

/// Supported local audio container formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Wav,
}

impl AudioFormat {
    /// Canonical MIME type for this format.
    pub const fn mime_type(self) -> &'static str {
        match self {
            Self::Wav => "audio/wav",
        }
    }

    /// Canonical lowercase file extension without a leading dot.
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Wav => "wav",
        }
    }
}

/// A validated local audio clip ready to be encoded for transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedAudio {
    pub format: AudioFormat,
    pub bytes: Vec<u8>,
    pub duration_ms: Option<u64>,
    pub filename: String,
}

impl ValidatedAudio {
    /// Encode the validated raw audio bytes using standard base64.
    pub fn encode_base64(&self) -> String {
        STANDARD.encode(&self.bytes)
    }

    /// Format a permanent, human-readable placeholder carrying the turn nonce.
    pub fn placeholder(&self, nonce: &str) -> String {
        format!(
            "[audio clip: {}, {}, id {}]",
            self.filename,
            format_duration(self.duration_ms),
            nonce
        )
    }
}

/// Errors returned when loading or validating a local audio attachment.
#[derive(Debug, Error)]
pub enum AudioError {
    #[error("failed to read local audio at `{}`: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error(
        "local audio `{}` is {size} bytes, exceeding the {max_bytes}-byte limit",
        path.display()
    )]
    TooLarge {
        path: PathBuf,
        size: u64,
        max_bytes: usize,
    },

    #[error("unsupported or invalid audio file `{}`: {detail}", path.display())]
    UnsupportedFormat { path: PathBuf, detail: String },

    #[error("local audio `{}` must be a regular file", path.display())]
    NotRegularFile { path: PathBuf },

    #[error("at most one local audio clip may be attached per turn")]
    TooManyAttachments,
}

/// Read and validate a local WAV file, enforcing [`MAX_AUDIO_FILE_BYTES`].
pub fn load_local_wav(path: &Path) -> Result<ValidatedAudio, AudioError> {
    load_local_wav_with_limit(path, MAX_AUDIO_FILE_BYTES)
}

/// Read and validate a local WAV file with an explicit raw-byte ceiling.
pub fn load_local_wav_with_limit(
    path: &Path,
    max_bytes: usize,
) -> Result<ValidatedAudio, AudioError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NONBLOCK);
    let file = options.open(path).map_err(|source| AudioError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = file.metadata().map_err(|source| AudioError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(AudioError::NotRegularFile {
            path: path.to_path_buf(),
        });
    }
    let max_u64 = u64::try_from(max_bytes).unwrap_or(u64::MAX);
    if metadata.len() > max_u64 {
        return Err(AudioError::TooLarge {
            path: path.to_path_buf(),
            size: metadata.len(),
            max_bytes,
        });
    }

    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0).min(max_bytes));
    let read_limit = max_u64.saturating_add(1);
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|source| AudioError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > max_bytes {
        return Err(AudioError::TooLarge {
            path: path.to_path_buf(),
            size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            max_bytes,
        });
    }

    let duration_ms =
        validate_wav_bytes(&bytes).map_err(|detail| AudioError::UnsupportedFormat {
            path: path.to_path_buf(),
            detail,
        })?;
    let filename = sanitize_filename(path);

    Ok(ValidatedAudio {
        format: AudioFormat::Wav,
        bytes,
        duration_ms,
        filename,
    })
}

/// Validate a RIFF/WAVE container in memory and return its duration in
/// milliseconds when a valid `fmt ` byte rate and `data` chunk are present.
pub fn validate_wav_bytes(bytes: &[u8]) -> Result<Option<u64>, String> {
    if bytes.len() < 12 {
        return Err(
            "file is shorter than a 12-byte RIFF/WAVE header (only WAV is supported)".into(),
        );
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("missing RIFF/WAVE magic header (only WAV is supported)".into());
    }
    let riff_size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    if riff_size < 4 {
        return Err("WAV RIFF header size is smaller than the WAVE form tag".into());
    }
    let Some(riff_end) = riff_size.checked_add(8) else {
        return Err("WAV RIFF header size overflow".into());
    };
    if riff_end > bytes.len() {
        return Err("WAV RIFF declared extent exceeds file length".into());
    }
    if riff_end < bytes.len() {
        return Err("WAV file has trailing bytes past declared RIFF extent".into());
    }

    let mut cursor = 12usize;
    let mut byte_rate: Option<u32> = None;
    let mut data_size: Option<u32> = None;

    while cursor + 8 <= riff_end {
        let chunk_id = &bytes[cursor..cursor + 4];
        let chunk_len = u32::from_le_bytes([
            bytes[cursor + 4],
            bytes[cursor + 5],
            bytes[cursor + 6],
            bytes[cursor + 7],
        ]) as usize;
        cursor += 8;

        let Some(chunk_end) = cursor.checked_add(chunk_len) else {
            return Err("WAV chunk length overflow".into());
        };
        if chunk_end > riff_end {
            return Err("WAV chunk extends past declared RIFF extent".into());
        }

        if chunk_id == b"fmt " {
            if chunk_len < 16 {
                return Err("WAV `fmt ` chunk is shorter than 16 bytes".into());
            }
            let audio_format = u16::from_le_bytes([bytes[cursor], bytes[cursor + 1]]);
            let channels = u16::from_le_bytes([bytes[cursor + 2], bytes[cursor + 3]]);
            let sample_rate = u32::from_le_bytes([
                bytes[cursor + 4],
                bytes[cursor + 5],
                bytes[cursor + 6],
                bytes[cursor + 7],
            ]);
            let rate = u32::from_le_bytes([
                bytes[cursor + 8],
                bytes[cursor + 9],
                bytes[cursor + 10],
                bytes[cursor + 11],
            ]);
            let block_align = u16::from_le_bytes([bytes[cursor + 12], bytes[cursor + 13]]);
            let bits_per_sample = u16::from_le_bytes([bytes[cursor + 14], bytes[cursor + 15]]);
            if channels == 0 {
                return Err("WAV `fmt ` chunk reports zero channels".into());
            }
            if sample_rate == 0 {
                return Err("WAV `fmt ` chunk reports zero sample rate".into());
            }
            if block_align == 0 {
                return Err("WAV `fmt ` chunk reports zero block align".into());
            }
            match audio_format {
                1 => {
                    if !matches!(bits_per_sample, 8 | 16 | 24 | 32) {
                        return Err(format!(
                            "WAV PCM `fmt ` chunk has unsupported bits_per_sample ({bits_per_sample})"
                        ));
                    }
                }
                3 => {
                    if !matches!(bits_per_sample, 32 | 64) {
                        return Err(format!(
                            "WAV IEEE float `fmt ` chunk has unsupported bits_per_sample ({bits_per_sample})"
                        ));
                    }
                }
                0xFFFE => {
                    if chunk_len < 40 {
                        return Err("WAV extensible `fmt ` chunk is shorter than 40 bytes".into());
                    }
                    if bits_per_sample == 0 || bits_per_sample % 8 != 0 {
                        return Err(format!(
                            "WAV extensible `fmt ` chunk has invalid bits_per_sample ({bits_per_sample})"
                        ));
                    }
                }
                other => {
                    return Err(format!(
                        "unsupported WAV audio format tag 0x{other:04x} (only PCM, IEEE float, and extensible WAV are supported)"
                    ));
                }
            }
            byte_rate = Some(rate);
        } else if chunk_id == b"data" {
            if chunk_len == 0 {
                return Err("WAV `data` chunk is empty".into());
            }
            data_size = Some(u32::try_from(chunk_len).unwrap_or(u32::MAX));
        }

        // RIFF chunks are padded to an even byte boundary.
        let padded = chunk_len + (chunk_len & 1);
        cursor = cursor.saturating_add(padded);
    }

    let Some(rate) = byte_rate else {
        return Err("WAV file is missing a `fmt ` chunk".into());
    };
    let Some(data_len) = data_size else {
        return Err("WAV file is missing a `data` chunk".into());
    };
    if rate == 0 {
        return Err("WAV `fmt ` chunk reports a zero byte rate".into());
    }

    let duration_ms = u64::try_from((u128::from(data_len) * 1_000) / u128::from(rate)).ok();
    Ok(duration_ms)
}

/// Format an optional duration in milliseconds for the permanent placeholder.
pub fn format_duration(duration_ms: Option<u64>) -> String {
    match duration_ms {
        Some(ms) => {
            let tenths = (ms + 50) / 100;
            format!("{}.{} s", tenths / 10, tenths % 10)
        }
        None => "unknown duration".to_string(),
    }
}

fn sanitize_filename(path: &Path) -> String {
    let raw = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("audio.wav");
    let cleaned: String = raw
        .chars()
        .filter(|ch| !ch.is_control() && !matches!(ch, '[' | ']' | ','))
        .take(96)
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "audio.wav".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Format a permanent note used when a turn containing a local audio clip is
/// recovered from the session journal without re-reading or re-sending the file.
pub fn recovery_placeholder(path: &Path) -> String {
    format!(
        "[audio clip not re-sent after recovery: {}]",
        sanitize_filename(path)
    )
}

#[cfg(test)]
mod tests;
