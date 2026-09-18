use super::*;

pub(crate) fn exec_command_suite() {
    test_escape_command();
    test_strip_bash_lc_and_escape();
}

fn test_escape_command() {
    let args = vec!["foo".into(), "bar baz".into(), "weird&stuff".into()];
    let cmdline = escape_command(&args);
    assert_eq!(cmdline, "foo 'bar baz' 'weird&stuff'");
}

fn test_strip_bash_lc_and_escape() {
    // Test bash
    let args = vec!["bash".into(), "-lc".into(), "echo hello".into()];
    let cmdline = strip_bash_lc_and_escape(&args);
    assert_eq!(cmdline, "echo hello");

    // Test zsh
    let args = vec!["zsh".into(), "-lc".into(), "echo hello".into()];
    let cmdline = strip_bash_lc_and_escape(&args);
    assert_eq!(cmdline, "echo hello");

    // Test absolute path to zsh
    let args = vec!["/usr/bin/zsh".into(), "-lc".into(), "echo hello".into()];
    let cmdline = strip_bash_lc_and_escape(&args);
    assert_eq!(cmdline, "echo hello");

    // Test absolute path to bash
    let args = vec!["/bin/bash".into(), "-lc".into(), "echo hello".into()];
    let cmdline = strip_bash_lc_and_escape(&args);
    assert_eq!(cmdline, "echo hello");
}
