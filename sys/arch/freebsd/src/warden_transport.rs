//! One request/reply capability per command. The private bootstrap endpoint
//! selects a launcher-authenticated identity, never a UID from wire data.
//! Control sockets are never passed into the command's descriptor table.

use crate::warden::{CommandRequest, CommandSession};
use crate::warden_channel::{Channel, MAX_PACKET};
use crate::warden_process::CommandIdentity;
use serde::{Deserialize, Serialize};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::time::{Duration, Instant};

const VERSION: u32 = 1;
const CLEANUP_WAIT: Duration = Duration::from_secs(5);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u32,
    command: CommandRequest,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Error {
        message: String,
    },
}

impl Response {
    fn from_error(error: &io::Error) -> Self {
        Self::Error {
            message: error.to_string().chars().take(512).collect(),
        }
    }

    fn send(&self, channel: &Channel) -> io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        match channel.send(&bytes, &[]) {
            Err(error) if matches!(error.raw_os_error(), Some(libc::EPIPE | libc::ECONNRESET)) => {
                Ok(())
            }
            result => result,
        }
    }
}

pub struct PendingCommand {
    reply: Channel,
}

impl PendingCommand {
    /// Closing this command-specific channel also requests cancellation. It
    /// does not close or serialize replies on the shared dispatcher channel.
    pub fn cancel(self) -> io::Result<()> {
        self.reply.send(b"cancel", &[])
    }

    pub fn wait(self) -> io::Result<Response> {
        let packet = self.reply.receive()?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Warden exited before command completion",
            )
        })?;
        if !packet.descriptors.is_empty() {
            return Err(invalid("authority is not allowed in Warden replies"));
        }
        let response: Response = serde_json::from_slice(&packet.bytes).map_err(io::Error::other)?;
        if let Response::Exited { code, signal } = &response
            && !matches!(
                (code, signal),
                (Some(0..=255), None) | (None, Some(1..=128))
            )
        {
            return Err(invalid("invalid Warden exit status"));
        }
        Ok(response)
    }
}

pub fn submit(
    bootstrap: &Channel,
    command: CommandRequest,
    stdio: [BorrowedFd<'_>; 3],
) -> io::Result<PendingCommand> {
    bootstrap.require_root_peer()?;
    command.validate()?;
    validate_stdio(&stdio)?;
    let bytes = serde_json::to_vec(&Request {
        version: VERSION,
        command,
    })
    .map_err(io::Error::other)?;
    if bytes.len() > MAX_PACKET {
        return Err(invalid("Warden request exceeds transport limit"));
    }
    let (client, worker) = Channel::pair()?;
    bootstrap.send(&bytes, &[worker.as_fd(), stdio[0], stdio[1], stdio[2]])?;
    Ok(PendingCommand { reply: client })
}

/// Serve one command in a single-threaded root worker. The optional shutdown
/// capability is monitored by poll (e.g. the launcher's process descriptor).
/// Timeout is launcher policy, not a request-supplied privilege or UID.
pub fn serve_one(
    bootstrap: &Channel,
    identity: &CommandIdentity,
    state_parent: &Path,
    shutdown: Option<BorrowedFd<'_>>,
    maximum_runtime: Duration,
) -> io::Result<bool> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(io::Error::from_raw_os_error(libc::EPERM));
    }
    serve_one_with(bootstrap, shutdown, maximum_runtime, |request, stdio| {
        CommandSession::start_with_stdio(request, identity, state_parent, Some(stdio))
    })
}

