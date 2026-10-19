use super::*;

fn egress(proxy: &str) -> AntigravityEgress {
    AntigravityEgress {
        proxy_url: proxy.into(),
        ca_bundle_path: None,
    }
}

#[tokio::test]
async fn rejects_remote_ambient_or_invalid_proxy_endpoints() {
    for proxy in [
        "http://example.org:1234",
        "http://127.0.0.1:0",
        "http://127.0.0.1:1234/path",
        "http://127.0.0.1:1234?ignored",
        "http://localhost:1234",
        "https://127.0.0.1:1234",
        "http://user:secret@127.0.0.1:1234",
    ] {
        assert!(
            network_descriptor(proxy, Path::new("/private/bridge.sock"))
                .await
                .is_err(),
        );
    }
}

#[tokio::test]
async fn descriptor_does_not_enable_socks_local_binding_or_unix_wildcards() {
    let network = network_descriptor("http://127.0.0.1:43123", Path::new("/private/bridge.sock"))
        .await
        .expect("strict descriptor");
    assert_eq!(network.http_addr().port(), 43123);
    assert!(!network.allow_local_binding());
    assert!(!network.dangerously_allow_all_unix_sockets());
    let config = network.current_cfg().await.expect("frozen config");
    assert!(!config.network.enable_socks5);
    assert!(!config.network.allow_upstream_proxy);
}

#[tokio::test]
async fn protected_profiles_reject_overlaps_and_parent_traversal() {
    let home = Path::new("/private/agy");
    let protected = [home.join(".gemini/config")];
    for workspace in [home, Path::new("/private"), Path::new("/private/agy/work")] {
        assert!(
            prepare_antigravity_sandbox(
                Path::new("/usr/lib/chaos/alcatraz"),
                home,
                workspace,
                &egress("http://127.0.0.1:43123"),
                Path::new("/private/bridge.sock"),
                &protected,
                &[],
            )
            .await
            .is_err()
        );
    }
    for writable in [
        "/private/agy",
        "/private/agy/.gemini",
        "/private/agy/.gemini/config",
        "/private/agy/.gemini/config/skills",
        "/private/agy/.cache/../.gemini/config",
        "/other",
    ] {
        assert!(
            prepare_antigravity_sandbox(
                Path::new("/usr/lib/chaos/alcatraz"),
                home,
                Path::new("/srv/work"),
                &egress("http://127.0.0.1:43123"),
                Path::new("/private/bridge.sock"),
                &protected,
                &[PathBuf::from(writable)],
            )
            .await
            .is_err()
        );
    }
}

