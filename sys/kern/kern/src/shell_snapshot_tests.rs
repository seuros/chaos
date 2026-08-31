use super::*;
use chaos_ipc::protocol::SessionSource;
use chaos_journald::CreateProcessInput;
use pretty_assertions::assert_eq;
use std::process::Command;
#[cfg(target_os = "linux")]
use std::process::Command as StdCommand;

use tempfile::tempdir;

struct BlockingStdinPipe {
    original: i32,
    write_end: i32,
}

impl BlockingStdinPipe {
    fn install() -> Result<Self> {
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } == -1 {
            return Err(std::io::Error::last_os_error()).context("create stdin pipe");
        }

        let original = unsafe { libc::dup(libc::STDIN_FILENO) };
        if original == -1 {
            let err = std::io::Error::last_os_error();
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(err).context("dup stdin");
        }

        if unsafe { libc::dup2(fds[0], libc::STDIN_FILENO) } == -1 {
            let err = std::io::Error::last_os_error();
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
                libc::close(original);
            }
            return Err(err).context("replace stdin");
        }

        unsafe {
            libc::close(fds[0]);
        }

        Ok(Self {
            original,
            write_end: fds[1],
        })
    }
}

impl Drop for BlockingStdinPipe {
    fn drop(&mut self) {
        unsafe {
            libc::dup2(self.original, libc::STDIN_FILENO);
            libc::close(self.original);
            libc::close(self.write_end);
        }
    }
}

fn assert_posix_snapshot_sections(snapshot: &str) {
    assert!(snapshot.contains("# Snapshot file"));
    assert!(snapshot.contains("aliases "));
    assert!(snapshot.contains("exports "));
    assert!(
        snapshot.contains("PATH"),
        "snapshot should capture a PATH export"
    );
    assert!(snapshot.contains("setopts "));
}

async fn get_snapshot(shell_type: ShellType) -> Result<String> {
    let dir = tempdir()?;
    let path = dir.path().join("snapshot.sh");
    write_shell_snapshot(shell_type, &path, dir.path()).await?;
    let content = fs::read_to_string(&path).await?;
    Ok(content)
}

#[test]
fn strip_snapshot_preamble_removes_leading_output() {
    let snapshot = "noise\n# Snapshot file\nexport PATH=/bin\n";
    let cleaned = strip_snapshot_preamble(snapshot).expect("snapshot marker exists");
    assert_eq!(cleaned, "# Snapshot file\nexport PATH=/bin\n");
}

#[test]
fn strip_snapshot_preamble_requires_marker() {
    let result = strip_snapshot_preamble("missing header");
    assert!(result.is_err());
}

#[test]
fn bash_snapshot_filters_invalid_exports() -> Result<()> {
    let output = Command::new("/bin/bash")
        .arg("-c")
        .arg(bash_snapshot_script())
        .env("BASH_ENV", "/dev/null")
        .env("VALID_NAME", "ok")
        .env("PWD", "/tmp/stale")
        .env("NEXTEST_BIN_EXE_some-test-binary", "/path/to/bin")
        .env("BAD-NAME", "broken")
        .output()?;

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("VALID_NAME"));
    assert!(!stdout.contains("PWD=/tmp/stale"));
    assert!(!stdout.contains("NEXTEST_BIN_EXE_some-test-binary"));
    assert!(!stdout.contains("BAD-NAME"));

    Ok(())
}

#[test]
fn bash_snapshot_preserves_multiline_exports() -> Result<()> {
    let multiline_cert = "-----BEGIN CERTIFICATE-----\nabc\n-----END CERTIFICATE-----";
    let output = Command::new("/bin/bash")
        .arg("-c")
        .arg(bash_snapshot_script())
        .env("BASH_ENV", "/dev/null")
        .env("MULTILINE_CERT", multiline_cert)
        .output()?;

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("MULTILINE_CERT=") || stdout.contains("MULTILINE_CERT"),
        "snapshot should include the multiline export name"
    );

    let dir = tempdir()?;
    let snapshot_path = dir.path().join("snapshot.sh");
    std::fs::write(&snapshot_path, stdout.as_bytes())?;

    let validate = Command::new("/bin/bash")
        .arg("-c")
        .arg("set -e; . \"$1\"")
        .arg("bash")
        .arg(&snapshot_path)
        .env("BASH_ENV", "/dev/null")
        .output()?;

    assert!(
        validate.status.success(),
        "snapshot validation failed: {}",
        String::from_utf8_lossy(&validate.stderr)
    );

    Ok(())
}

