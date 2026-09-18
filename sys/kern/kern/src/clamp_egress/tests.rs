use super::*;

#[test]
fn sandbox_args_confine_writes_and_request_proxy_only_networking() {
    let sandbox = antigravity_sandbox(
        Path::new("/usr/lib/chaos/alcatraz"),
        Some(Path::new("/private/agy")),
        Path::new("/srv/work"),
    )
    .expect("build sandbox invocation");

    assert_eq!(sandbox.program, Path::new("/usr/lib/chaos/alcatraz"));
    assert_eq!(sandbox.args.last().map(String::as_str), Some("--"));
    assert!(
        sandbox
            .args
            .iter()
            .any(|arg| arg == "--allow-network-for-proxy")
    );
    let file_system_policy = sandbox
        .args
        .iter()
        .position(|arg| arg == "--file-system-sandbox-policy")
        .and_then(|index| sandbox.args.get(index + 1))
        .expect("filesystem policy argument");
    assert!(file_system_policy.contains("/private/agy"));
    assert!(file_system_policy.contains("/srv/work"));
}
