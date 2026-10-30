//! Native CLI ownership survives cancellation of the future driving its pipes.
use std::io;
use std::time::Duration;

use tokio::process::{Child, Command};

#[derive(Debug)]
pub(crate) struct NativeChild {
    child: Child,
    #[cfg(unix)]
    group: Option<i32>,
}

impl NativeChild {
    pub(crate) fn spawn(command: &mut Command) -> io::Result<Self> {
        command.kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let child = command.spawn()?;
        Ok(Self {
            #[cfg(unix)]
            group: child.id().map(|id| id as i32),
            child,
        })
    }

    pub(crate) fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    pub(crate) fn id(&self) -> Option<u32> {
        self.child.id()
    }

    pub(crate) async fn terminate(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        self.kill_group()?;
        #[cfg(not(unix))]
        self.child.start_kill()?;
        tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "native child did not exit"))??;
        #[cfg(unix)]
        if let Some(group) = self.group {
            tokio::time::timeout(Duration::from_secs(5), async {
                while group_exists(group)? {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Ok::<_, io::Error>(())
            })
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "native process group remains")
            })??;
            self.group = None;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn kill_group(&self) -> io::Result<()> {
        if let Some(group) = self.group {
            // SAFETY: spawn establishes a separate group with this child's PID.
            // Never resolve the parent's process group after the child exits.
            if unsafe { libc::killpg(group, libc::SIGKILL) } == -1 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn group_exists(group: i32) -> io::Result<bool> {
    // SAFETY: signal zero is only an existence/permission probe.
    if unsafe { libc::killpg(group, 0) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error),
    }
}

impl Drop for NativeChild {
    fn drop(&mut self) {
        // Drop is best effort only; callers must await terminate for proof.
        #[cfg(unix)]
        let _ = self.kill_group();
    }
}

#[cfg(test)]
mod tests;
