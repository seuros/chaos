use super::*;

#[tokio::test]
async fn sandbox_args_confine_writes_and_request_proxy_only_networking() {
    let sandbox = antigravity_sandbox(
        Path::new("/usr/lib/chaos/alcatraz"),
        Some(Path::new("/private/agy")),
        Path::new("/srv/work"),
        &AntigravityEgress {
            proxy_url: "http://127.0.0.1:43123".into(),
            ca_bundle_path: None,
        },
        Path::new("/private/bridge.sock"),
    )
    .await
    .expect("build sandbox invocation");

    assert_eq!(sandbox.program, Path::new("/usr/lib/chaos/alcatraz"));
    assert_eq!(sandbox.args.last().map(String::as_str), Some("--"));
    #[cfg(target_os = "linux")]
    {
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
    #[cfg(target_os = "macos")]
    {
        assert_eq!(sandbox.args.first().map(String::as_str), Some("-p"));
        assert!(sandbox.args[1].contains("localhost:43123"));
        assert!(!sandbox.args[1].contains("localhost:*"));
    }
}
