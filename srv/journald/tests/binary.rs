//! Drive the real `chaos_journald` executable, not the in-process server.
//!
//! Being an integration test of this package also makes every workspace test
//! build produce `<target>/<profile>/chaos_journald`, which other packages'
//! tests (and the `chaos` binaries they spawn) resolve beside themselves.

use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

use chaos_journald::JournalRpcClient;
use chaos_journald::hello_is_compatible;

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Err(error) = self.0.kill().and_then(|()| self.0.wait().map(drop)) {
            eprintln!("stop chaos_journald: {error}");
        }
    }
}

fn spawn(socket: &Path, db: &Path) -> std::io::Result<Daemon> {
    Command::new(env!("CARGO_BIN_EXE_chaos_journald"))
        .arg("--socket")
        .arg(socket)
        .arg("--db")
        .arg(db)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map(Daemon)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_serves_a_compatible_sqlite_journal() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let socket = home.path().join("run/journald.sock");
    let mut daemon = spawn(&socket, &home.path().join("chaos.sqlite"))?;
    let client = JournalRpcClient::new(socket);

    let deadline = Instant::now() + Duration::from_secs(30);
    let hello = loop {
        match client.hello("journald-binary-test").await {
            Ok(hello) => break hello,
            Err(error) => {
                anyhow::ensure!(
                    daemon.0.try_wait()?.is_none(),
                    "chaos_journald exited before serving: {error}"
                );
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "chaos_journald never answered hello: {error}"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    };
    assert!(hello_is_compatible(&hello), "{hello:?}");
    assert_eq!(hello.backend, "sqlite");
    Ok(())
}
