use super::CargoBinError;
use super::cargo_bin;

#[test]
fn missing_binary_fails_fast_with_the_fix() {
    let name = "chaos-which-no-such-binary";
    let error = cargo_bin(name).expect_err("binary must not exist");
    let CargoBinError::NotFound { expected, .. } = &error else {
        panic!("unexpected error: {error}");
    };
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    assert!(expected.ends_with(&file), "{expected:?}");
    let message = error.to_string();
    assert!(message.contains("cargo build --bin chaos-which-no-such-binary"));
    assert!(message.contains("CARGO_BIN_EXE_chaos-which-no-such-binary="));
}