fn serve_one_with(
    bootstrap: &Channel,
    shutdown: Option<BorrowedFd<'_>>,
    maximum_runtime: Duration,
    start: impl FnOnce(&CommandRequest, [OwnedFd; 3]) -> io::Result<CommandSession>,
) -> io::Result<bool> {
    let Some(packet) = bootstrap.receive()? else {
        return Ok(false);
    };
    let mut rights = packet.descriptors.into_iter();
    // A malformed packet without a usable reply capability terminates this
    // bootstrap, never guesses a response destination or adopts a numeric ID.
    let reply = Channel::from_owned(
        rights
            .next()
            .ok_or_else(|| invalid("missing Warden reply channel"))?,
    )?;
    let stdio: Vec<_> = rights.collect();
    let decoded = (|| {
        let stdio: [OwnedFd; 3] = stdio
            .try_into()
            .map_err(|_| invalid("Warden requires exactly three stdio descriptors"))?;
        validate_stdio(&stdio.each_ref().map(AsFd::as_fd))?;
        let request: Request = serde_json::from_slice(&packet.bytes).map_err(io::Error::other)?;
        if request.version != VERSION {
            return Err(invalid("unsupported Warden protocol version"));
        }
        request.command.validate()?;
        Ok((request.command, stdio))
    })();
    let (request, stdio) = match decoded {
        Ok(value) => value,
        Err(error) => {
            Response::from_error(&error).send(&reply)?;
            return Ok(true);
        }
    };
    if cancelled(&reply, shutdown, 0)? {
        return Ok(true);
    }
    let mut session = match start(&request, stdio) {
        Ok(session) => session,
        Err(error) => {
            Response::from_error(&error).send(&reply)?;
            return Ok(true);
        }
    };
    let result = drive(&mut session, &reply, shutdown, maximum_runtime);
    let response = match result {
        Ok((code, signal)) => Response::Exited { code, signal },
        Err(error) => {
            // A control/poll/protocol failure must still attempt bounded
            // teardown, rather than just dropping a running command.
            if let Err(cleanup) = session.cancel_and_cleanup(CLEANUP_WAIT) {
                eprintln!(
                    "Warden: cleanup retained {}: {cleanup}",
                    session.state_path().display()
                );
            }
            Response::from_error(&error)
        }
    };
    // An abandoned client cannot turn completed teardown into a worker panic.
    match response.send(&reply) {
        Ok(()) => Ok(true),
        Err(error) if matches!(error.raw_os_error(), Some(libc::EPIPE | libc::ECONNRESET)) => {
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

fn drive(
    session: &mut CommandSession,
    reply: &Channel,
    shutdown: Option<BorrowedFd<'_>>,
    maximum_runtime: Duration,
) -> io::Result<(Option<i32>, Option<i32>)> {
    let deadline = Instant::now()
        .checked_add(maximum_runtime)
        .ok_or_else(|| invalid("runtime limit overflow"))?;
    loop {
        if session.poll_exit()?.is_some() {
            let status = session.finish_cleanup(CLEANUP_WAIT)?;
            return Ok((status.code(), status.signal()));
        }
        if Instant::now() >= deadline || cancelled(reply, shutdown, 20)? {
            let status = session.cancel_and_cleanup(CLEANUP_WAIT)?;
            return Ok((status.code(), status.signal()));
        }
    }
}

fn cancelled(reply: &Channel, shutdown: Option<BorrowedFd<'_>>, timeout: i32) -> io::Result<bool> {
    let mut fds = [
        libc::pollfd {
            fd: reply.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: shutdown.map_or(-1, |fd| fd.as_raw_fd()),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout) } < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(error);
    }
    // No ancillary FDs or messages are accepted on the cancellation path.
    // Any readability, shutdown, error or invalid FD means remove the jail.
    Ok(fds.iter().any(|fd| fd.revents != 0))
}

fn validate_stdio(fds: &[BorrowedFd<'_>; 3]) -> io::Result<()> {
    for fd in fds {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { stat.assume_init() }.st_mode & libc::S_IFMT == libc::S_IFDIR {
            return Err(invalid("directory stdio cannot enter a jail"));
        }
        if unsafe { libc::isatty(fd.as_raw_fd()) } != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Warden transport currently supports pipes, not PTY sessions",
            ));
        }
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
