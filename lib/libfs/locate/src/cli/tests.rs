use super::*;
use std::ffi::OsStr;

#[test]
fn parses_defaults_and_repeated_excludes() {
    let argv = [
        OsStr::new("--exclude"),
        OsStr::new("target"),
        OsStr::new("--exclude"),
        OsStr::new(".git"),
        OsStr::new("needle"),
    ];

    let cli = Cli::parse_from(&argv).expect("valid locate arguments");

    assert_eq!(cli.limit.get(), 64);
    assert_eq!(cli.threads.get(), 2);
    assert_eq!(cli.exclude, ["target", ".git"]);
    assert_eq!(cli.pattern.as_deref(), Some("needle"));
}

#[test]
fn rejects_unknown_flags() {
    let argv = [OsStr::new("--unknown")];

    assert!(matches!(
        Cli::parse_from(&argv),
        Err(usage::Error::UnknownFlag { .. })
    ));
}