#[tokio::test]
async fn try_new_creates_and_deletes_snapshot_file() -> Result<()> {
    let dir = tempdir()?;
    let shell = Shell {
        shell_type: ShellType::Bash,
        shell_path: PathBuf::from("/bin/bash"),
        shell_snapshot: crate::shell::empty_shell_snapshot_receiver(),
    };

    let snapshot = ShellSnapshot::try_new(dir.path(), ProcessId::new(), dir.path(), &shell)
        .await
        .expect("snapshot should be created");
    let path = snapshot.path.clone();
    assert!(path.exists());
    assert_eq!(snapshot.cwd, dir.path().to_path_buf());
    let snapshot_dir = dir.path().join(SNAPSHOT_DIR);
    let mut entries = fs::read_dir(&snapshot_dir).await?;
    let mut paths = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        paths.push(entry.path());
    }
    assert_eq!(
        paths,
        vec![path.clone()],
        "successful finalization must not leave a temporary snapshot behind"
    );

    drop(snapshot);

    assert!(!path.exists());

    Ok(())
}

#[tokio::test]
async fn refreshed_snapshot_survives_old_generation_drop() -> Result<()> {
    let dir = tempdir()?;
    let session_id = ProcessId::new();
    let shell = Shell {
        shell_type: ShellType::Bash,
        shell_path: PathBuf::from("/bin/bash"),
        shell_snapshot: crate::shell::empty_shell_snapshot_receiver(),
    };

    let first = ShellSnapshot::try_new(dir.path(), session_id, dir.path(), &shell)
        .await
        .expect("first snapshot should be created");
    let second = ShellSnapshot::try_new(dir.path(), session_id, dir.path(), &shell)
        .await
        .expect("second snapshot should be created");

    assert_ne!(first.path, second.path);
    assert!(first.path.exists());
    assert!(second.path.exists());

    let first_path = first.path.clone();
    let second_path = second.path.clone();
    drop(first);

    assert!(!first_path.exists());
    assert!(
        second_path.exists(),
        "dropping the old generation must not delete the current snapshot"
    );

    drop(second);
    assert!(!second_path.exists());

    Ok(())
}

#[tokio::test]
async fn snapshot_actor_replaces_generations_without_cross_deletion() -> Result<()> {
    let dir = tempdir()?;
    let first_cwd = dir.path().join("first");
    let second_cwd = dir.path().join("second");
    fs::create_dir_all(&first_cwd).await?;
    fs::create_dir_all(&second_cwd).await?;
    let mut shell = Shell {
        shell_type: ShellType::Bash,
        shell_path: PathBuf::from("/bin/bash"),
        shell_snapshot: crate::shell::empty_shell_snapshot_receiver(),
    };
    let actor = ShellSnapshotActor::spawn_inner(
        dir.path().to_path_buf(),
        ProcessId::new(),
        ShellSnapshotStartup::Idle,
        &mut shell,
        None,
    );

    actor
        .refresh_and_wait(first_cwd.clone())
        .await
        .expect("first refresh");
    let first = shell.shell_snapshot().expect("first snapshot published");
    let first_path = first.path.clone();
    assert_eq!(first.cwd, first_cwd);
    assert!(first_path.exists());

    actor
        .refresh_and_wait(second_cwd.clone())
        .await
        .expect("second refresh");
    let second = shell.shell_snapshot().expect("second snapshot published");
    let second_path = second.path.clone();
    assert_eq!(second.cwd, second_cwd);
    assert_ne!(first_path, second_path);
    assert!(
        first_path.exists(),
        "the retained old handle still owns its file"
    );
    assert!(second_path.exists());

    drop(first);
    assert!(!first_path.exists());
    assert!(
        second_path.exists(),
        "releasing a previous generation must not delete the current snapshot"
    );

    Ok(())
}