#[tokio::test]
async fn generates_host_platform_policy_with_protected_agent_configuration() {
    let sandbox = prepare_antigravity_sandbox(
        Path::new("/usr/lib/chaos/alcatraz"),
        Path::new("/private/agy"),
        Path::new("/srv/work"),
        &egress("http://127.0.0.1:43123"),
        Path::new("/private/bridge.sock"),
        &[PathBuf::from("/private/agy/.gemini/config")],
        &[PathBuf::from("/private/agy/.cache")],
    )
    .await
    .expect("platform policy");
    assert_eq!(sandbox.arg0.as_deref(), Some("alcatraz"));
    assert_eq!(sandbox.args.last().map(String::as_str), Some("--"));
    #[cfg(target_os = "macos")]
    {
        assert_eq!(sandbox.args.first().map(String::as_str), Some("-p"));
        let policy = &sandbox.args[1];
        assert!(policy.contains("localhost:43123"));
        assert!(!policy.contains("localhost:*"));
        assert!(!policy.contains("(allow network-bind (local ip"));
        assert!(!policy.contains("(allow network*)"));
        assert!(
            sandbox
                .args
                .iter()
                .any(|arg| arg.contains("/private/bridge.sock"))
        );
        // Protecting the entire profile root makes its policy subtree
        // implicitly read-only, without a redundant separate path parameter.
        assert!(
            sandbox
                .args
                .iter()
                .any(|arg| arg.contains("/private/agy/.cache"))
        );
        assert!(
            !sandbox
                .args
                .iter()
                .any(|arg| arg.starts_with("-DWRITABLE_ROOT") && arg.ends_with("=/private/agy"))
        );
    }
    #[cfg(target_os = "linux")]
    {
        assert!(
            sandbox
                .args
                .iter()
                .any(|arg| arg == "--allow-network-for-proxy")
        );
        assert!(
            sandbox
                .args
                .iter()
                .any(|arg| arg.contains("/private/agy/.gemini/config"))
        );
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    async fn probe(
        sandbox: &AntigravitySandbox,
        args: &[&std::ffi::OsStr],
    ) -> std::process::Output {
        let mut command = tokio::process::Command::new("/usr/bin/sandbox-exec");
        command
            .args(&sandbox.args)
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        tokio::time::timeout(std::time::Duration::from_secs(10), command.output())
            .await
            .expect("bounded OS probe")
            .expect("run sandbox-exec")
    }

    #[ignore = "requires a host permitting nested Seatbelt and loopback fixtures"]
    #[tokio::test]
    async fn os_policy_allows_only_exact_proxy_and_rejects_local_binding() {
        let permitted = std::net::TcpListener::bind("127.0.0.1:0").expect("proxy fixture");
        let prohibited = std::net::TcpListener::bind("127.0.0.1:0").expect("bypass fixture");
        let directory = tempfile::tempdir().expect("empty private home");
        let sandbox = prepare_antigravity_sandbox(
            Path::new("/usr/bin/sandbox-exec"),
            directory.path(),
            directory.path(),
            &egress(&format!(
                "http://127.0.0.1:{}",
                permitted.local_addr().expect("proxy address").port()
            )),
            &directory.path().join("bridge.sock"),
            &[],
            &[],
        )
        .await
        .expect("OS policy");
        let script = r#"
import socket, sys
with socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=1):
    pass
try:
    socket.create_connection(("127.0.0.1", int(sys.argv[2])), timeout=1)
except OSError:
    pass
else:
    raise SystemExit(21)
try:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
except OSError:
    pass
else:
    raise SystemExit(22)
print("STRICT_NETWORK_OK")
"#;
        let permitted_port = permitted
            .local_addr()
            .expect("proxy address")
            .port()
            .to_string();
        let prohibited_port = prohibited
            .local_addr()
            .expect("bypass address")
            .port()
            .to_string();
        let result = probe(
            &sandbox,
            &[
                std::ffi::OsStr::new("/usr/bin/python3"),
                std::ffi::OsStr::new("-c"),
                std::ffi::OsStr::new(script),
                std::ffi::OsStr::new(&permitted_port),
                std::ffi::OsStr::new(&prohibited_port),
            ],
        )
        .await;
        assert!(
            result.status.success(),
            "OS network probe failed: exit={:?}; sandbox_apply_denied={}",
            result.status.code(),
            String::from_utf8_lossy(&result.stderr).contains("sandbox_apply"),
        );
        assert_eq!(result.stdout, b"STRICT_NETWORK_OK\n");
    }

    #[ignore = "requires a host permitting nested Seatbelt; private fixtures only"]
    #[tokio::test]
    async fn os_policy_protects_hook_file_and_ancestor_rename() {
        let directory = tempfile::tempdir().expect("private home");
        let home = std::fs::canonicalize(directory.path()).expect("canonical home");
        let workspace = tempfile::tempdir().expect("separate workspace");
        let config = home.join(".gemini/config");
        std::fs::create_dir_all(&config).expect("policy directory");
        let hooks = config.join("hooks.json");
        std::fs::write(&hooks, "IMMUTABLE_POLICY").expect("policy fixture");
        std::fs::create_dir(home.join(".cache")).expect("writable state");
        let sandbox = prepare_antigravity_sandbox(
            Path::new("/usr/bin/sandbox-exec"),
            &home,
            workspace.path(),
            &egress("http://127.0.0.1:43123"),
            &home.join("bridge.sock"),
            std::slice::from_ref(&config),
            &[home.join(".cache")],
        )
        .await
        .expect("OS policy");
        let script = r#"
import os, sys
home = sys.argv[1]
operations = [
    lambda: open(home + "/.gemini/config/hooks.json", "w").write("REPLACED"),
    lambda: os.unlink(home + "/.gemini/config/hooks.json"),
    lambda: os.rename(home + "/.gemini/config", home + "/.gemini/moved-config"),
    lambda: os.rename(home + "/.gemini", home + "/moved-gemini"),
    lambda: os.rename(home, home + "-moved"),
    lambda: os.link(home + "/.gemini/config/hooks.json", home + "/.cache/hardlink"),
]
for index, operation in enumerate(operations):
    try:
        operation()
    except OSError:
        continue
    raise SystemExit(30 + index)
with open(home + "/.cache/mutable-state", "w") as f:
    f.write("STATE_WRITE_OK")
print("POLICY_PROTECTED")
"#;
        let result = probe(
            &sandbox,
            &[
                std::ffi::OsStr::new("/usr/bin/python3"),
                std::ffi::OsStr::new("-c"),
                std::ffi::OsStr::new(script),
                home.as_os_str(),
            ],
        )
        .await;
        assert!(
            result.status.success(),
            "OS policy protection failed: exit={:?}; sandbox_apply_denied={}",
            result.status.code(),
            String::from_utf8_lossy(&result.stderr).contains("sandbox_apply"),
        );
        assert_eq!(result.stdout, b"POLICY_PROTECTED\n");
        assert_eq!(
            std::fs::read_to_string(hooks).expect("policy remains"),
            "IMMUTABLE_POLICY"
        );
    }
}
