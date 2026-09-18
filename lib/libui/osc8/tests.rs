use super::*;

fn register_roundtrips_url_via_lookup() {
    let color = register("https://example.com/a");
    assert_eq!(lookup(color).as_deref(), Some("https://example.com/a"));
}

pub(crate) fn osc8_suite() {
    register_roundtrips_url_via_lookup();
    register_deduplicates_identical_urls();
    lookup_ignores_non_rgb_and_zero_sentinel();
    open_and_close_use_st_terminator();
}
#[cfg(test)]
fn register_deduplicates_identical_urls() {
    let a = register("https://example.com/dedup");
    let b = register("https://example.com/dedup");
    assert_eq!(a, b);
}

#[cfg(test)]
fn lookup_ignores_non_rgb_and_zero_sentinel() {
    assert!(lookup(Color::Reset).is_none());
    assert!(lookup(Color::Rgb(0, 0, 0)).is_none());
}

#[cfg(test)]
fn open_and_close_use_st_terminator() {
    assert_eq!(
        open("https://example.com"),
        "\x1b]8;;https://example.com\x1b\\"
    );
    assert_eq!(close(), "\x1b]8;;\x1b\\");
}