#[tokio::test]
async fn snapshot_actor_keeps_last_valid_snapshot_after_refresh_failure() -> Result<()> {
    let dir = tempdir()?;
    let valid_cwd = dir.path().join("valid");
    fs::create_dir_all(&valid_cwd).await?;
    let mut shell = Shell {
        shell_type: ShellType::Bash,
        shell_path: PathBuf::from("/bin/bash"),
        shell_snapshot: crate::shell::empty_shell_snapshot_receiver(),
    };
    let actor = ShellSnapshotActor::spawn_inner(
        dir.path().to_path_buf(),
        ProcessId::new(),
        ShellSnapshotStartup::Idle,
        &mut shell,
        None,
    );

    actor
        .refresh_and_wait(valid_cwd.clone())
        .await
        .expect("valid refresh");
    let valid = shell.shell_snapshot().expect("valid snapshot published");
    let valid_path = valid.path.clone();

    actor
        .refresh_and_wait(dir.path().join("does-not-exist"))
        .await
        .expect("failed generation is still acknowledged");
    let current = shell
        .shell_snapshot()
        .expect("the last valid snapshot must remain published");
    assert_eq!(current.path, valid_path);
    assert_eq!(current.cwd, valid_cwd);
    assert!(valid_path.exists());

    Ok(())
}

