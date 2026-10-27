use super::*;
use std::io::Read;

fn request(script: &str) -> CommandRequest {
    CommandRequest {
        filesystem: VfsPolicy::default(),
        network: SocketPolicy::Restricted,
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        environment: BTreeMap::new(),
        cwd: PathBuf::from("/"),
        policy_cwd: PathBuf::from("/"),
    }
}

#[test]
fn command_bounds_and_environment_are_explicit() {
    let mut input = request("exit 0");
    assert!(input.validate().is_ok());
    input.environment.insert("".into(), "value".into());
    assert!(input.validate().is_err());
    input.environment.clear();
    input.argv[0] = "relative".into();
    assert!(input.validate().is_err());
    input.argv[0] = "/bin/sh".into();
    input.argv.push("x".repeat(1024 * 1024));
    assert!(input.validate().is_err());
    input.argv.pop();
    input.argv.push("bad\0argument".into());
    assert!(input.validate().is_err());
}

fn start_native(input: &CommandRequest) -> CommandSession {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    input.validate().unwrap();
    let plan = warden_plan::compile(&input.filesystem, input.network, &input.cwd).unwrap();
    let identity = CommandIdentity::new(65534, 65534, vec![65534]).unwrap();
    // Only this cfg(test) path omits MAC adoption. Public start has no bypass.
    CommandSession::start_inner(input, &identity, Path::new("/tmp"), &plan.filesystem, None)
        .unwrap()
}

#[test]
#[ignore = "explicit root-only native Warden lifecycle test; no custom MAC module"]
fn native_session_exit_removes_descendants_before_pipe_eof_and_cleans_mounts() {
    let mut session = start_native(&request("echo ready; /bin/sleep 60 & exit 0"));
    let state = session.state_path().to_owned();
    let mut pipes = session.take_io();
    drop(pipes.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if session.poll_exit().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut output = String::new();
    pipes
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    assert_eq!(output, "ready\n");
    assert!(
        session
            .finish_cleanup(Duration::from_secs(5))
            .unwrap()
            .success()
    );
    assert!(!state.exists());
}

#[test]
#[ignore = "explicit root-only native Warden cancellation test; no custom MAC module"]
fn native_session_cancellation_reaps_primary_and_removes_the_view() {
    use std::os::unix::process::ExitStatusExt;
    let mut session = start_native(&request("exec /bin/sleep 60"));
    let state = session.state_path().to_owned();
    assert_eq!(
        session.finish_cleanup(Duration::ZERO).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let status = session.cancel_and_cleanup(Duration::from_secs(5)).unwrap();
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert!(!state.exists());
}
