use super::super::tests::output;
use super::*;

#[test]
fn failures_take_precedence_over_measurements() {
    for (success, stdout, stderr, expected) in [
        (
            false,
            "1 0.0 indexer",
            "Operation not permitted",
            Error::PermissionDenied,
        ),
        (true, "Permission denied", "", Error::PermissionDenied),
        (true, "", "Not authorized", Error::PermissionDenied),
        (
            false,
            "1 0.0 indexer",
            "",
            Error::Unavailable("probe exited unsuccessfully".into()),
        ),
    ] {
        let mut probe = output(stdout);
        probe.success = success;
        probe.stderr = stderr.as_bytes().to_vec();
        assert_eq!(processes(&probe, |_| true), Err(expected));
    }
    let mut probe = output("");
    probe.stdout = vec![0xff];
    assert!(matches!(
        validated_text(&probe, "invalid encoding"),
        Err(Error::Unavailable(_))
    ));
}

#[test]
fn malformed_tables_are_not_empty_measurements() {
    for text in [
        "",
        "not a process table",
        "1 0.0 unrelated\ntruncated row",
        "5 NaN indexer",
        "5 inf indexer",
        "5 -1.0 indexer",
        "5 0,1 indexer",
        "5 0.1 ",
        "oops 0.1 indexer",
    ] {
        assert!(
            matches!(
                processes(&output(text), |_| true),
                Err(Error::Unavailable(_))
            ),
            "{text:?}"
        );
    }
    assert_eq!(
        processes(&output("1 0.0 unrelated\n"), |_| false),
        Ok(vec![])
    );
}
