#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;

use tempfile::NamedTempFile;

use super::*;

fn synthetic_pcm_wav(sample_rate: u32, num_samples: usize) -> Vec<u8> {
    let channels: u16 = 1;
    let bits_per_sample: u16 = 16;
    let block_align = channels * (bits_per_sample / 8);
    let byte_rate = sample_rate * u32::from(block_align);
    let data_len = (num_samples * usize::from(block_align)) as u32;
    let riff_size = 36 + data_len;

    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.resize(44 + data_len as usize, 0);
    out
}

#[test]
fn valid_pcm_wav_parses_duration_and_placeholder() {
    let bytes = synthetic_pcm_wav(16_000, 16_000);
    let mut file = NamedTempFile::with_suffix(".wav").unwrap();
    file.write_all(&bytes).unwrap();

    let audio = load_local_wav(file.path()).unwrap();
    assert_eq!(audio.format, AudioFormat::Wav);
    assert_eq!(audio.format.mime_type(), "audio/wav");
    assert_eq!(audio.duration_ms, Some(1_000));
    assert_eq!(audio.bytes, bytes);
    assert_eq!(
        audio.placeholder("7f3a9c"),
        format!("[audio clip: {}, 1.0 s, id 7f3a9c]", audio.filename)
    );
}

#[test]
fn wav_with_odd_length_junk_chunk_before_data_parses_cleanly() {
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    // Odd-length JUNK chunk (3 bytes + 1 pad byte)
    out.extend_from_slice(b"JUNK");
    out.extend_from_slice(&3u32.to_le_bytes());
    out.extend_from_slice(&[1, 2, 3, 0]);
    // fmt chunk (8000 Hz, mono 16-bit -> 16000 bytes/sec)
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&8_000u32.to_le_bytes());
    out.extend_from_slice(&16_000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    // data chunk (0.5 s = 8000 bytes)
    out.extend_from_slice(b"data");
    out.extend_from_slice(&8_000u32.to_le_bytes());
    out.resize(out.len() + 8_000, 0);
    let riff_size = u32::try_from(out.len() - 8).unwrap();
    out[4..8].copy_from_slice(&riff_size.to_le_bytes());

    assert_eq!(validate_wav_bytes(&out).unwrap(), Some(500));
}

#[test]
fn rejects_non_wav_truncated_empty_and_oversized_files() {
    assert!(validate_wav_bytes(b"ID3\x03\x00\x00\x00\x00\x00\x00fake mp3").is_err());
    assert!(validate_wav_bytes(b"short").is_err());

    let mut empty_data = synthetic_pcm_wav(8_000, 0);
    assert!(validate_wav_bytes(&empty_data).is_err());

    // Truncated data chunk
    empty_data = synthetic_pcm_wav(8_000, 100);
    empty_data.truncate(50);
    assert!(validate_wav_bytes(&empty_data).is_err());

    // Oversized file rejected by cap
    let valid = synthetic_pcm_wav(8_000, 100);
    let mut file = NamedTempFile::new().unwrap();
    file.write_all(&valid).unwrap();
    let err = load_local_wav_with_limit(file.path(), 64).unwrap_err();
    assert!(matches!(err, AudioError::TooLarge { .. }));
}

#[test]
fn rejects_invalid_riff_extent_and_fmt_fields() {
    // Declared RIFF size exceeds actual file length.
    let mut oversized_riff = synthetic_pcm_wav(8_000, 100);
    let bad_riff = u32::try_from(oversized_riff.len()).unwrap();
    oversized_riff[4..8].copy_from_slice(&bad_riff.to_le_bytes());
    assert!(validate_wav_bytes(&oversized_riff).is_err());

    // Data chunk sits past declared RIFF extent (trailing garbage ignored).
    let mut short_riff = synthetic_pcm_wav(8_000, 100);
    // 4 ("WAVE") + 24 ("fmt " header+body) = 28 -> excludes "data" chunk.
    short_riff[4..8].copy_from_slice(&28u32.to_le_bytes());
    assert!(validate_wav_bytes(&short_riff).is_err());

    // Unsupported format tag.
    let mut bad_tag = synthetic_pcm_wav(8_000, 100);
    bad_tag[20..22].copy_from_slice(&0x9999u16.to_le_bytes());
    assert!(validate_wav_bytes(&bad_tag).is_err());

    // Zero channels.
    let mut zero_channels = synthetic_pcm_wav(8_000, 100);
    zero_channels[22..24].copy_from_slice(&0u16.to_le_bytes());
    assert!(validate_wav_bytes(&zero_channels).is_err());

    // Invalid PCM bits_per_sample (12-bit).
    let mut bad_bps = synthetic_pcm_wav(8_000, 100);
    bad_bps[34..36].copy_from_slice(&12u16.to_le_bytes());
    assert!(validate_wav_bytes(&bad_bps).is_err());
}

#[cfg(unix)]
#[test]
fn rejects_named_pipe_without_blocking() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let dir = tempfile::tempdir().unwrap();
    let fifo_path = dir.path().join("pipe.wav");
    let c_path = CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
    // SAFETY: valid NUL-terminated path with standard mode bits.
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
    assert_eq!(rc, 0);

    let err = load_local_wav(&fifo_path).unwrap_err();
    assert!(matches!(err, AudioError::NotRegularFile { .. }));
}
