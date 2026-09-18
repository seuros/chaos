use super::*;

#[test]
fn kmg() {
    let formatter = make_en_us_formatter();
    let fmt = |n: i64| format_si_suffix_with_formatter(n, &formatter);
    assert_eq!(fmt(0), "0");
    assert_eq!(fmt(999), "999");
    assert_eq!(fmt(1_000), "1.00K");
    assert_eq!(fmt(1_200), "1.20K");
    assert_eq!(fmt(10_000), "10.0K");
    assert_eq!(fmt(100_000), "100K");
    assert_eq!(fmt(999_500), "1.00M");
    assert_eq!(fmt(1_000_000), "1.00M");
    assert_eq!(fmt(1_234_000), "1.23M");
    assert_eq!(fmt(12_345_678), "12.3M");
    assert_eq!(fmt(999_950_000), "1.00G");
    assert_eq!(fmt(1_000_000_000), "1.00G");
    assert_eq!(fmt(1_234_000_000), "1.23G");
    // Above 1000G we keep whole‑G precision (no higher unit supported here).
    assert_eq!(fmt(1_234_000_000_000), "1,234G");
}
