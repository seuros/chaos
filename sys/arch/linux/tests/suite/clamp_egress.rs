#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

//! End-to-end proof that a clamped subprocess has exactly one route off the
//! machine: the allowlisting CONNECT proxy.
//!
//! The two halves are checked together because neither is sufficient alone.
//! Landlock rules are port-scoped, so the sandbox can only say "this loopback
//! port"; the proxy is what decides which hosts that port reaches.

use std::io::Read;
use std::io::Write;
use std::net::Ipv4Addr;
use std::net::TcpListener;
use std::process::Output;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use chaos_clamp::EgressPolicy;
use chaos_clamp::EgressProxy;
use chaos_clamp::FileWiretapSink;
use chaos_clamp::WiretapSink;
use tokio::process::Command;

const TIMEOUT_MS: u64 = 10_000;

fn landlock_supported() -> bool {
    use landlock::ABI;
    use landlock::Access;
    use landlock::AccessFs;
    use landlock::CompatLevel;
    use landlock::Compatible;
    use landlock::Ruleset;
    use landlock::RulesetAttr;
    Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(ABI::V1))
        .and_then(Ruleset::create)
        .is_ok()
}

/// Runs a shell probe inside the Antigravity sandbox invocation, with the
/// proxy environment the clamp transport would set.
async fn run_probe(
    sandbox: &chaos_clamp::AntigravitySandbox,
    proxy_url: &str,
    script: &str,
) -> Output {
    let cwd = std::env::current_dir().expect("cwd should exist");
    let mut args = sandbox.args.clone();
    args.push("bash".to_string());
    args.push("-c".to_string());
    args.push(script.to_string());

    let mut command = Command::new(&sandbox.program);
    command
        .args(args)
        .arg0("alcatraz-linux")
        .current_dir(&cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HTTPS_PROXY", proxy_url)
        .env("HTTP_PROXY", proxy_url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    tokio::time::timeout(Duration::from_millis(TIMEOUT_MS), command.output())
        .await
        .expect("probe should not time out")
        .expect("probe should execute")
}

#[tokio::test]
async fn sandboxed_egress_reaches_allowlisted_hosts_only_through_the_proxy() {
    if !landlock_supported() {
        eprintln!("skipping: kernel does not support Landlock");
        return;
    }

    // Stands in for an upstream service. Reachable by name from the proxy,
    // and deliberately unreachable directly from inside the sandbox.
    let upstream = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("skipping: loopback bind is not permitted: {error}");
            return;
        }
    };
    let upstream_port = upstream.local_addr().expect("upstream addr").port();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = upstream.accept() else {
            return;
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("set read timeout");
        let mut buf = [0_u8; 4];
        if stream.read_exact(&mut buf).is_ok() {
            let _ = stream.write_all(b"pong");
        }
    });

    // Relay rather than inspect: the stand-in upstream speaks a bare protocol,
    // not TLS. The host allowlist is enforced either way.
    let sink: Arc<dyn WiretapSink> = Arc::new(FileWiretapSink::new(None));
    let proxy = EgressProxy::start(
        EgressPolicy::new(["localhost"]).without_body_inspection(),
        sink,
        None,
    )
    .await
    .expect("start egress proxy");
    let proxy_url = proxy.proxy_url();

    let temporary_home = tempfile::tempdir().expect("create sandbox home");
    let cwd = std::env::current_dir().expect("cwd should exist");
    let sandbox = chaos_kern::clamp_egress::antigravity_sandbox(
        std::path::Path::new(env!("CARGO_BIN_EXE_alcatraz-linux")),
        Some(temporary_home.path()),
        &cwd,
    )
    .expect("build sandbox invocation");

    // 1. A direct connection to the upstream, bypassing the proxy, is refused
    //    by the sandbox even though the listener is accepting.
    let direct = run_probe(
        &sandbox,
        &proxy_url,
        &format!("exec 3<>/dev/tcp/127.0.0.1/{upstream_port}"),
    )
    .await;
    assert!(
        !direct.status.success(),
        "direct connection should be blocked; stderr={}",
        String::from_utf8_lossy(&direct.stderr)
    );

    // 2. The same destination through the proxy completes a full round trip.
    let tunneled = run_probe(
        &sandbox,
        &proxy_url,
        &format!(
            r#"exec 3<>/dev/tcp/127.0.0.1/{proxy_port}
printf 'CONNECT localhost:{upstream_port} HTTP/1.1\r\nHost: localhost:{upstream_port}\r\n\r\n' >&3
status=""
while IFS= read -r line <&3; do
  line="${{line%$'\r'}}"
  [ -z "$status" ] && status="$line"
  [ -z "$line" ] && break
done
printf 'ping' >&3
IFS= read -r -N 4 body <&3
printf '%s|%s\n' "$status" "$body""#,
            proxy_port = proxy.port(),
        ),
    )
    .await;
    let stdout = String::from_utf8_lossy(&tunneled.stdout);
    assert!(
        stdout.contains("200") && stdout.contains("pong"),
        "expected a completed tunnel; stdout={stdout}; stderr={}",
        String::from_utf8_lossy(&tunneled.stderr)
    );

    // 3. A destination outside the allowlist is refused by the proxy before a
    //    connection to it exists.
    let blocked = run_probe(
        &sandbox,
        &proxy_url,
        &format!(
            r#"exec 3<>/dev/tcp/127.0.0.1/{proxy_port}
printf 'CONNECT generativelanguage.googleapis.com:443 HTTP/1.1\r\nHost: generativelanguage.googleapis.com:443\r\n\r\n' >&3
IFS= read -r status <&3
printf '%s\n' "$status""#,
            proxy_port = proxy.port(),
        ),
    )
    .await;
    let stdout = String::from_utf8_lossy(&blocked.stdout);
    assert!(
        stdout.contains("403"),
        "expected the proxy to refuse a non-allowlisted host; stdout={stdout}; stderr={}",
        String::from_utf8_lossy(&blocked.stderr)
    );

    proxy.shutdown();
}
