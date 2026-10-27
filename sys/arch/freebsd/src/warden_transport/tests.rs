use super::*;
use std::collections::BTreeMap;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

fn request(script: &str) -> CommandRequest {
    CommandRequest {
        filesystem: chaos_ipc::protocol::VfsPolicy::default(),
        network: chaos_ipc::protocol::SocketPolicy::Restricted,
        argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
        environment: BTreeMap::new(),
        cwd: "/".into(),
        policy_cwd: "/".into(),
    }
}

#[test]
fn wire_rejects_identity_fields_unknown_versions_and_mismatched_rights() {
    let bytes = serde_json::to_vec(&Request {
        version: VERSION,
        command: request("exit 0"),
    })
    .unwrap();
    let mut json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    json["uid"] = serde_json::json!(0);
    assert!(serde_json::from_value::<Request>(json).is_err());

    for (version, count) in [(VERSION + 1, 3), (VERSION, 2), (VERSION, 4)] {
        let (client, worker) = Channel::pair().unwrap();
        let (reply, server_reply) = Channel::pair().unwrap();
        let file = std::fs::File::open("/dev/null").unwrap();
        let mut fds = vec![server_reply.as_fd()];
        fds.extend(std::iter::repeat_n(file.as_fd(), count));
        let bytes = serde_json::to_vec(&Request {
            version,
            command: request("exit 0"),
        })
        .unwrap();
        client.send(&bytes, &fds).unwrap();
        assert!(
            serve_one_with(&worker, None, Duration::from_secs(1), |_, _| {
                panic!("invalid request reached command setup")
            })
            .unwrap()
        );
        let response = PendingCommand { reply }.wait().unwrap();
        assert!(matches!(response, Response::Error { .. }));
    }
}

#[test]
fn request_specific_replies_cannot_be_read_by_another_client() {
    let (first, first_server) = Channel::pair().unwrap();
    let (second, second_server) = Channel::pair().unwrap();
    Response::Exited {
        code: Some(12),
        signal: None,
    }
    .send(&second_server)
    .unwrap();
    Response::Exited {
        code: Some(7),
        signal: None,
    }
    .send(&first_server)
    .unwrap();
    assert_eq!(
        PendingCommand { reply: first }.wait().unwrap(),
        Response::Exited {
            code: Some(7),
            signal: None
        }
    );
    assert_eq!(
        PendingCommand { reply: second }.wait().unwrap(),
        Response::Exited {
            code: Some(12),
            signal: None
        }
    );
}

#[test]
fn abandoning_request_is_cancellation_without_touching_shared_dispatcher() {
    let (client, worker) = Channel::pair().unwrap();
    assert!(!cancelled(&worker, None, 0).unwrap());
    drop(PendingCommand { reply: client });
    assert!(cancelled(&worker, None, 0).unwrap());
}

#[test]
fn stdio_directories_and_invalid_exit_status_are_refused() {
    let directory = std::fs::File::open("/").unwrap();
    assert!(validate_stdio(&[directory.as_fd(); 3]).is_err());
    let (reply, worker) = Channel::pair().unwrap();
    Response::Exited {
        code: Some(0),
        signal: Some(9),
    }
    .send(&worker)
    .unwrap();
    assert!(PendingCommand { reply }.wait().is_err());
}

// The root test execs a fresh process running only this fixture as UID65534.
// No allocations, library runtime or test code run between fork and exec.
#[test]
#[ignore = "internal subprocess fixture for explicit root-only transport tests"]
fn transport_client_fixture() {
    let fd = std::env::var("CHAOS_WARDEN_TEST_FD")
        .expect("internal fixture requires its parent-created transport capability");
    assert_eq!(unsafe { libc::geteuid() }, 65534);
    let fd: RawFd = fd.parse().unwrap();
    let channel = Channel::from_owned(unsafe { OwnedFd::from_raw_fd(fd) }).unwrap();
    let stdio = unsafe {
        [
            BorrowedFd::borrow_raw(0),
            BorrowedFd::borrow_raw(1),
            BorrowedFd::borrow_raw(2),
        ]
    };
    let script = std::env::var("CHAOS_WARDEN_TEST_SCRIPT").unwrap();
    let pending = submit(&channel, request(&script), stdio).unwrap();
    if std::env::var_os("CHAOS_WARDEN_TEST_CANCEL").is_some() {
        let ready: RawFd = std::env::var("CHAOS_WARDEN_TEST_READY_FD")
            .unwrap()
            .parse()
            .unwrap();
        let ready = Channel::from_owned(unsafe { OwnedFd::from_raw_fd(ready) }).unwrap();
        assert_eq!(ready.receive().unwrap().unwrap().bytes, b"running");
        pending.cancel().unwrap();
    } else {
        assert_eq!(
            pending.wait().unwrap(),
            Response::Exited {
                code: Some(23),
                signal: None
            }
        );
    }
}

