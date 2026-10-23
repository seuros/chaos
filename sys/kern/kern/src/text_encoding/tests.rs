use super::*;
use encoding_rs::BIG5;
use encoding_rs::EUC_KR;
use encoding_rs::GBK;
use encoding_rs::ISO_8859_2;
use encoding_rs::ISO_8859_4;
use encoding_rs::ISO_8859_5;
use encoding_rs::ISO_8859_6;
use encoding_rs::ISO_8859_7;
use encoding_rs::ISO_8859_8;
use encoding_rs::ISO_8859_10;
use encoding_rs::SHIFT_JIS;
use encoding_rs::WINDOWS_874;
use encoding_rs::WINDOWS_1250;
use encoding_rs::WINDOWS_1251;
use encoding_rs::WINDOWS_1253;
use encoding_rs::WINDOWS_1254;
use encoding_rs::WINDOWS_1255;
use encoding_rs::WINDOWS_1256;
use encoding_rs::WINDOWS_1257;
use encoding_rs::WINDOWS_1258;
use pretty_assertions::assert_eq;

#[test]
fn decoding_preserves_utf8_and_handles_ambiguous_legacy_bytes() {
    for (bytes, expected) in [
        ("Hello, мир! 世界".as_bytes(), "Hello, мир! 世界"),
        // Valid UTF-8 mojibake must not be "repaired".
        ("ÐŸÑ€Ð¸Ð²ÐµÑ‚".as_bytes(), "ÐŸÑ€Ð¸Ð²ÐµÑ‚"),
        (b"\x1b[31mred\x1b[0m", "\x1b[31mred\x1b[0m"),
        (b"\xEF\xF0\xE8\xEC\xE5\xF0", "пример"),
        // Regression: CP1251 words must not be mistaken for CP1252.
        (b"\xCF\xF0\xE8\xE2\xE5\xF2", "Привет"),
        (b"\xF0\xD2\xC9\xD7\xC5\xD4", "Привет"),
        (b"\xAF\xE0\xA8\xAC\xA5\xE0", "пример"),
        (b"\x8F\x90\x88", "ПРИ"),
        // Uppercase CP866 next to ASCII must not be mistaken for CP1252.
        (b"\x8F\x90\x88 test", "ПРИ test"),
        (b"\x93\x94test", "\u{201C}\u{201D}test"),
        (b"\x93foo\x94 \x96 \x93bar\x94", "“foo” – “bar”"),
        (b"caf\xE9", "café"),
    ] {
        assert_eq!(bytes_to_string_smart(bytes), expected, "bytes: {bytes:?}");
    }
}

#[test]
fn detects_non_utf8_terminal_output() {
    // Non-ASCII samples exercise our detector selection, not just encoding_rs's
    // ASCII round-trip. Aliases with identical bytes need only one case.
    for (encoding, sample) in [
        (ISO_8859_2, "Příliš žluťoučký kůň"),
        (
            ISO_8859_4,
            "Šis ir rakstzīmju kodēšanas tests. Dažās valodās, kurās tiek \
             izmantotas latīņu valodas burti, lēmuma pieņemšanai mums ir nepieciešams \
             vairāk ieguldījuma.",
        ),
        (ISO_8859_5, "Привет"),
        (ISO_8859_6, "مرحبا"),
        (ISO_8859_7, "Καλημέρα"),
        (ISO_8859_8, "שלום"),
        (ISO_8859_10, "Þetta er prófun fyrir Ægir og Øystein."),
        (WINDOWS_874, "ภาษาไทยสำหรับการทดสอบ ISO-8859-11"),
        (WINDOWS_1250, "Příliš žluťoučký kůň"),
        (WINDOWS_1251, "Привет из Windows-1251"),
        (WINDOWS_1253, "Γειά σου"),
        (WINDOWS_1254, "İstanbul"),
        (WINDOWS_1255, "שלום"),
        (WINDOWS_1256, "مرحبا"),
        (WINDOWS_1257, "Pērkons"),
        (WINDOWS_1258, "Xin chào"),
        (WINDOWS_874, "สวัสดีครับ นี่คือการทดสอบภาษาไทย"),
        (SHIFT_JIS, "こんにちは"),
        (GBK, "你好，世界，这是一个测试"),
        (EUC_KR, "안녕하세요"),
        (BIG5, "繁體"),
    ] {
        let (encoded, _, had_errors) = encoding.encode(sample);
        assert!(!had_errors, "invalid fixture for {}", encoding.name());
        assert_eq!(
            bytes_to_string_smart(encoded.as_ref()),
            sample,
            "encoding: {}",
            encoding.name()
        );
    }
}

#[test]
fn undecodable_output_falls_back_to_lossy_utf8() {
    let invalid_bytes = [0xFF, 0xFE, 0xFD];
    assert_eq!(
        bytes_to_string_smart(&invalid_bytes),
        String::from_utf8_lossy(&invalid_bytes)
    );
}
