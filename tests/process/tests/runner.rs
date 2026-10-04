#![cfg(unix)]

use std::error::Error;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

fn fixture() -> Command {
    Command::new(env!("CARGO_BIN_EXE_chaos-test-process"))
}

#[test]
fn runs_without_an_external_interpreter() -> TestResult {
    let output = fixture()
        .env("PATH", "/no-external-interpreters")
        .args(["--eval", "print('internal lua')"])
        .output()?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"internal lua\n");
    Ok(())
}

#[test]
fn shell_wrappers_preserve_script_arguments() -> TestResult {
    let text = "literal '$HOME' \"quoted\"; \n second line";
    let script = format!("print({text:?})");
    let argv = chaos_test_process::command(&script)?;
    let output = Command::new("/bin/sh")
        .args(["-c", "exec \"$@\"", "fixture-wrapper"])
        .args(&argv)
        .output()?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout)?, format!("{text}\n"));

    let output = Command::new("/bin/sh")
        .args(["-c", &chaos_test_process::shell_command(&script)?])
        .output()?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout)?, format!("{text}\n"));
    Ok(())
}

#[test]
fn repl_preserves_state_and_exits() -> TestResult {
    let mut child = fixture()
        .arg("--interactive")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child.stdin.take().ok_or("missing fixture stdin")?;
    input.write_all(b"answer = 41\nprint(answer + 1)\nexit()\n")?;
    drop(input);
    let output = child.wait_with_output()?;
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout)?;
    assert!(text.starts_with("> "), "{text:?}");
    assert!(text.contains("42\n"), "{text:?}");
    Ok(())
}

#[test]
fn failed_scripts_fail_the_process() -> TestResult {
    let output = fixture()
        .args(["--eval", "error('fixture failure')"])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains("fixture failure"));
    Ok(())
}

#[test]
fn native_probes_work_without_language_bindings() -> TestResult {
    let output = fixture()
        .args([
            "--eval",
            "assert(fixture.getpwuid() ~= ''); assert(fixture.openpty_roundtrip() == 'ping'); fixture.fork_semaphore(); assert(fixture.socket(fixture.AF_UNIX, fixture.SOCK_STREAM, 0)); assert(not fixture.stdin_isatty())",
        ])
        .stdin(Stdio::null())
        .output()?;
    assert!(output.status.success(), "{output:?}");
    Ok(())
}

fn http_server(status: &str, body: &str) -> io::Result<(String, JoinHandle<io::Result<String>>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let url = format!("http://{}", listener.local_addr()?);
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let server = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "no HTTP request"));
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => return Err(err),
            }
        };
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let read = stream.read(&mut buffer)?;
            if read == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "HTTP headers"));
            }
            request.extend_from_slice(&buffer[..read]);
        }
        stream.write_all(response.as_bytes())?;
        String::from_utf8(request).map_err(io::Error::other)
    });
    Ok((url, server))
}

#[test]
fn direct_http_ignores_proxy_environment() -> TestResult {
    let (url, server) = http_server("200 OK", "direct")?;
    let output = fixture()
        .env_clear()
        .env("http_proxy", "http://127.0.0.1:9")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .args([
            "--eval",
            &format!("print(fixture.http_get({url:?}, false, 2))"),
        ])
        .output()?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"direct\n");
    let request = server
        .join()
        .map_err(|_| io::Error::other("HTTP server panicked"))??;
    assert!(request.starts_with("GET / HTTP/1.1\r\n"), "{request}");
    Ok(())
}

#[test]
fn proxy_http_uses_absolute_form_without_resolving_origin() -> TestResult {
    let (proxy, server) = http_server("200 OK", "proxied")?;
    let output = fixture()
        .env_clear()
        .env("HTTP_PROXY", proxy)
        .args([
            "--eval",
            "print(fixture.http_get('http://fixture.invalid/path?query=1', true, 2))",
        ])
        .output()?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"proxied\n");
    let request = server
        .join()
        .map_err(|_| io::Error::other("HTTP server panicked"))??;
    assert!(
        request.starts_with("GET http://fixture.invalid/path?query=1 HTTP/1.1\r\n"),
        "{request}"
    );
    assert!(
        request.contains("\r\nhost: fixture.invalid\r\n"),
        "{request}"
    );
    Ok(())
}

#[test]
fn no_proxy_bypasses_the_configured_proxy() -> TestResult {
    let (url, server) = http_server("200 OK", "bypassed")?;
    let output = fixture()
        .env_clear()
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("NO_PROXY", "127.0.0.1")
        .args([
            "--eval",
            &format!("print(fixture.http_get({url:?}, true, 2))"),
        ])
        .output()?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"bypassed\n");
    let request = server
        .join()
        .map_err(|_| io::Error::other("HTTP server panicked"))??;
    assert!(request.starts_with("GET / HTTP/1.1\r\n"), "{request}");
    Ok(())
}

#[test]
fn http_failure_is_not_reported_as_success() -> TestResult {
    let (url, server) = http_server("403 Forbidden", "denied")?;
    let output = fixture()
        .env_clear()
        .args([
            "--eval",
            &format!("print(fixture.http_get({url:?}, false, 2))"),
        ])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains("HTTP 403"));
    server
        .join()
        .map_err(|_| io::Error::other("HTTP server panicked"))??;
    Ok(())
}
