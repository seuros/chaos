use super::*;

#[test]
fn ioctl_bindings_match_installed_native_headers() {
    let root = tempfile::tempdir().unwrap();
    let executable = root.path().join("machine-abi");
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/freebsd_abi.c");
    let build = std::process::Command::new("cc")
        .args(["-Wall", "-Wextra", "-Werror", "-I/usr/local/include/libdrm"])
        .arg(source)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("native C compiler");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let output = std::process::Command::new(executable).output().unwrap();
    assert!(output.status.success());
    let values: Vec<usize> = String::from_utf8(output.stdout)
        .unwrap()
        .split_whitespace()
        .map(|value| value.parse().unwrap())
        .collect();
    assert_eq!(
        &values[..6],
        &[
            size_of::<BatteryInfo>(),
            size_of::<BatteryArgument>(),
            align_of::<BatteryArgument>(),
            std::mem::offset_of!(BatteryInfo, state),
            BATTERY_UNITS as usize,
            BATTERY_INFO as usize,
        ]
    );
    if values[6] == 1 {
        assert_eq!(
            &values[7..],
            &[
                size_of::<CardResources>(),
                size_of::<Connector>(),
                std::mem::offset_of!(Connector, connection),
                size_of::<[u32; 17]>(),
                CARD_RESOURCES as usize,
                CONNECTOR as usize,
            ]
        );
    }
}
