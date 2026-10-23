//! Integration test for the text encoding fix for issue #6178.
//!
//! These tests simulate VSCode's shell preview on Windows/WSL where the output
//! may be encoded with a legacy code page before it reaches Chaos.

use chaos_kern::exec::StreamOutput;
use pretty_assertions::assert_eq;

#[test]
fn shell_output_decoding_preserves_utf8_and_recovers_legacy_code_pages() {
    for (encoding, bytes, expected) in [
        ("utf8", "пример".as_bytes(), "пример"),
        ("cp1251", b"\xEF\xF0\xE8\xEC\xE5\xF0".as_slice(), "пример"),
        ("cp866", b"\xAF\xE0\xA8\xAC\xA5\xE0".as_slice(), "пример"),
        (
            "windows1252",
            b"\x93\x94 test \x96 dash".as_slice(),
            "\u{201C}\u{201D} test \u{2013} dash",
        ),
        (
            "mixed ascii/latin1",
            b"Output: caf\xE9".as_slice(), // codespell:ignore caf
            "Output: café",
        ),
        ("latin1", b"caf\xE9".as_slice(), "café"), // codespell:ignore caf
        (
            "invalid bytes",
            b"\xFF\xFE\xFD".as_slice(),
            "\u{FFFD}\u{FFFD}\u{FFFD}",
        ),
    ] {
        assert_eq!(decode_shell_output(bytes), expected, "{encoding}");
    }
}

fn decode_shell_output(bytes: &[u8]) -> String {
    StreamOutput {
        text: bytes.to_vec(),
        truncated_after_lines: None,
    }
    .from_utf8_lossy()
    .text
}