#[tokio::test]
async fn snapshot_shell_does_not_inherit_stdin() -> Result<()> {
    let _stdin_guard = BlockingStdinPipe::install()?;

    let dir = tempdir()?;
    let home = dir.path();
    let read_status_path = home.join("stdin-read-status");
    let read_status_display = read_status_path.display();
    // Persist the startup `read` exit status so the test can assert whether
    // bash saw EOF on stdin after the snapshot process exits.
    let bashrc = format!("read -t 1 -r ignored\nprintf '%s' \"$?\" > \"{read_status_display}\"\n");
    fs::write(home.join(".bashrc"), bashrc).await?;

    let shell = Shell {
        shell_type: ShellType::Bash,
        shell_path: PathBuf::from("/bin/bash"),
        shell_snapshot: crate::shell::empty_shell_snapshot_receiver(),
    };

    let home_display = home.display();
    let script = format!(
        "HOME=\"{home_display}\"; export HOME; {}",
        bash_snapshot_script()
    );
    let output = run_script_with_timeout(&shell, &script, Duration::from_secs(2), true, home)
        .await
        .context("run snapshot command")?;
    let read_status = fs::read_to_string(&read_status_path)
        .await
        .context("read stdin probe status")?;

    assert_eq!(
        read_status, "1",
        "expected shell startup read to see EOF on stdin; status={read_status:?}"
    );

    assert!(
        output.contains("# Snapshot file"),
        "expected snapshot marker in output; output={output:?}"
    );

    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn timed_out_snapshot_shell_is_terminated() -> Result<()> {
    use std::process::Stdio;
    use tokio::time::Duration as TokioDuration;
    use tokio::time::Instant;
    use tokio::time::sleep;

    let dir = tempdir()?;
    let pid_path = dir.path().join("pid");
    let script = format!("echo $$ > \"{}\"; sleep 30", pid_path.display());

    let shell = Shell {
        shell_type: ShellType::Sh,
        shell_path: PathBuf::from("/bin/sh"),
        shell_snapshot: crate::shell::empty_shell_snapshot_receiver(),
    };

    let err = run_script_with_timeout(&shell, &script, Duration::from_secs(1), true, dir.path())
        .await
        .expect_err("snapshot shell should time out");
    assert!(
        err.to_string().contains("timed out"),
        "expected timeout error, got {err:?}"
    );

    let pid = fs::read_to_string(&pid_path)
        .await
        .expect("snapshot shell writes its pid before timing out")
        .trim()
        .parse::<i32>()?;

    let deadline = Instant::now() + TokioDuration::from_secs(1);
    loop {
        let kill_status = StdCommand::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .status()?;
        if !kill_status.success() {
            break;
        }
        if Instant::now() >= deadline {
            panic!("timed out snapshot shell is still alive after grace period");
        }
        sleep(TokioDuration::from_millis(50)).await;
    }

    Ok(())
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn macos_zsh_snapshot_includes_sections() -> Result<()> {
    let snapshot = get_snapshot(ShellType::Zsh).await?;
    assert_posix_snapshot_sections(&snapshot);
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn linux_bash_snapshot_includes_sections() -> Result<()> {
    let snapshot = get_snapshot(ShellType::Bash).await?;
    assert_posix_snapshot_sections(&snapshot);
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn linux_sh_snapshot_includes_sections() -> Result<()> {
    let snapshot = get_snapshot(ShellType::Sh).await?;
    assert_posix_snapshot_sections(&snapshot);
    Ok(())
}

async fn create_journal_process_stub(
    chaos_home: &Path,
    session_id: ProcessId,
    age: Duration,
) -> Result<()> {
    let store = SqliteJournalStore::open(&chaos_proc::runtime_db_path(chaos_home)).await?;
    let age_secs = i64::try_from(age.as_secs())?;
    let created_at = jiff::Timestamp::from_second(jiff::Timestamp::now().as_second() - age_secs)?;
    store
        .create_process(CreateProcessInput {
            process_id: session_id,
            parent: None,
            source: SessionSource::Exec,
            cwd: chaos_home.to_path_buf(),
            created_at,
            title: Some("shell snapshot test".to_string()),
            model_provider: Some("test".to_string()),
            cli_version: None,
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn cleanup_stale_snapshots_removes_orphans_and_keeps_live() -> Result<()> {
    let dir = tempdir()?;
    let chaos_home = dir.path();
    let snapshot_dir = chaos_home.join(SNAPSHOT_DIR);
    fs::create_dir_all(&snapshot_dir).await?;

    let live_session = ProcessId::new();
    let orphan_session = ProcessId::new();
    let live_snapshot = snapshot_dir.join(format!("{live_session}.123.sh"));
    let orphan_snapshot = snapshot_dir.join(format!("{orphan_session}.123.sh"));
    let invalid_snapshot = snapshot_dir.join("not-a-snapshot.txt");

    create_journal_process_stub(chaos_home, live_session, Duration::ZERO).await?;
    fs::write(&live_snapshot, "live").await?;
    fs::write(&orphan_snapshot, "orphan").await?;
    fs::write(&invalid_snapshot, "invalid").await?;

    cleanup_stale_snapshots(chaos_home, ProcessId::new()).await?;

    assert_eq!(live_snapshot.exists(), true);
    assert_eq!(orphan_snapshot.exists(), false);
    assert_eq!(invalid_snapshot.exists(), false);
    Ok(())
}

#[tokio::test]
async fn cleanup_stale_snapshots_removes_stale_sessions() -> Result<()> {
    let dir = tempdir()?;
    let chaos_home = dir.path();
    let snapshot_dir = chaos_home.join(SNAPSHOT_DIR);
    fs::create_dir_all(&snapshot_dir).await?;

    let stale_session = ProcessId::new();
    let stale_snapshot = snapshot_dir.join(format!("{stale_session}.123.sh"));
    create_journal_process_stub(
        chaos_home,
        stale_session,
        SNAPSHOT_RETENTION + Duration::from_secs(60),
    )
    .await?;
    fs::write(&stale_snapshot, "stale").await?;

    cleanup_stale_snapshots(chaos_home, ProcessId::new()).await?;

    assert_eq!(stale_snapshot.exists(), false);
    Ok(())
}

#[tokio::test]
async fn cleanup_stale_snapshots_skips_active_session() -> Result<()> {
    let dir = tempdir()?;
    let chaos_home = dir.path();
    let snapshot_dir = chaos_home.join(SNAPSHOT_DIR);
    fs::create_dir_all(&snapshot_dir).await?;

    let active_session = ProcessId::new();
    let active_snapshot = snapshot_dir.join(format!("{active_session}.123.sh"));
    fs::write(&active_snapshot, "active").await?;

    cleanup_stale_snapshots(chaos_home, active_session).await?;

    assert_eq!(active_snapshot.exists(), true);
    Ok(())
}