fn launch_client(endpoint: &Channel, ready: Option<&Channel>) -> std::process::Child {
    let fd = endpoint.as_fd().as_raw_fd();
    let ready_fd = ready.map(|channel| channel.as_fd().as_raw_fd());
    let cancel = ready.is_some();
    let group = 65534_u32;
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "warden_transport::tests::transport_client_fixture",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("CHAOS_WARDEN_TEST_FD", fd.to_string())
        .env(
            "CHAOS_WARDEN_TEST_SCRIPT",
            if cancel {
                "exec /bin/sleep 60"
            } else {
                "printf 'transport-output\\n'; test ! -e /dev/zfs; exit 23"
            },
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if cancel {
        command.env("CHAOS_WARDEN_TEST_CANCEL", "1");
        command.env("CHAOS_WARDEN_TEST_READY_FD", ready_fd.unwrap().to_string());
    }
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(1, std::ptr::addr_of!(group)) != 0
                || libc::setresgid(65534, 65534, 65534) != 0
                || libc::setresuid(65534, 65534, 65534) != 0
                || libc::fcntl(fd, libc::F_SETFD, 0) != 0
            {
                return Err(io::Error::last_os_error());
            }
            if let Some(fd) = ready_fd
                && libc::fcntl(fd, libc::F_SETFD, 0) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().unwrap()
}

#[test]
#[ignore = "explicit root-only private transport/jail/stdio test; no custom MAC module"]
fn native_transport_drops_identity_executes_and_preserves_output_and_status() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let (client, worker) = Channel::pair().unwrap();
    let child = launch_client(&client, None);
    drop(client);
    let identity = CommandIdentity::new(65534, 65534, vec![65534]).unwrap();
    assert!(
        serve_one_with(&worker, None, Duration::from_secs(5), |request, stdio| {
            CommandSession::start_transport_test(request, &identity, Path::new("/tmp"), stdio)
        })
        .unwrap()
    );
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("transport-output\n"));
}

#[test]
#[ignore = "explicit root-only private transport cancellation test; no custom MAC module"]
fn native_transport_client_cancellation_never_leaves_a_running_jail() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let (client, worker) = Channel::pair().unwrap();
    let (ready_client, ready_worker) = Channel::pair().unwrap();
    let child = launch_client(&client, Some(&ready_client));
    drop(client);
    drop(ready_client);
    let identity = CommandIdentity::new(65534, 65534, vec![65534]).unwrap();
    assert!(
        serve_one_with(&worker, None, Duration::from_secs(5), |request, stdio| {
            let session =
                CommandSession::start_transport_test(request, &identity, Path::new("/tmp"), stdio)?;
            ready_worker.send(b"running", &[])?;
            Ok(session)
        })
        .unwrap()
    );
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "explicit root-only rebuilt Alcatraz RPC test; set CHAOS_FREEBSD_TEST_HELPER"]
fn native_rebuilt_helper_executes_remote_readonly_and_preserves_signal_status() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let helper = std::env::var_os("CHAOS_FREEBSD_TEST_HELPER")
        .expect("set CHAOS_FREEBSD_TEST_HELPER to freshly built absolute helper path");
    for (script, code, signal) in [
        (
            "test -z \"$CHAOS_WARDEN_CONTROL_FD\" && printf 'real-helper\\n'; exit 37",
            Some(37),
            None,
        ),
        ("kill -TERM $$", None, Some(libc::SIGTERM)),
    ] {
        let (client, worker) = Channel::pair().unwrap();
        let fd = client.as_fd().as_raw_fd();
        let group = 65534_u32;
        let mut command = Command::new(&helper);
        command
            .arg0("alcatraz")
            .env_clear()
            .env(crate::warden_client::CONTROL_ENV, fd.to_string())
            .env("PATH", "/bin:/usr/bin:/sbin:/usr/sbin")
            .args(["--sandbox-policy-cwd", "/", "--file-system-sandbox-policy"])
            .arg(serde_json::to_string(&chaos_ipc::protocol::VfsPolicy::default()).unwrap())
            .arg("--network-sandbox-policy")
            .arg(serde_json::to_string(&chaos_ipc::protocol::SocketPolicy::Restricted).unwrap())
            .args(["--", "/bin/sh", "-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            command.pre_exec(move || {
                if libc::setgroups(1, std::ptr::addr_of!(group)) != 0
                    || libc::setresgid(group, group, group) != 0
                    || libc::setresuid(group, group, group) != 0
                    || libc::fcntl(fd, libc::F_SETFD, 0) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        drop(client);
        let identity = CommandIdentity::new(group, group, vec![group]).unwrap();
        assert!(
            serve_one_with(&worker, None, Duration::from_secs(5), |request, stdio| {
                CommandSession::start_transport_test(request, &identity, Path::new("/tmp"), stdio)
            })
            .unwrap()
        );
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            output.status.code(),
            code,
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.status.signal(), signal);
        if code.is_some() {
            assert_eq!(output.stdout, b"real-helper\n");
        }
    }
}

#[test]
#[ignore = "explicit root-only early harness bootstrap test; set CHAOS_FREEBSD_TEST_HARNESS_PROBE and HELPER"]
fn native_harness_bootstrap_capture_and_helper_only_inheritance() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    let probe = std::env::var_os("CHAOS_FREEBSD_TEST_HARNESS_PROBE")
        .expect("build the warden_harness_probe example");
    let helper = std::env::var_os("CHAOS_FREEBSD_TEST_HELPER").expect("build the Alcatraz helper");
    let (client, worker) = Channel::pair().unwrap();
    let fd = client.as_fd().as_raw_fd();
    let group = 65534_u32;
    let mut command = Command::new(probe);
    command
        .arg(helper)
        .env_clear()
        .env(crate::warden_client::CONTROL_ENV, fd.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(1, std::ptr::addr_of!(group)) != 0
                || libc::setresgid(group, group, group) != 0
                || libc::setresuid(group, group, group) != 0
                || libc::fcntl(fd, libc::F_SETFD, 0) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().unwrap();
    drop(client);
    let identity = CommandIdentity::new(group, group, vec![group]).unwrap();
    assert!(
        serve_one_with(&worker, None, Duration::from_secs(5), |request, stdio| {
            CommandSession::start_transport_test(request, &identity, Path::new("/tmp"), stdio)
        })
        .unwrap()
    );
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"harness-probe\n");
}
